#!/usr/bin/env bash
# Existing native installations only. Never restore a trading database automatically.
set -Eeuo pipefail
test "$(id -u)" = 0
release_id=${1:?release id required}
checksum=${2:?archive checksum required}
[[ "$release_id" =~ ^[a-zA-Z0-9-]+$ && "$checksum" =~ ^[a-f0-9]{64}$ ]]
exec 9>/run/tide-deploy.lock
flock -n 9 || { echo 'Another server deployment is running'; exit 1; }
archive="/root/tide-$release_id.tgz"
release="/opt/tide/releases/$release_id"
backup="/opt/tide/backups/$release_id"
printf '%s  %s\n' "$checksum" "$archive" | sha256sum -c --quiet
test ! -e "$release" && test ! -e "$backup"
previous=$(readlink -f /opt/tide/current)
test -x "$previous/tide"
test -f /opt/tide/data/config.json && test -f /opt/tide/data/tide.sqlite
systemctl is-active --quiet tide tide-caddy tide-sni

umask 022
install -d -m 755 "$release"
tar --no-same-owner -xzf "$archive" -C "$release"
(cd "$release" && sha256sum -c --quiet SHA256SUMS)
test -x "$release/tide" && test -f "$release/web/index.html"
# The service account must be able to read the complete static site.
chmod -R a+rX "$release"
umask 077
install -d -m 700 "$backup"
cp -p /opt/tide/data/config.json /opt/tide/data/auth.json /etc/tide/tide.env "$backup/"
sqlite3 /opt/tide/data/tide.sqlite ".timeout 30000" ".backup '$backup/tide.sqlite'"
test "$(sqlite3 "$backup/tide.sqlite" 'PRAGMA quick_check;')" = ok
sha256sum /opt/tide/data/config.json /opt/tide/data/auth.json /etc/tide/tide.env > "$backup/config.sha256"
printf '%s\n' "$previous" > "$backup/previous-release"

switched=false
rollback() {
  code=$?
  trap - ERR
  if test "$switched" = true; then
    echo "Deployment failed; restoring $previous" >&2
    ln -sfn "$previous" /opt/tide/current.next
    mv -Tf /opt/tide/current.next /opt/tide/current
    systemctl restart tide || true
    curl --retry 10 --retry-connrefused --retry-delay 1 --max-time 5 -fsS http://127.0.0.1:8000/healthz || true
  fi
  echo "Deployment failed. Backup: $backup (database was not restored)." >&2
  exit "$code"
}
trap rollback ERR
ln -sfn "$release" /opt/tide/current.next
switched=true
mv -Tf /opt/tide/current.next /opt/tide/current
systemctl restart tide
ready=false
for ((i=0; i<30; i++)); do
  if systemctl is-active --quiet tide && curl --max-time 2 -fsS http://127.0.0.1:8000/healthz >/dev/null; then
    ready=true
    break
  fi
  sleep 1
done
test "$ready" = true
pid=$(systemctl show tide -p MainPID --value)
test "$(readlink -f "/proc/$pid/exe")" = "$release/tide"
sha256sum -c --quiet "$backup/config.sha256"
origin=$(sed -n 's/^TIDE_PUBLIC_ORIGIN=//p' /etc/tide/tide.env | tr -d '\r\"\047')
[[ "$origin" == https://* ]]
curl --retry 2 --max-time 15 -fsS "$origin/healthz" >/dev/null
curl --retry 2 --max-time 15 -fsS "$origin/" >/dev/null
systemctl is-active --quiet tide tide-caddy tide-sni
trap - ERR
echo "Deployed: $release"
echo "Backup: $backup"
echo "Previous release: $previous"
