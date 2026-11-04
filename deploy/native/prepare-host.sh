#!/usr/bin/env bash
# One-time preparation for the existing Tokyo host. Does not stop live containers.
set -euo pipefail
test "$(id -u)" = 0
install -d -m 755 /opt/tide/bin /opt/tide/releases /etc/tide
if ! command -v nginx >/dev/null || ! test -f /usr/lib/nginx/modules/ngx_stream_module.so; then
    # Prevent the package's default HTTP server from starting during installation.
    systemctl mask nginx.service
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq nginx-core libnginx-mod-stream sqlite3
fi
if ! test -x /opt/tide/bin/caddy; then
    docker cp tide-caddy:/usr/bin/caddy /opt/tide/bin/caddy
    chmod 755 /opt/tide/bin/caddy
fi
if ! test -f /etc/tide/tide.env; then
    set -a
    . /opt/tide/.env
    set +a
    umask 077
    printf '%s\n' \
      'TIDE_LISTEN=127.0.0.1:8000' \
      'TIDE_DATA_DIR=/opt/tide/data' \
      'TIDE_WEB_DIR=/opt/tide/current/web' \
      "TIDE_PUBLIC_ORIGIN=https://trade.$DOMAIN" \
      "PANEL_SESSION_SECRET=$PANEL_SESSION_SECRET" \
      "DOMAIN=$DOMAIN" "SERVER_IP=$SERVER_IP" 'RUST_LOG=tide=info' > /etc/tide/tide.env
fi
/opt/tide/bin/caddy version
/usr/sbin/nginx -V 2>&1 | head -1
