#!/usr/bin/env bash
# Run from Linux or WSL; SSH authentication stays in the user's SSH config.
set -euo pipefail
cd "$(dirname "$0")/../.."
host=${DEPLOY_HOST:-s-tokyo}
[[ "$host" =~ ^[a-zA-Z0-9_.@-]+$ ]] || { echo 'Invalid DEPLOY_HOST'; exit 1; }
mkdir -p dist
exec 8>dist/deploy.lock
flock -n 8 || { echo 'Another local deployment is running'; exit 1; }

ssh_cmd=${DEPLOY_SSH:-ssh}
scp_cmd=${DEPLOY_SCP:-scp}
# Reuse Windows aliases when the requested alias is not defined in WSL.
win_ssh=/mnt/c/Windows/System32/OpenSSH/ssh.exe
if test -z "${DEPLOY_SSH:-}" && test -x "$win_ssh"; then
  native_host=$(ssh -G "$host" 2>/dev/null | sed -n 's/^hostname //p')
  windows_host=$("$win_ssh" -G "$host" 2>/dev/null | sed -n 's/^hostname //p' | tr -d '\r')
  if test "$native_host" = "${host##*@}" && test "$windows_host" != "$native_host"; then
    ssh_cmd=$win_ssh
    scp_cmd=/mnt/c/Windows/System32/OpenSSH/scp.exe
  fi
fi
opts=(-o BatchMode=yes -o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=4)
echo "Checking deployment host: $host"
"$ssh_cmd" "${opts[@]}" "$host" 'test "$(id -u)" = 0 && command -v sqlite3 >/dev/null && command -v flock >/dev/null && systemctl is-active --quiet tide && test -f /etc/tide/tide.env && test -L /opt/tide/current'

echo 'Testing and building release'
bash deploy/native/build.sh
release_id="$(date -u +%Y%m%dT%H%M%SZ)-$(git rev-parse --short HEAD)-$(od -An -N4 -tx1 /dev/urandom | tr -d ' \n')"
archive="$PWD/dist/tide-linux-amd64.tgz"
checksum=$(sha256sum "$archive" | cut -d ' ' -f1)
if [[ "$scp_cmd" == *.exe ]]; then archive=$(wslpath -w "$archive"); fi
echo "Uploading release: $release_id"
"$scp_cmd" "${opts[@]}" "$archive" "$host:/root/tide-$release_id.tgz"
"$ssh_cmd" "${opts[@]}" "$host" "bash -s -- '$release_id' '$checksum'" < deploy/native/upgrade.sh
