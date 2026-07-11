# acl-merge v2

从上游订阅只抽取节点(proxies),套用【内置的 vasma 原版 clashMeta 模板】,
注入 gist 规则补丁,把 provider 模式转成 inline proxies,
输出**无域名、无 phone-home、自包含**的干净 clash 配置。

模板已用 `include_str!("template.yaml")` 焊死在二进制里,
以后换不换 vasma、vasma 怎么更新它的模板,都不影响你的输出格式。

## 编译(在你的 x86_64 VPS 上)
```bash
# 若无 rust: curl https://sh.rustup.rs -sSf | sh -s -- -y
cargo build --release
# 产物: target/release/acl-merge  (已 strip + opt-level=z,体积小,适合低配 VPS)
```

## 运行(沿用你原来的习惯)
```bash
./acl-merge \
  --listen "127.0.0.1:8080" \
  --acl-url "http://127.0.0.1:40353/s/clashMeta/<你的订阅id>" \
  --gist-url "https://gist.githubusercontent.com/.../my_rules.yaml" \
  --secret "some-token" \
  --scrub "example.com,bad.example.com" \
  --cache-secs 120
```

设备端访问: `http://<地址>:8080/p?token=some-token`

## 相比 v1 的关键变化
- 模板**内置**,不再从 vasma 在线读整份 config(--acl-url 现在只用来取节点)
- 自动删除 proxy-providers,把 groups 的 `use:` 转成 inline `proxies:`
- 新增 `--scrub`: 净化输出里任何残留的旧域名(命中的整行删除)
- 新增 `--cache-secs`: 低配 VPS 缓存,避免每次请求都回源
- --acl-url 可换成任何能吐 `proxies:` 的上游(acl4ssr / 机场订阅 / vasma 本机订阅)

## 隐私建议
- --listen 绑 127.0.0.1,再用 CF Tunnel(sub.example.com 橙云) 或 Tailscale 对外,
  这样设备侧订阅走 HTTPS、真实 IP 不暴露。
- --acl-url 指向 vasma 的**本机** 127.0.0.1 订阅(nginx 已绑本地),节点抽取全程不出网。

## 更新内置模板
若哪天你想换模板,替换 src/template.yaml 后 `cargo build --release` 即可。
当前模板 = mack-a/v2ray-agent install.sh 中 clashMetaProfiles 的原版骨架。
