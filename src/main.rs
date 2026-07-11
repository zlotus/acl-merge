// acl-merge v2
//
// 从上游订阅(acl-url)只抽取节点(proxies),套用【内置的 vasma 原版模板】,
// 注入 gist 规则补丁,把 provider 模式转成 inline proxies,输出无域名/无 phone-home 的干净 config。
//
// 模板已焊死在本程序里(template.yaml),以后换不换 vasma 都不影响输出格式。

use anyhow::{anyhow, Context, Result};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Router,
};
use clap::Parser;
use serde::Deserialize;
use serde_yaml::{Sequence, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// 内置的 vasma 原版 clashMeta 模板(编译期嵌入)
const BUILTIN_TEMPLATE: &str = include_str!("template.yaml");

#[derive(Parser, Debug, Clone)]
#[command(name = "acl-merge", version, about = "合并上游节点 + 内置模板 + gist 规则,输出干净的 clash 配置")]
struct Args {
    /// 监听地址,例如 127.0.0.1:8080 (建议绑本地,配合 CF Tunnel / Tailscale)
    #[arg(long, default_value = "127.0.0.1:8080")]
    listen: String,

    /// 上游订阅 URL(只取其中的 proxies 节点)。例如 vasma 本机订阅 http://127.0.0.1:40353/s/clashMeta/<id>
    #[arg(long)]
    acl_url: String,

    /// gist 规则补丁 URL(prepend/append/delete 结构)
    #[arg(long)]
    gist_url: String,

    /// 访问 token
    #[arg(long)]
    secret: String,

    /// 缓存秒数,低配 VPS 建议 60~300,避免每次请求都去拉上游
    #[arg(long, default_value = "120")]
    cache_secs: u64,

    /// 需要净化的域名/字符串(逗号分隔),例如你的旧实名域名。命中的节点整条丢弃;若成品里仍残留则拒绝下发。
    #[arg(long, default_value = "")]
    scrub: String,
}

/// gist 规则补丁结构(与你现有 gist 格式一致)
#[derive(Debug, Deserialize, Default)]
struct RulePatch {
    #[serde(default)]
    prepend: Vec<String>,
    #[serde(default)]
    append: Vec<String>,
    #[serde(default)]
    delete: Vec<String>,
}

struct AppState {
    args: Args,
    http: reqwest::Client,
    scrub: Vec<String>,
    cache: Mutex<Option<(Instant, String)>>,
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let args = Args::parse();

    let scrub: Vec<String> = args
        .scrub
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;

    let listen = args.listen.clone();
    let state = Arc::new(AppState {
        args,
        http,
        scrub,
        cache: Mutex::new(None),
    });

    let app = Router::new()
        .route("/p", get(serve_profile))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("无法绑定监听地址 {listen}"))?;
    tracing::info!("acl-merge v2 启动");
    tracing::info!("  监听地址: http://{listen}/p?token=***");
    tracing::info!("  节点源(acl-url): {}", state.args.acl_url);
    tracing::info!("  规则源(gist-url): {}", state.args.gist_url);
    tracing::info!("  scrub 关键词: {:?}", state.scrub);
    tracing::info!("  缓存: {}s", state.args.cache_secs);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn serve_profile(
    State(st): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
) -> impl IntoResponse {
    // token 校验
    if q.token.as_deref() != Some(st.args.secret.as_str()) {
        return (StatusCode::FORBIDDEN, "invalid token").into_response();
    }

    // 缓存命中
    {
        let cache = st.cache.lock().await;
        if let Some((t, body)) = cache.as_ref() {
            if t.elapsed() < Duration::from_secs(st.args.cache_secs) {
                tracing::info!("命中缓存({}s 内),直接返回 {} 字节", t.elapsed().as_secs(), body.len());
                return yaml_ok(body.clone());
            }
        }
    }

    match build_config(&st).await {
        Ok(body) => {
            let mut cache = st.cache.lock().await;
            *cache = Some((Instant::now(), body.clone()));
            yaml_ok(body)
        }
        Err(e) => {
            tracing::error!("生成失败: {e:#}");
            (StatusCode::BAD_GATEWAY, format!("build error: {e:#}")).into_response()
        }
    }
}

fn yaml_ok(body: String) -> axum::response::Response {
    (
        StatusCode::OK,
        [("content-type", "text/yaml; charset=utf-8")],
        body,
    )
        .into_response()
}

/// 核心:拉上游节点 + 拉 gist + 内置模板 → 组装 → 净化
async fn build_config(st: &AppState) -> Result<String> {
    // 1. 上游订阅:只抽 proxies
    let acl_text = fetch_text(&st.http, &st.args.acl_url)
        .await
        .context("拉取 acl-url 失败")?;
    let mut proxies = extract_proxies(&acl_text).context("从上游解析 proxies 失败")?;
    let proxies_upstream = proxies.len();

    // 1b. 净化:整个节点命中 scrub 关键词就整条丢弃(不能只删一行,那会做出缺 server 的残废节点)
    if !st.scrub.is_empty() {
        proxies.retain(|p| {
            let text = serde_yaml::to_string(p).unwrap_or_default();
            match st.scrub.iter().find(|kw| text.contains(kw.as_str())) {
                Some(kw) => {
                    tracing::warn!("丢弃命中 scrub 关键词 `{kw}` 的节点: {:?}", proxy_name(p));
                    false
                }
                None => true,
            }
        });
    }
    let proxies_scrubbed = proxies_upstream - proxies.len();

    if proxies.is_empty() {
        return Err(anyhow!("上游订阅里没有解析到任何节点(proxies 为空,或全被 scrub 丢弃)"));
    }

    // 2. gist 规则补丁
    let gist_text = fetch_text(&st.http, &st.args.gist_url)
        .await
        .context("拉取 gist-url 失败")?;
    let patch: RulePatch = serde_yaml::from_str(&gist_text).context("解析 gist 规则补丁失败")?;

    // 3. 内置模板
    let mut root: Value = serde_yaml::from_str(BUILTIN_TEMPLATE).context("内置模板解析失败")?;

    // 4. 组装
    let proxies_final = proxies.len();
    let proxy_names = collect_proxy_names(&proxies);
    remove_proxy_providers(&mut root);
    rewrite_groups_use_to_proxies(&mut root, &proxy_names);
    inject_proxies(&mut root, proxies);
    let stats = apply_rule_patch(&mut root, &patch)?;

    // 4b. 净化 rules:一条规则是一个字符串标量,整条丢弃是安全的(不像节点里删一行会做出残废节点)。
    //     典型来源:gist 里还留着 `DOMAIN,xxx.你的域名,DIRECT` 这种旧规则。
    let mut rules_scrubbed = 0usize;
    if !st.scrub.is_empty() {
        if let Some(Value::Sequence(rules)) = root.get_mut("rules") {
            let before = rules.len();
            rules.retain(|r| {
                let s = r.as_str().unwrap_or("");
                match st.scrub.iter().find(|kw| s.contains(kw.as_str())) {
                    Some(kw) => {
                        tracing::warn!("丢弃命中 scrub 关键词 `{kw}` 的规则: {s}");
                        false
                    }
                    None => true,
                }
            });
            rules_scrubbed = before - rules.len();
        }
    }

    let rules_final = root
        .get("rules")
        .and_then(|r| r.as_sequence())
        .map_or(0, |s| s.len());
    tracing::info!(
        "构建完成: 节点 {proxies_final}/{proxies_upstream}(scrub 丢弃 {proxies_scrubbed}), \
         规则 原始={} 最终={rules_final} (prepend={} append={} delete={} scrub={rules_scrubbed})",
        stats.original, stats.prepend, stats.append, stats.deleted
    );

    // 5. 序列化
    let out = serde_yaml::to_string(&root)?;

    // 6. 兜底断言:成品里绝不能再出现 scrub 关键词。命中就拒发(而不是偷偷删行发出坏配置)。
    if let Some(kw) = st.scrub.iter().find(|kw| out.contains(kw.as_str())) {
        return Err(anyhow!(
            "输出里仍残留 scrub 关键词 `{kw}`(可能在模板或 gist 规则里),拒绝下发"
        ));
    }

    Ok(out)
}

fn proxy_name(p: &Value) -> Option<&str> {
    p.as_mapping()?
        .get(Value::String("name".into()))?
        .as_str()
}

async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String> {
    // 本地文件:非 http(s) 开头的当路径读(节点源可以是磁盘上的 nodes.yaml,不必再起 nginx)
    if !url.starts_with("http://") && !url.starts_with("https://") {
        let path = url.strip_prefix("file://").unwrap_or(url);
        return std::fs::read_to_string(path)
            .with_context(|| format!("读取本地文件失败: {path}"));
    }
    let resp = client.get(url).send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(anyhow!("HTTP {status}: {}", text.chars().take(200).collect::<String>()));
    }
    Ok(text)
}

/// 从上游 clash yaml 里抽出 proxies 列表。
/// 兼容两种上游:(a) 完整 config 含 proxies:  (b) proxy-provider 风格,顶层就是 proxies: [...]
fn extract_proxies(text: &str) -> Result<Sequence> {
    let val: Value = serde_yaml::from_str(text).context("上游不是合法 YAML")?;
    if let Value::Mapping(map) = &val {
        if let Some(Value::Sequence(seq)) = map.get(Value::String("proxies".into())) {
            return Ok(seq.clone());
        }
    }
    // 有些 provider 文件顶层直接是一个 sequence
    if let Value::Sequence(seq) = &val {
        return Ok(seq.clone());
    }
    Err(anyhow!("上游里找不到 proxies 字段"))
}

fn collect_proxy_names(proxies: &Sequence) -> Vec<String> {
    proxies
        .iter()
        .filter_map(|p| {
            p.as_mapping()
                .and_then(|m| m.get(Value::String("name".into())))
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
        })
        .collect()
}

fn remove_proxy_providers(root: &mut Value) {
    if let Value::Mapping(map) = root {
        map.remove(Value::String("proxy-providers".into()));
    }
}

/// 把每个 proxy-group 里的 `use: [xxx_provider]` 展开成 inline `proxies:`。
///
/// 顺序照抄 mihomo 原生语义:模板里写死的 `proxies:`(DIRECT / 手动切换 …)排前面,
/// provider 供给的节点接在后面。别把节点插到最前面 —— select 组默认选中列表第一项,
/// 那样会让 `本地直连` / `国内媒体` 的默认值从 DIRECT 翻成走代理。
fn rewrite_groups_use_to_proxies(root: &mut Value, proxy_names: &[String]) {
    let Value::Mapping(map) = root else { return };
    let Some(Value::Sequence(groups)) = map.get_mut(Value::String("proxy-groups".into())) else {
        return;
    };

    for g in groups.iter_mut() {
        let Value::Mapping(gm) = g else { continue };

        // 只有原本靠 provider 供给节点的组(带 use 字段)才需要塞入真实节点
        if !gm.contains_key(Value::String("use".into())) {
            continue;
        }
        gm.remove(Value::String("use".into()));

        // 模板写死的组引用。`手动切换` / `自动选择` 写的是 `proxies: null`,那个 null 必须滤掉,
        // 否则组里会多出一个空项,mihomo 拒配置。
        let mut final_list: Vec<Value> = match gm.get(Value::String("proxies".into())) {
            Some(Value::Sequence(existing)) => {
                existing.iter().filter(|v| !v.is_null()).cloned().collect()
            }
            _ => Vec::new(),
        };
        final_list.extend(proxy_names.iter().map(|n| Value::String(n.clone())));

        gm.insert(Value::String("proxies".into()), Value::Sequence(final_list));
    }
}

fn inject_proxies(root: &mut Value, proxies: Sequence) {
    if let Value::Mapping(map) = root {
        map.insert(
            Value::String("proxies".into()),
            Value::Sequence(proxies),
        );
    }
}

/// 规则处理统计(用于日志)
#[derive(Debug, Default)]
struct RuleStats {
    original: usize,
    prepend: usize,
    append: usize,
    deleted: usize,
}

/// 应用 gist 规则补丁:prepend 到 rules 顶部、append 到尾部、delete 删除匹配项
fn apply_rule_patch(root: &mut Value, patch: &RulePatch) -> Result<RuleStats> {
    let Value::Mapping(map) = root else {
        return Err(anyhow!("模板根不是 mapping"));
    };
    let rules_val = map
        .entry(Value::String("rules".into()))
        .or_insert(Value::Sequence(Sequence::new()));
    let Value::Sequence(rules) = rules_val else {
        return Err(anyhow!("rules 不是 sequence"));
    };

    let original = rules.len();

    // delete:删除任何等于 delete 项、或包含该子串的规则
    if !patch.delete.is_empty() {
        rules.retain(|r| {
            let s = r.as_str().unwrap_or("");
            !patch.delete.iter().any(|d| s == d || s.contains(d.as_str()))
        });
    }
    let deleted = original - rules.len();

    // prepend:插到最前面(保持顺序)
    if !patch.prepend.is_empty() {
        let mut new_rules: Sequence = patch
            .prepend
            .iter()
            .map(|s| Value::String(s.clone()))
            .collect();
        new_rules.extend(rules.drain(..));
        *rules = new_rules;
    }

    // append:插在 MATCH 之前。clash 规则首次命中即停,MATCH 是通配兜底,
    // 排在它后面的规则永远走不到。
    if !patch.append.is_empty() {
        let at = rules
            .iter()
            .position(|r| r.as_str().is_some_and(|s| s.starts_with("MATCH")))
            .unwrap_or(rules.len());
        for (i, a) in patch.append.iter().enumerate() {
            rules.insert(at + i, Value::String(a.clone()));
        }
    }

    Ok(RuleStats {
        original,
        prepend: patch.prepend.len(),
        append: patch.append.len(),
        deleted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内置模板经过完整组装后,必须满足红线:无 provider、无 use、append 在 MATCH 之前。
    #[test]
    fn pipeline_on_builtin_template() {
        let upstream = r#"
proxies:
  - {name: 好节点, type: vless, server: 1.2.3.4, port: 443}
  - {name: 坏节点, type: vless, server: bad.example.com, port: 443}
"#;
        let scrub = ["example.com".to_string()];

        let mut proxies = extract_proxies(upstream).unwrap();
        proxies.retain(|p| {
            let t = serde_yaml::to_string(p).unwrap();
            !scrub.iter().any(|kw| t.contains(kw.as_str()))
        });
        // 命中 scrub 的节点被整条丢弃,而不是删掉 server 那一行留个残废节点
        assert_eq!(collect_proxy_names(&proxies), vec!["好节点"]);

        let patch = RulePatch {
            prepend: vec!["DOMAIN-SUFFIX,claude.ai,ClaudeAI".into()],
            append: vec!["GEOIP,CN,DIRECT".into()],
            delete: vec![],
        };

        let mut root: Value = serde_yaml::from_str(BUILTIN_TEMPLATE).unwrap();
        let names = collect_proxy_names(&proxies);
        remove_proxy_providers(&mut root);
        rewrite_groups_use_to_proxies(&mut root, &names);
        inject_proxies(&mut root, proxies);
        apply_rule_patch(&mut root, &patch).unwrap();
        let out = serde_yaml::to_string(&root).unwrap();

        assert!(!out.contains("proxy-providers"), "残留 proxy-providers");
        assert!(!out.contains("example.com"), "残留旧域名");
        assert!(!out.contains("${"), "残留模板占位符");

        let map = root.as_mapping().unwrap();
        let group = |name: &str| -> Vec<String> {
            map["proxy-groups"]
                .as_sequence()
                .unwrap()
                .iter()
                .find(|g| g["name"].as_str() == Some(name))
                .unwrap_or_else(|| panic!("模板里没有 group {name}"))["proxies"]
                .as_sequence()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        };
        for g in map["proxy-groups"].as_sequence().unwrap() {
            let g = g.as_mapping().unwrap();
            assert!(!g.contains_key("use"), "group 还带 use: {:?}", g["name"]);
            let p = g["proxies"].as_sequence().unwrap();
            assert!(!p.is_empty(), "空 group: {:?}", g["name"]);
            assert!(!p.iter().any(|v| v.is_null()), "group 里有 null 项: {:?}", g["name"]);
        }
        // select 组默认选中第一项:这两个组的默认值必须仍是 DIRECT,节点只能排在后面
        assert_eq!(group("本地直连")[0], "DIRECT", "本地直连 默认值被节点顶掉了");
        assert_eq!(group("国内媒体")[0], "DIRECT", "国内媒体 默认值被节点顶掉了");
        assert_eq!(group("手动切换"), vec!["好节点"], "手动切换 应该只有节点");

        let rules: Vec<&str> = map["rules"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap())
            .collect();
        assert_eq!(rules[0], "DOMAIN-SUFFIX,claude.ai,ClaudeAI", "prepend 不在顶部");
        let m = rules.iter().position(|r| r.starts_with("MATCH")).expect("模板没有 MATCH");
        let a = rules.iter().position(|r| *r == "GEOIP,CN,DIRECT").expect("append 丢了");
        assert!(a < m, "append 排在 MATCH 之后 = 死规则");
        assert_eq!(m, rules.len() - 1, "MATCH 必须是最后一条");
    }
}
