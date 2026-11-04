#!/usr/bin/env bash
# Roll back application code only. Never restart the legacy Freqtrade engine if
# Rust has created orders: two engines must not manage the same account.
set -euo pipefail
release=$(realpath "${1:?usage: rollback.sh /opt/tide/releases/PREVIOUS}")
case "$release" in /opt/tide/releases/*) ;; *) exit 1;; esac
test -x "$release/tide"
bash "$release/deploy/install.sh" "$release"
systemctl restart tide-caddy tide-sni
