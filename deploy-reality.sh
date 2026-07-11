#!/usr/bin/env bash
# deploy-reality.sh —— 在一台干净 VPS 上起一个裸 VLESS-Reality 节点(无 nginx/无证书/无 dokodemo)。
#
# 用法:  curl -sL <这个脚本的 gist raw 地址> | bash
#   可选环境变量覆盖默认值:
#     PORT=443              监听端口
#     DEST=www.apple.com    伪装站(见下方选站铁律!选错国内连不上)
#     XRAY_DIR=/opt/xray    安装目录
#
# 这个脚本没有任何秘密,可以安全地放公开 gist。密钥是每次运行现场生成的,只留在本机 creds.env。
#
# ── 伪装站(DEST)选站三铁律(2026-07-11 花几小时排障换来的) ──
#   ① 国内能访问、不被墙        —— Cloudflare 系(one.one.one.one)、google 系会被 GFW 干扰,别用
#   ② 证书链别太大              —— www.microsoft.com 证书 9659B 会撑爆 xray 转发缓冲,握手完不成
#   ③ 最好和 VPS 同区域          —— 延迟低、GFW 更不敏感
#   www.apple.com 三条都满足(全球 CDN、国内可访问、证书适中),已验证。备选:www.bing.com。
set -euo pipefail

PORT="${PORT:-443}"
DEST="${DEST:-www.apple.com}"
XRAY_DIR="${XRAY_DIR:-/opt/xray}"
SERVICE=reality

echo "==> [1/6] 安装 xray 到 $XRAY_DIR"
mkdir -p "$XRAY_DIR" && cd "$XRAY_DIR"
command -v unzip >/dev/null || { apt-get update -qq && apt-get install -y unzip -qq; }
curl -sL --max-time 120 https://github.com/XTLS/Xray-core/releases/latest/download/Xray-linux-64.zip -o x.zip
unzip -oq x.zip xray && rm x.zip && chmod +x xray
echo "    $(./xray version | head -1)"

echo "==> [2/6] 现场生成本机密钥"
UUID=$(./xray uuid)
KEYS=$(./xray x25519)
PRIV=$(echo "$KEYS" | awk '/PrivateKey/{print $2}')
PUB=$(echo "$KEYS"  | grep -oP 'Password.*?:\s*\K\S+')
SID=$(openssl rand -hex 8)
IP=$(curl -s4 --max-time 8 ifconfig.me || hostname -I | awk '{print $1}')
cat > "$XRAY_DIR/creds.env" <<EOF
UUID=$UUID
PRIVATE_KEY=$PRIV
PUBLIC_KEY=$PUB
SHORT_ID=$SID
SERVER_IP=$IP
PORT=$PORT
SNI=$DEST
EOF
chmod 600 "$XRAY_DIR/creds.env"

echo "==> [3/6] 写服务端配置 (target=$DEST, port=$PORT)"
cat > "$XRAY_DIR/config.json" <<EOF
{
  "log": { "loglevel": "warning" },
  "inbounds": [{
    "listen": "0.0.0.0", "port": $PORT, "protocol": "vless",
    "settings": { "clients": [{ "id": "$UUID", "flow": "xtls-rprx-vision" }], "decryption": "none" },
    "streamSettings": { "network": "tcp", "security": "reality",
      "realitySettings": {
        "target": "$DEST:443",
        "serverNames": ["$DEST"],
        "privateKey": "$PRIV",
        "shortIds": ["$SID"]
      } }
  }],
  "outbounds": [{ "protocol": "freedom" }]
}
EOF
./xray run -test -c "$XRAY_DIR/config.json" >/dev/null

echo "==> [4/6] 写 systemd unit 并启动"
cat > /etc/systemd/system/$SERVICE.service <<EOF
[Unit]
Description=Xray VLESS-Reality (clean)
After=network.target
[Service]
ExecStart=$XRAY_DIR/xray run -c $XRAY_DIR/config.json
Restart=on-failure
RestartSec=3
LimitNOFILE=65535
[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable --now $SERVICE >/dev/null 2>&1
sleep 2
systemctl is-active --quiet $SERVICE || { echo "!! $SERVICE 未启动"; journalctl -u $SERVICE -n 20 --no-pager; exit 1; }

echo "==> [5/6] 自检:loopback 实拨节点,确认 Reality 握手能走完 (这一步把'伪装站选错'挡在部署阶段)"
cat > /tmp/rt-c.json <<EOF
{ "log": {"loglevel": "warning"},
  "inbounds": [{"port": 10888, "listen": "127.0.0.1", "protocol": "socks"}],
  "outbounds": [{"protocol": "vless",
    "settings": {"vnext": [{"address": "127.0.0.1", "port": $PORT,
      "users": [{"id": "$UUID", "encryption": "none", "flow": "xtls-rprx-vision"}]}]},
    "streamSettings": {"network": "tcp", "security": "reality",
      "realitySettings": {"serverName": "$DEST", "fingerprint": "chrome",
        "publicKey": "$PUB", "shortId": "$SID"}}}] }
EOF
./xray run -c /tmp/rt-c.json >/dev/null 2>&1 &
RT_PID=$!
sleep 2
CODE=$(curl -s --max-time 15 -x socks5h://127.0.0.1:10888 -o /dev/null -w "%{http_code}" https://cp.cloudflare.com/generate_204 || echo 000)
kill $RT_PID 2>/dev/null; rm -f /tmp/rt-c.json
if [ "$CODE" != "204" ]; then
  echo "!! 自检失败(HTTP $CODE)。伪装站 '$DEST' 可能证书链太大或不可达。"
  echo "!! 换一个再来:  DEST=www.bing.com curl -sL <脚本> | bash"
  exit 1
fi
echo "    自检通过 (HTTP 204)"

echo "==> [6/6] 生成客户端节点文件 $XRAY_DIR/nodes.yaml"
cat > "$XRAY_DIR/nodes.yaml" <<EOF
proxies:
  - name: Reality-$(hostname)
    type: vless
    server: $IP
    port: $PORT
    uuid: $UUID
    network: tcp
    tls: true
    udp: true
    flow: xtls-rprx-vision
    servername: $DEST
    client-fingerprint: chrome
    reality-opts:
      public-key: $PUB
      short-id: $SID
EOF

# ── (可选) 部署 acl-merge 订阅服务 ──
# 设了 SECRET 环境变量才会装。它读上面生成的 nodes.yaml,套内置模板+gist 规则,输出干净 config。
#   SECRET=<token>              必填,触发 acl-merge 安装
#   GIST_URL=<raw gist url>     规则补丁(prepend/append/delete),不填则用空补丁
#   SCRUB=old.example.com       要净化的旧域名(逗号分隔)
#   LISTEN=127.0.0.1:8080       监听地址(默认本地,配合 CF Tunnel)
if [ -n "${SECRET:-}" ]; then
  echo "==> [+] 部署 acl-merge 订阅服务 (systemd)"
  curl -sL --max-time 60 https://github.com/zlotus/acl-merge/releases/latest/download/acl-merge -o "$XRAY_DIR/acl-merge"
  chmod +x "$XRAY_DIR/acl-merge"
  GIST_URL="${GIST_URL:-}"; SCRUB="${SCRUB:-}"; LISTEN="${LISTEN:-127.0.0.1:8080}"
  cat > /etc/systemd/system/acl-merge.service <<EOF
[Unit]
Description=acl-merge subscription server
After=network.target $SERVICE.service
[Service]
ExecStart=$XRAY_DIR/acl-merge --listen $LISTEN --acl-url $XRAY_DIR/nodes.yaml --gist-url "$GIST_URL" --secret "$SECRET" --scrub "$SCRUB" --cache-secs 120
Restart=on-failure
RestartSec=3
[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload
  systemctl enable --now acl-merge >/dev/null 2>&1
  sleep 2
  systemctl is-active --quiet acl-merge && echo "    acl-merge 已启动 (监听 $LISTEN)" || { echo "!! acl-merge 未启动"; journalctl -u acl-merge -n 10 --no-pager; }
fi

cat <<EOF

========================================================================
 完成。reality.service 已启动并开机自启,自检握手通过。
 节点文件: $XRAY_DIR/nodes.yaml (供 acl-merge 用 --acl-url 读)
 分享链接(可直接导入部分客户端):
 vless://$UUID@$IP:$PORT?encryption=none&flow=xtls-rprx-vision&security=reality&sni=$DEST&fp=chrome&pbk=$PUB&sid=$SID&type=tcp#Reality-$(hostname)
========================================================================
EOF
