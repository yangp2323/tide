#!/usr/bin/env bash
# One-time transition from the existing Docker deployment. Python is used only
# inside the OLD container to pause its API; the new services never invoke it.
set -euo pipefail
release=$(realpath "${1:?usage: migrate-legacy.sh /opt/tide/releases/RELEASE}")
case "$release" in /opt/tide/releases/*) ;; *) exit 1;; esac
test -x "$release/tide"
test ! -f /opt/tide/data/config.json
cd /opt/tide
umask 077
backup="/opt/tide/backups/pre-rust-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$backup"
chmod 700 "$backup"

# Refuse to take over active positions. The API is stopped only after the first
# check, then checked again so a signal racing the pause cannot escape detection.
docker exec -i tide-bot python - <<'PY'
import base64, json, urllib.request
c=json.load(open('/freqtrade/user_data/config.json')); a=c['api_server']
auth=base64.b64encode((a['username']+':'+a['password']).encode()).decode()
def call(path, method='GET'):
    r=urllib.request.Request('http://localhost:8080/api/v1/'+path,method=method,
        data=b'' if method=='POST' else None,headers={'Authorization':'Basic '+auth})
    return json.load(urllib.request.urlopen(r,timeout=20))
assert not call('status'), 'Existing positions: migration refused'
print('Legacy state:',call('show_config')['state'])
call('stop','POST')
assert not call('status'), 'Position opened while pausing: migration refused'
print('Legacy engine paused; no tracked positions')
PY

# Stop every writer before taking a consistent filesystem backup.
docker compose stop freqtrade panel web
tar -czf "$backup/user-data.tgz" user_data
cp -a .env docker-compose.yml "$backup/"
tar -czf "$backup/caddy-data.tgz" -C /var/lib/docker/volumes/tide_caddy_data/_data .
install -d -m 700 /opt/tide/data
TIDE_DATA_DIR=/opt/tide/data "$release/tide" migrate /opt/tide/user_data
mkdir -p /opt/tide/caddy-data
cp -a /var/lib/docker/volumes/tide_caddy_data/_data/. /opt/tide/caddy-data/
bash "$release/deploy/install.sh" "$release"

# Existing HTTPS remains up until the native application's health check passes.
rollback_network() {
    systemctl stop tide-sni tide-caddy || true
    docker start tide-caddy tide-sni || true
    echo 'HTTPS restored to legacy containers. Trading remains paused; inspect logs before proceeding.'
}
trap rollback_network ERR
docker compose stop caddy sni
systemctl start tide-caddy tide-sni
set -a
. /etc/tide/tide.env
set +a
curl --retry 10 --retry-all-errors --retry-delay 1 --fail --silent --show-error "${TIDE_PUBLIC_ORIGIN}/healthz"
docker update --restart=no tide-bot tide-panel tide-web tide-caddy tide-sni >/dev/null
echo
echo "Native deployment complete. Backup: $backup"
echo 'Trading remains paused. Verify account and history before starting it in the UI.'
