# 原生部署与迁移

当前目标为 Ubuntu 22.04 x86_64，项目目录 `/opt/tide`，SSH 名称 `s-tokyo`。服务均由 systemd 管理，核心应用无 Docker、Node.js、Python 运行依赖。

## 一键部署（后续升级）

在项目根目录的 Linux / WSL 终端运行：

```bash
make depoly
```

`make deploy` 是同一个命令的别名。默认使用 SSH 配置中的 `s-tokyo`，也可运行 `make depoly DEPLOY_HOST=其他SSH别名`。需要免交互 SSH 登录和远端 root 权限。主机别名只配置在 Windows 时，WSL 会自动使用 Windows OpenSSH，复用 Windows 的主机别名和密钥。

构建需要 make、Python 3（仅运行部署测试）、Rust（`rust-toolchain.toml`）、musl 工具链、Node.js（`.nvmrc`）和 Corepack/pnpm；使用 nvm 时先运行 `nvm install`，之后脚本会自动加载项目指定的 Node 版本。服务器需要已完成原生安装、sqlite3 和 flock。本命令不负责首次初始化或旧版迁移。

脚本会依次执行格式检查、后端测试、锁定依赖构建、前端类型检查和构建，生成全新的发布包并校验上传内容。服务器加锁后，将配置、登录凭据、环境文件和 SQLite 一致性备份保存到 `/opt/tide/backups/<版本>`，校验发布包，原子切换版本并重启 Tide。随后检查实际运行的二进制、本机与公网健康接口以及页面访问。

部署保留已有配置、密钥、运行开关和交易记录，不重装系统服务或覆盖 HTTPS/SNI 配置。构建和上传期间旧版本继续运行，仅切换时短暂重启；重启后的自动交易保持现有设置。切换后检查失败时自动恢复上一版应用代码；**不会自动恢复数据库或撤销交易**。每次部署保留备份、发布包及旧版本供核对；本地和服务器均禁止并发部署。

## 仅构建和手动上传

在 Linux / WSL 构建：

```bash
bash deploy/native/build.sh
scp dist/tide-linux-amd64.tgz s-tokyo:/root/
```

在服务器解包到新的版本目录（用实际版本号替换）：

```bash
release=/opt/tide/releases/20261102-rust
mkdir -p "$release"
tar -xzf /root/tide-linux-amd64.tgz -C "$release"
cd "$release" && sha256sum -c SHA256SUMS
```

## 从旧版首次迁移

1. 执行 `prepare-host.sh`，安装原生 Nginx stream 模块，从旧容器复制 Caddy 二进制，并生成 `/etc/tide/tide.env`。这一准备过程不停止旧容器。
2. 通过旧引擎的接口暂停开仓。核对 `/status`、Bybit 挂单和持仓；发现未完成订单或仓位则先处理，禁止让两个引擎同时管理账户。
3. 停止旧交易引擎和业务服务，将 `/opt/tide/user_data`、`.env`、Compose 文件和证书卷备份到仅 root 可读的位置。
4. 使用新程序迁移：

   ```bash
   umask 077
   TIDE_DATA_DIR=/opt/tide/data "$release/tide" migrate /opt/tide/user_data
   ```

5. 将旧 `tide_caddy_data` 卷的内容复制到 `/opt/tide/caddy-data`，保留内部的 `caddy/` 目录。检查 `DOMAIN`、`TIDE_PUBLIC_ORIGIN` 和 `sni.conf` 中的域名一致；分流配置默认使用 `juyouagent.com`。
6. 执行 `bash "$release/deploy/install.sh" "$release"`。先在本机检查新 `/healthz`、登录、账户、持仓、历史记录和 AI 只读快照。
7. 停止旧 Caddy 和 SNI 容器，再启动 `tide-caddy`、`tide-sni` 原生服务。验证公网登录页及受保护接口后禁用旧容器的自动重启。

迁移后新引擎保持暂停，旧数据和容器保留用于核对；旧交易引擎不得与新引擎同时运行。不要在新引擎已经下单后直接恢复 Freqtrade，否则旧数据库无法跟踪新订单。

## 后续升级

上传到新的 `releases/<版本>` 目录，备份配置及 SQLite，再执行该版本的 `install.sh`。脚本检查二进制校验和、切换 `current` 并重启应用；健康检查失败时恢复上一版本链接。

数据库在线备份需用 SQLite backup，不能只复制处于 WAL 模式的主文件：

```bash
umask 077
sqlite3 /opt/tide/data/tide.sqlite '.backup /root/tide-backup.sqlite'
```

代码回滚：`bash deploy/native/rollback.sh /opt/tide/releases/<上一版本>`。这只回滚原生应用代码，不回滚交易账户或订单，不会启动旧 Freqtrade。

## 管理

```bash
systemctl status tide tide-caddy tide-sni
systemctl restart tide
journalctl -u tide --since '10 minutes ago'
curl -fsS http://127.0.0.1:8000/healthz
```

新增域名须同时调整 `/etc/tide/tide.env` 和 `/etc/tide/sni.conf`。只对公网开放 443，交易 API 的 8000 和 Caddy 的 8443 只监听回环地址。原有 sing-box 服务继续监听 127.0.0.1:8444。

密钥和密码保存在受权限保护的服务器文件中，禁止将 `/opt/tide/data` 或 `/etc/tide/tide.env` 提交到仓库。
