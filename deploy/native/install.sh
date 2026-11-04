#!/usr/bin/env bash
# Install a verified release. Prerequisites: caddy binary, nginx stream module,
# /etc/tide/tide.env and initialized/migrated /opt/tide/data.
set -euo pipefail
test "$(id -u)" = 0
release=$(realpath "${1:?usage: install.sh /opt/tide/releases/RELEASE}")
case "$release" in /opt/tide/releases/*) ;; *) echo 'Release must be inside /opt/tide/releases'; exit 1;; esac
test -f "$release/tide" && test -f "$release/web/index.html"
cd "$release"
sha256sum -c --quiet SHA256SUMS
test -f /etc/tide/tide.env && test -f /opt/tide/data/config.json
test -x /opt/tide/bin/caddy && test -f /usr/lib/nginx/modules/ngx_stream_module.so
exec 9>/run/tide-deploy.lock
flock -n 9
id tide >/dev/null 2>&1 || useradd --system --home-dir /opt/tide --shell /usr/sbin/nologin tide
install -d -m 700 -o tide -g tide /opt/tide/data /opt/tide/caddy-data /opt/tide/caddy-config
chown -R tide:tide /opt/tide/data /opt/tide/caddy-data /opt/tide/caddy-config
chmod 700 /opt/tide/data
chmod 600 /etc/tide/tide.env
install -m 644 deploy/tide.service deploy/tide-caddy.service deploy/tide-sni.service /etc/systemd/system/
install -m 644 deploy/Caddyfile deploy/sni.conf /etc/tide/
previous=$(readlink -f /opt/tide/current || true)
ln -sfn "$release" /opt/tide/current.next
mv -Tf /opt/tide/current.next /opt/tide/current
systemctl daemon-reload
rollback() {
  if test -n "$previous" && test -x "$previous/tide"; then
    ln -sfn "$previous" /opt/tide/current.next
    mv -Tf /opt/tide/current.next /opt/tide/current
    systemctl restart tide
    echo "Restored previous release: $previous"
  fi
}
trap rollback ERR
systemctl restart tide
ready=false
for i in $(seq 1 30); do
  if curl -fsS http://127.0.0.1:8000/healthz >/dev/null; then ready=true; break; fi
  sleep 1
done
test "$ready" = true
systemctl enable tide tide-caddy tide-sni
echo "Tide release ready: $release"
echo 'For first migration, switch HTTPS/SNI only after stopping the legacy containers.'
