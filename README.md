# acl-merge v2

在一台干净 VPS 上部署一个**无域名、无 phone-home、自包含**的 VLESS-Reality 翻墙节点,
并把订阅做成干净的 clash 配置对外分发。

一句话:**上游给节点 + 编译期内置的 clash 模板给策略 + gist 给规则 → 一份干净 config**。
模板用 `include_str!("template.yaml")` 焊死在二进制里,与上游(mack-a/v2ray-agent)脱钩,
上游怎么更新都不影响你的输出。

## 架构

```
 xray (443, 裸 Reality)           ← 节点本体,单 inbound,无 nginx/无证书/无 dokodemo
        │  部署脚本生成
        ▼
 /opt/xray/nodes.yaml (本地文件)   ← 客户端节点(clash 格式)
        │  acl-merge 读它
        ▼
 acl-merge (127.0.0.1:8080)       ← 套内置模板 + gist 规则 + 净化旧域名
        │  cloudflared 走 localhost
        ▼
 CF Tunnel → https://sub.example.com/p?token=xxx   ← 橙云隐藏真实 IP + 自动 HTTPS
        │
        ▼
 Clash Verge 订阅
```

比 vasma(mack-a 八合一)少了三层:nginx 订阅服务、dokodemo 端口转发、TLS 证书链。

## 前置

- 一台 x86_64 Linux VPS(Debian/Ubuntu,root)
- (分发用,可选)一个托管在 Cloudflare 的域名 —— 用来做 CF Tunnel,隐藏 VPS 真实 IP
- (规则用,可选)一个 gist,存 `prepend`/`append`/`delete` 结构的规则补丁

---

## 第 1 步:部署 Reality 节点 + acl-merge 订阅服务

一个脚本搞定节点和订阅服务。先下载再运行(方便你先看一眼脚本内容):

```bash
curl -sL https://raw.githubusercontent.com/zlotus/acl-merge/master/deploy-reality.sh -o deploy-reality.sh

# 设了 SECRET 才会连 acl-merge 一起装;不设就只装 Reality 节点
SECRET=<你的访问token> \
GIST_URL=<你的 gist raw url> \
SCRUB=old.example.com \
  bash deploy-reality.sh
```

脚本会自动:
1. 下 xray 到 `/opt/xray/`
2. **现场生成**本机密钥(UUID / Reality 密钥对 / shortId),存 `/opt/xray/creds.env`(600,不外传)
3. 写服务端 `config.json`(单 inbound 裸 Reality,伪装站默认 `www.apple.com`)
4. 写 systemd unit `reality.service` 并 `enable --now`
5. **loopback 自检**:本机实拨节点,确认 Reality 握手能走完 —— 伪装站选错会当场报错退出
6. 生成客户端节点 `/opt/xray/nodes.yaml`
7. (设了 SECRET 时)从 GitHub release 下载 acl-merge 二进制,配成 `acl-merge.service`

可用的环境变量:

| 变量 | 默认 | 说明 |
|---|---|---|
| `PORT` | `443` | Reality 监听端口 |
| `DEST` | `www.apple.com` | **伪装站**(见下方选站铁律,选错国内连不上) |
| `SECRET` | 无 | acl-merge 访问 token;**设了才装 acl-merge** |
| `GIST_URL` | 空 | 规则补丁 raw URL,不设则用空补丁 |
| `SCRUB` | 空 | 要净化的旧域名(逗号分隔) |
| `LISTEN` | `127.0.0.1:8080` | acl-merge 监听地址(默认本地,配合 CF Tunnel) |

脚本结束会打印客户端节点信息和 `vless://` 分享链接。

### ⚠ 伪装站(DEST)选站三铁律

Reality 的 `DEST` 是握手时"借"TLS 的真实网站,选错的表现是**国内客户端一直红色 Error**(VPS 本机测却正常)。三条硬约束:

1. **国内能访问、不被墙** —— Cloudflare 系(`one.one.one.one`)、Google 系会被 GFW 干扰,别用
2. **证书链别太大** —— `www.microsoft.com` 证书 ~9.6KB 会撑爆 xray 转发缓冲,握手完不成
3. **最好和 VPS 同区域** —— 延迟低、GFW 更不敏感

`www.apple.com`(默认)三条都满足,已验证。脚本第 5 步的自检就是把"选错站"挡在部署阶段。

---

## 第 2 步:CF Tunnel 分发(隐藏 IP + HTTPS)

让订阅走 `https://sub.example.com`,不暴露 VPS 真实 IP。

1. **建隧道拿 token**:Cloudflare 后台 → Zero Trust → Networks → Tunnels → Create a tunnel →
   Cloudflared → 起个名 → 复制它给的 `cloudflared service install <TOKEN>` 命令,在 VPS 上执行。
2. **加公共主机名**:该隧道的 Public Hostname 标签页 → Add:
   - Subdomain `sub`,Domain `example.com`,Type `HTTP`,URL `localhost:8080`
3. **配 DNS**(若自动加失败会提示手动):域名 DNS 页 →
   - 删掉 `sub` 现有的 A 记录(如果有)
   - 新建 CNAME:`sub` → `<tunnel-id>.cfargotunnel.com`,**橙云 ON**(隐藏 IP 的关键)
4. **收口**:确认 `https://sub.example.com/p?token=xxx` 通了后,acl-merge 已绑 `127.0.0.1`,
   防火墙只留 22 + 443(`ufw delete allow 8080` 等清掉不需要的)。

> `<tunnel-id>.cfargotunnel.com` 是 Cloudflare 的内部路由标记,不会解析出公网 IP,
> 所以橙云必须开 —— 灰云会让浏览器真去解析它而失败。

---

## 第 3 步:客户端订阅

Clash Verge → 订阅填:

```
https://sub.example.com/p?token=<你的token>
```

导入后应能看到节点、测延迟为绿、分流生效。首次导入 mihomo 会拉 geoip/geosite 数据库(模板里 jsdelivr 源),稍等即可。

---

## 运维

三个服务都是 systemd,开机自启:

```bash
systemctl status reality       # 节点
systemctl status acl-merge     # 订阅服务
systemctl status cloudflared   # 隧道

journalctl -u acl-merge -f     # 看订阅服务日志
```

acl-merge 每次构建会打一行摘要,便于核对:

```
构建完成: 节点 1/1(scrub 丢弃 0), 规则 原始=19 最终=43 (prepend=26 append=0 delete=0 scrub=2)
```

`scrub=2` 表示 gist 里有 2 条带旧域名的规则被拦下了(净化在起作用)。

---

## 手动运行 acl-merge(不用脚本时)

```bash
acl-merge \
  --listen 127.0.0.1:8080 \
  --acl-url /opt/xray/nodes.yaml \          # 本地文件,或任何吐 proxies: 的 http 上游
  --gist-url https://gist.../raw \
  --secret <token> \
  --scrub old.example.com,bad.example.com \  # 命中的节点整条丢弃;成品仍残留则拒发(fail loud)
  --cache-secs 120
```

- `--acl-url` 支持**本地文件路径**(非 http 开头当路径读)或任何吐 `proxies:` 的 http 上游
- `--scrub` 净化:命中的**节点整条丢弃**;序列化后若成品仍含关键词则**拒绝下发**(而非偷偷删行)
- 处理流水线:抽 proxies → 删 proxy-providers → groups 的 `use:` 转 inline `proxies:` →
  注入节点 → 应用 gist 规则(append 自动插在 MATCH 之前)→ 净化

## 从源码构建 / 发布

```bash
cargo build --release                                  # 本机 glibc 二进制
cargo build --release --target x86_64-unknown-linux-musl  # 静态,任何 Linux 可跑(需 musl-tools)
cargo test --release                                   # 跑内置流水线自检
```

模板在 `src/template.yaml`,换模板改这里再重编。来源溯源见该文件头部注释。
