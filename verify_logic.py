import yaml, copy

# ---- 模拟输入 ----
# 1) 上游订阅(只含 proxies),模拟你新的纯 Reality 节点
acl_upstream = {
    "proxies": [
        {"name": "reality-vps", "type": "vless", "server": "203.0.113.10",
         "port": 14131, "uuid": "REDACTED-UUID", "network": "tcp", "tls": True, "udp": True,
         "servername": "www.microsoft.com",
         "reality-opts": {"public-key": "PBK...", "short-id": "6ba85179e30d4fc2"},
         "client-fingerprint": "chrome", "flow": "xtls-rprx-vision"},
    ]
}
# 2) gist 规则补丁(你现有格式)
gist_patch = {
    "prepend": [
        "IP-CIDR,203.0.113.10/32,DIRECT",
        "PROCESS-NAME,tailscaled,DIRECT",
        "IP-CIDR,100.64.0.0/10,DIRECT",
        "AND,((NETWORK,UDP),(DST-PORT,443),(DOMAIN-SUFFIX,claude.ai)),REJECT",
    ],
    "append": [],
    "delete": [],
}
scrub = ["example.com"]  # 净化旧域名

# ---- 复刻 rust 逻辑 ----
root = yaml.safe_load(open('acl-merge/src/template.yaml').read())
proxy_names = [p["name"] for p in acl_upstream["proxies"]]

# remove_proxy_providers
root.pop("proxy-providers", None)

# rewrite_groups_use_to_proxies
for g in root.get("proxy-groups", []):
    existing = [x for x in (g.get("proxies") or []) if x is not None]
    had_use = "use" in g
    g.pop("use", None)
    if had_use:
        gtype = g.get("type", "select")
        name = g.get("name", "")
        if gtype == "url-test" or name == "手动切换":
            g["proxies"] = list(proxy_names)
        else:
            g["proxies"] = list(proxy_names) + existing

# inject_proxies
root["proxies"] = acl_upstream["proxies"]

# apply_rule_patch
rules = root.get("rules", [])
if gist_patch["delete"]:
    rules = [r for r in rules if not any(r == d or d in r for d in gist_patch["delete"])]
rules = gist_patch["prepend"] + rules + gist_patch["append"]
root["rules"] = rules

# scrub
out = yaml.dump(root, allow_unicode=True, sort_keys=False)
out = "\n".join(l for l in out.splitlines() if not any(k in l for k in scrub)) + "\n"

# ---- 检查产出 ----
res = yaml.safe_load(out)
print("=== 产出校验 ===")
print("有 proxy-providers :", "proxy-providers" in res, " (期望 False)")
print("有 proxies         :", "proxies" in res, "节点数:", len(res["proxies"]))
print("任一组还有 use 字段 :", any("use" in g for g in res["proxy-groups"]), " (期望 False)")
print("含旧域名 example.com:", "example.com" in out, " (期望 False)")
print("rules 前3条        :")
for r in res["rules"][:3]: print("   ", r)
print("'手动切换'组的proxies:", next(g for g in res['proxy-groups'] if g['name']=='手动切换')['proxies'])
print("'全球代理'组的proxies:", next(g for g in res['proxy-groups'] if g['name']=='全球代理')['proxies'])
print("'自动选择'组的proxies:", next(g for g in res['proxy-groups'] if g['name']=='自动选择')['proxies'])
open('sample_output.yaml','w').write(out)
print("\n完整样例已写入 sample_output.yaml, 行数:", len(out.splitlines()))
