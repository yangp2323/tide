set -e
export DEBIAN_FRONTEND=noninteractive
# swap
if ! swapon --show | grep -q swapfile; then
  fallocate -l 2G /swapfile && chmod 600 /swapfile && mkswap /swapfile >/dev/null && swapon /swapfile
  grep -q '/swapfile' /etc/fstab || echo '/swapfile none swap sw 0 0' >> /etc/fstab
  sysctl -w vm.swappiness=10 >/dev/null; echo 'vm.swappiness=10' > /etc/sysctl.d/99-swap.conf
fi
# BBR
cat > /etc/sysctl.d/99-bbr.conf <<B
net.core.default_qdisc=fq
net.ipv4.tcp_congestion_control=bbr
B
sysctl --system >/dev/null
apt-get update -qq
apt-get install -y -qq curl ca-certificates ufw fail2ban unzip jq >/dev/null
# firewall
ufw default deny incoming >/dev/null
ufw default allow outgoing >/dev/null
ufw allow 22/tcp >/dev/null
ufw allow 443/tcp >/dev/null
ufw --force enable >/dev/null
# fail2ban for sshd
cat > /etc/fail2ban/jail.local <<J
[sshd]
enabled = true
maxretry = 5
bantime = 1h
J
systemctl enable --now fail2ban >/dev/null 2>&1; systemctl restart fail2ban
echo "== done"; swapon --show; sysctl net.ipv4.tcp_congestion_control; ufw status | head -8; systemctl is-active fail2ban
