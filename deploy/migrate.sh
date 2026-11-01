#!/bin/bash
# 把 /opt/freqtrade 迁移到 /opt/tide（改名 tide），分两步执行：
#   bash migrate.sh prep     准备：备份、复制目录、迁移卷（旧栈继续运行，不影响线上）
#   bash migrate.sh cutover  切换：解压新代码、停旧栈、起新栈
#   bash migrate.sh rollback 回滚：停新栈、起旧栈
set -euo pipefail

OLD=/opt/freqtrade
NEW=/opt/tide

prep() {
  echo "=== 1/4 备份 ==="
  tar -czf /root/tide-pre-rename.tgz -C /opt \
    --exclude=freqtrade/web/node_modules --exclude=freqtrade/web/.next --exclude=freqtrade/web/.pnpm-store \
    freqtrade 2>/dev/null || true
  ls -lh /root/tide-pre-rename.tgz | awk '{print $5, $9}'

  echo "=== 2/4 复制部署目录 ==="
  rm -rf "$NEW"
  cp -a "$OLD" "$NEW"
  rm -rf "$NEW/panel/web" "$NEW/web/node_modules" "$NEW/web/.next" "$NEW/web/.pnpm-store"
  du -sh "$NEW"

  echo "=== 3/4 登录信息 ==="
  if [ -f /root/freqtrade-panel.txt ]; then cp -f /root/freqtrade-panel.txt /root/tide-panel.txt; else echo "没有旧登录文件"; fi

  echo "=== 4/4 迁移卷 ==="
  for v in caddy_data web_node_modules web_next; do
    docker volume create "tide_$v" >/dev/null
    docker run --rm -v "freqtrade_$v:/from" -v "tide_$v:/to" alpine:3.20 sh -c 'cp -a /from/. /to/ 2>/dev/null || true'
  done
  du -sh /var/lib/docker/volumes/tide_* 2>/dev/null || true

  echo "=== 准备完成，旧栈仍在运行 ==="
  docker ps --format '{{.Names}} {{.Status}}'
}

cutover() {
  echo "=== 1/3 解压新代码到 $NEW ==="
  tar -xzf /root/deploy.tgz -C "$NEW"
  [ -f /root/TrendFollow.py ] && mv -f /root/TrendFollow.py "$NEW/" || true
  ls "$NEW" | tr '\n' ' '; echo

  echo "=== 2/3 停旧栈 ==="
  (cd "$OLD" && docker compose down)

  echo "=== 3/3 启动新栈 ==="
  (cd "$NEW" && bash setup.sh)

  echo "=== 完成 ==="
  docker ps --format '{{.Names}} {{.Status}}'
}

rollback() {
  echo "=== 停新栈，恢复旧栈 ==="
  (cd "$NEW" && docker compose down) || true
  (cd "$OLD" && docker compose up -d)
  docker ps --format '{{.Names}} {{.Status}}'
}

case "${1:-}" in
  prep) prep ;;
  cutover) cutover ;;
  rollback) rollback ;;
  *) echo "用法: bash migrate.sh prep|cutover|rollback"; exit 1 ;;
esac
