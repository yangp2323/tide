set -e
# 在服务器上部署 Freqtrade + 网页控制台。先把本目录和 strategies/TrendFollow.py 传到 /opt/freqtrade 再执行：
#   scp -r deploy/freqtrade/* strategies/TrendFollow.py root@服务器:/opt/freqtrade/ && ssh root@服务器 bash /opt/freqtrade/setup.sh
# 可重复执行，不会覆盖已有的配置、Key 和交易记录。自定义域名：DOMAIN=example.com bash setup.sh（只在首次生成 .env 时生效）
export DEBIAN_FRONTEND=noninteractive
cd /opt/freqtrade
command -v docker >/dev/null || { curl -fsSL https://get.docker.com | sh >/tmp/docker.log 2>&1 || { tail -20 /tmp/docker.log; exit 1; }; }
systemctl enable --now docker >/dev/null 2>&1

IP=$(curl -4 -s -m 10 https://api.ipify.org)
mkdir -p user_data/strategies user_data/logs
mv -f TrendFollow.py user_data/strategies/ 2>/dev/null || true
# 控制台字体（Azeret Mono、Bricolage Grotesque，拉丁字符子集）
mkdir -p panel/fonts
[ -f panel/fonts/azeret.woff2 ] || curl -fsSL -o panel/fonts/azeret.woff2 https://fonts.gstatic.com/s/azeretmono/v21/3XFuErsiyJsY9O_Gepph-HHhZfk.woff2
[ -f panel/fonts/bricolage.woff2 ] || curl -fsSL -o panel/fonts/bricolage.woff2 https://fonts.gstatic.com/s/bricolagegrotesque/v9/3y996as8bTXq_nANBjzKo3IeZx8z6up5L-iNGQ.woff2

if [ ! -f user_data/config.json ]; then
JWT=$(openssl rand -hex 32); WS=$(openssl rand -hex 16); APIPASS=$(openssl rand -hex 16)
cat > user_data/config.json <<J
{
  "bot_name": "bybit-trend-LIVE",
  "dry_run": false,
  "db_url": "sqlite:////freqtrade/user_data/tradesv3.live.sqlite",
  "trading_mode": "spot",
  "stake_currency": "USDT",
  "stake_amount": "unlimited",
  "tradable_balance_ratio": 0.95,
  "max_open_trades": 3,
  "cancel_open_orders_on_exit": false,
  "unfilledtimeout": {"entry": 10, "exit": 10, "unit": "minutes"},
  "entry_pricing": {"price_side": "same", "use_order_book": true, "order_book_top": 1},
  "exit_pricing": {"price_side": "same", "use_order_book": true, "order_book_top": 1},
  "exchange": {
    "name": "bybit",
    "pair_whitelist": ["BTC/USDT", "ETH/USDT", "SOL/USDT"],
    "pair_blacklist": []
  },
  "pairlists": [{"method": "StaticPairList"}],
  "telegram": {
    "enabled": false, "token": "", "chat_id": "",
    "notification_settings": {
      "status": "silent", "warning": "on", "startup": "silent",
      "entry": "silent", "entry_fill": "on", "entry_cancel": "on",
      "exit": "silent", "exit_fill": "on", "exit_cancel": "on",
      "protection_trigger": "on", "protection_trigger_global": "on"
    }
  },
  "api_server": {
    "enabled": true, "listen_ip_address": "0.0.0.0", "listen_port": 8080,
    "verbosity": "error", "enable_openapi": false,
    "jwt_secret_key": "$JWT", "ws_token": "$WS", "CORS_origins": [],
    "username": "panel", "password": "$APIPASS"
  },
  "initial_state": "stopped",
  "force_entry_enable": false,
  "internals": {"process_throttle_secs": 5}
}
J
fi
[ -f user_data/config-private.json ] || echo '{}' > user_data/config-private.json
chown -R 1000:1000 user_data
chmod 600 user_data/config.json user_data/config-private.json

# 控制台登录密码，只在第一次生成
if [ ! -f .env ]; then
  PASS=$(openssl rand -base64 12 | tr -d '/+=')
  HASH=$(docker run --rm caddy:2-alpine caddy hash-password --plaintext "$PASS")
  cat > .env <<E
SERVER_IP=$IP
DOMAIN=${DOMAIN:-$(echo "$IP" | tr . -).sslip.io}
PANEL_USER=admin
PANEL_HASH='$HASH'
E
  chmod 600 .env
  echo "admin / $PASS" > /root/freqtrade-panel.txt; chmod 600 /root/freqtrade-panel.txt
fi

# 443 由 ft-sni 按域名分流，sing-box 代理需要监听 127.0.0.1:8444（只改一次）
SB=/etc/sing-box/conf/VLESS-REALITY-443.json
if [ -f $SB ] && grep -q '"listen_port": 443' $SB; then
  python3 - <<'PY'
import json; p="/etc/sing-box/conf/VLESS-REALITY-443.json"
c=json.load(open(p))
for i in c["inbounds"]:
    if i.get("listen_port")==443: i["listen"]="127.0.0.1"; i["listen_port"]=8444
json.dump(c,open(p,"w"),indent=2)
PY
  systemctl restart sing-box
fi
# 只开 443：证书走 443 的 TLS-ALPN 验证，不需要 80
ufw allow 443/tcp >/dev/null; ufw delete allow 80/tcp >/dev/null 2>&1 || true
docker compose pull -q
docker compose up -d --remove-orphans
. ./.env
echo "控制台地址: https://trade.$DOMAIN"
echo "登录: $(cat /root/freqtrade-panel.txt)"
