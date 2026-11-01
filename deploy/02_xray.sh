set -e
bash -c "$(curl -fsSL https://github.com/XTLS/Xray-install/raw/main/install-release.sh)" @ install >/tmp/xray-install.log 2>&1 || { tail -20 /tmp/xray-install.log; exit 1; }
UUID=$(xray uuid)
KEYS=$(xray x25519)
PRIV=$(echo "$KEYS" | grep -iE '^(Private|PrivateKey)' | awk -F': ' '{print $2}')
PUB=$(echo "$KEYS" | grep -iE '^(Public|Password)' | head -1 | awk -F': ' '{print $2}')
SID=$(openssl rand -hex 8)
SNI=www.microsoft.com
cat > /usr/local/etc/xray/config.json <<J
{
  "log": {"loglevel": "warning"},
  "inbounds": [{
    "listen": "0.0.0.0", "port": 443, "protocol": "vless",
    "settings": {"clients": [{"id": "$UUID", "flow": "xtls-rprx-vision"}], "decryption": "none"},
    "streamSettings": {
      "network": "tcp", "security": "reality",
      "realitySettings": {"dest": "$SNI:443", "serverNames": ["$SNI"], "privateKey": "$PRIV", "shortIds": ["$SID"]}
    },
    "sniffing": {"enabled": true, "destOverride": ["http", "tls", "quic"]}
  }],
  "outbounds": [{"protocol": "freedom", "tag": "direct"}, {"protocol": "blackhole", "tag": "block"}],
  "routing": {"rules": [{"type": "field", "ip": ["geoip:private"], "outboundTag": "block"}]}
}
J
xray run -test -c /usr/local/etc/xray/config.json | tail -1
systemctl enable xray >/dev/null 2>&1; systemctl restart xray; sleep 1; systemctl is-active xray
ss -tlnp | grep ':443'
echo "UUID=$UUID"; echo "PUB=$PUB"; echo "SID=$SID"; echo "SNI=$SNI"
xray version | head -1
