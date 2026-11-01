# 服务器部署（东京 VPS）

敏感信息（IP、密码、节点链接、控制台登录）在 `secrets/服务器信息.md`，该目录已被 .gitignore 忽略。

| 文件 | 作用 |
| --- | --- |
| 01_base.sh | 基础环境：2G swap、BBR、ufw、fail2ban |
| 02_xray.sh | 安装 Xray，生成 VLESS + REALITY 配置（旧脚本；服务器现在跑的是 sing-box，监听 127.0.0.1:8444） |
| freqtrade/ | Freqtrade + 管理后台（app.py、index.html、backtest.py）+ 443 分流（docker-compose、Caddy、sni.conf、setup.sh），部署到 /opt/freqtrade |
| ssh_run.py | 本地通过 SSH 把脚本推到服务器执行 |
| tools/reality_diag.py | 在服务器上排查 REALITY 握手 |

防火墙只放行 22 和 443（ft-sni 分流：juyouagent.com → 控制台，其余 → sing-box 代理）。80 端口已关闭，证书通过 443 自动续期；用 IP 直接访问控制台会被拒绝（403）。
