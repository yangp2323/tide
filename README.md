# Tide · Rust + Next.js 量化交易

Bybit 实盘交易管理平台。后端和交易引擎全部使用 Rust，前端使用 Next.js 静态构建。服务器直接运行二进制程序，通过 systemd 管理；运行时不依赖 Docker、Python、Freqtrade 或 Node.js。

管理后台：[trade.juyouagent.com](https://trade.juyouagent.com)。首次从旧版迁移后交易保持暂停，核对账户和配置后在页面启动。

## 项目结构

```text
backend/src/
  main.rs          服务入口、systemd 信号处理
  api.rs           管理接口、账户、策略设置
  auth.rs          scrypt 密码、HMAC 会话、Origin 检查
  exchange.rs      Bybit V5 签名、行情、下单及成交查询
  engine.rs        仓位、订单恢复、风控、止损
  indicators.rs    EMA / ADX / RSI / ATR
  ai.rs            Anthropic / OpenAI 兼容模型、定时决策
  store.rs         SQLite 订单、交易、决策和日志
  migrate.rs       旧版配置、登录及历史交易导入
  mcp.rs           MCP stdio 客户端
  tests.rs         模拟交易所及 HTTP 集成测试
web/               Next.js / React / TypeScript 页面
deploy/native/     构建脚本、原生服务、HTTPS 与 443 分流
```

## 架构

```text
浏览器 → Nginx SNI :443 → Caddy HTTPS :8443 → Rust :8000
                                              ├─ Next.js 静态页面
                                              ├─ 管理 / AI / MCP API
                                              ├─ SQLite
                                              └─ Bybit V5 REST
其他 TLS 域名 → 原有 sing-box :8444
```

Next.js 只用于开发和构建。部署包包含一个静态链接的 Rust 程序及 `web/` 静态资源；HTTPS 和分流同样以本机二进制服务运行。

## 本地构建

需要 Rust stable、musl 工具链、Node.js 和 pnpm（仅构建时）。Linux / WSL：

```bash
rustup target add x86_64-unknown-linux-musl
sudo apt-get install musl-tools
bash deploy/native/build.sh
```

产物：`dist/tide-linux-amd64.tgz`。依赖由 `Cargo.lock` 和 `web/pnpm-lock.yaml` 固定。

单独检查：

```bash
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cd web && pnpm install --frozen-lockfile && pnpm build
```

## 原生部署

服务器部署目录 `/opt/tide`，SSH 名称 `s-tokyo`。详细流程见 [部署说明](deploy/README.md)。

后续线上更新在项目根目录的 Linux / WSL 终端运行 `make depoly`（也支持 `make deploy`），自动测试、构建、备份、部署和检查，失败时回退代码。

```text
/opt/tide/bin/caddy             HTTPS 二进制
/opt/tide/releases/<版本>/tide  Rust 二进制
/opt/tide/releases/<版本>/web/  Next.js 静态资源
/opt/tide/current              当前版本软链接
/opt/tide/data/config.json     配置和 API 密钥，权限 600
/opt/tide/data/auth.json       管理员密码哈希，权限 600
/opt/tide/data/tide.sqlite     交易、订单、决策、运行日志
/opt/tide/caddy-data/          HTTPS 证书
/etc/tide/tide.env             环境配置，权限 600
```

服务管理：

```bash
systemctl status tide tide-caddy tide-sni
journalctl -u tide -f
curl http://127.0.0.1:8000/healthz
```

首次初始化新账户：设置 `TIDE_DATA_DIR`、`TIDE_ADMIN_PASSWORD`，执行 `tide init`。密码仅在初始化时需要；已有配置不会被覆盖。

旧版迁移：停止旧引擎并核对所有机器人仓位和挂单后，执行 `TIDE_DATA_DIR=/opt/tide/data tide migrate /opt/tide/user_data`。发现任一旧实盘库有未平仓记录就拒绝导入。导入现货和合约历史交易、登录密码、API Key、Telegram、AI 配置和决策记录，不导入模拟盘记录。旧数据原样保留。

## 策略与交易行为

- AI 使用 4 小时收盘 K 线和技术指标进行分析并决定开仓、持有或平仓；系统不再运行独立的均线自动交易策略。
- 默认暂停开仓。暂停时，已有仓位的止损、止盈继续执行。
- 最大持仓数只限制同时持仓数量，不要求资金足够开满。开仓预算预留 5% 后，按剩余名额与实际能支持的笔数分配；最低下单金额和数量以交易所对该币种的要求为准，不额外设置每笔 10 USDT 的门槛。
- 止损、止盈由 AI 在开仓时设置（止损限制在 2%–10%，缺省为 2.5×ATR；止盈缺省为止损距离的 2 倍），之后 AI 可用 `set_stops` 调整，止损只能收紧。两次分析之间，价格走出一个止损距离后，系统按止损距离的一半追踪止损作保底。方向、同时持仓数量、仓位大小和杠杆都由 AI 决定，页面上没有对应开关：同时最多 5 笔，每笔风险不超过 2%（代码常量 `MAX_OPEN`、`MAX_RISK_PER_TRADE_PCT`），杠杆不超过最高杠杆、交易所上限和保证止损先于强平的安全值，超出上限的请求会被下调而不是拒绝；快照会告诉 AI 每个币的最小下单额。按价格计算，杠杆会放大本金盈亏。
- 风控熔断：账户回撤超过上限（默认 20%）自动暂停机器人；连续亏损达到上限（默认 5 笔）停止开仓。每次启动机器人会重置回撤基线和亏损计数。AI 页可开启「影子模式」，只记录决策不下单。
- 市价单执行；交易所可能部分成交或拒绝成交。确认实际成交数量、价格和手续费后才更新本地仓位。
- 下单前将唯一订单号持久化。超时先核对成交，不盲目重发。存在待确认订单时禁止新开仓，重启后继续核对。
- 所有下单入口串行检查持仓、资金和风控。AI 自动执行和手动分析执行采用相同置信度限制；外部明确下单不接收模型置信度。
- 合约支持设置 1–25 倍杠杆，并校验交易对在交易所的杠杆上限；现货保持 1 倍。合约要求 Bybit 统一账户使用逐仓、单向持仓模式。合约止损挂在交易所，并同步实际平仓；不合并已有的手动合约仓位。
- 现货止损挂成 Bybit 条件单（卖出前先撤单），挂单失败时由 Rust 服务按价格监控；现货止盈由服务监控，服务关闭期间无法执行。现货平仓精度以下余额记为零碎余额，仍可在 Bybit 账户中查看。
- 自动交易只由 AI 决策，不再运行均线策略；不限制每日开仓次数。平仓释放名额后，AI 可继续开多或开空（需允许做空）。待确认订单仍会阻止新开仓，当日亏损和置信度限制继续执行。
- AI 以账户净盈利增长为目标，结合手续费、滑点与适用的资金费评估交易机会；根据后续预期收益判断开仓、持有和平仓，不以单笔回本为目标。分析须说明具体行情依据；未达到日亏损上限仅表示通过一项开仓限制，不代表存在盈利机会。现有风控仍生效，缺乏正预期收益依据时可以不交易。

实盘汇总以本地记录的成交和交易费用为基础，不能代替交易所账单（例如合约资金费及外部交易会影响账户权益）。

## AI 与外部接入

接口地址填根域名时，OpenAI 兼容模式会自动补 `/v1`（例如 `https://api.onerouter.com` → `/v1/responses`）；完整端点与自定义路径前缀保留原路径。所有模型请求明确使用非流式 JSON；收到 HTML 网页会提示实际请求地址，避免误报为模型回答的 JSON 格式错误。

OpenAI 兼容模式可选择 Chat Completions 或 Responses，以及模型支持的思考等级。旧配置默认 Chat Completions、思考等级为“服务商默认”（不传参数），不会自动切换接口或提升等级。Responses 使用 `reasoning.effort`，Chat Completions 使用 `reasoning_effort`；不支持的参数会显示服务商错误。Claude 使用 Messages 接口，可独立配置自适应思考、手动预算或关闭（以模型支持为准），思考等级通过 `output_config.effort` 发送；手动预算为 1024–15999 tokens，总输出上限保持 16000。旧配置不额外传参，保留服务商默认行为。地址可填写服务商基础地址或完整端点；模型输出不完整或拒绝生成时不执行交易。

AI 支持 Anthropic 和 OpenAI 兼容接口。分析间隔可自定义为 1–1440 分钟，另可设置计划时间后的延迟秒数（必须小于间隔）；默认每 4 小时、延迟 180 秒。按固定时间轴对齐，下次运行时间显示在页面；修改后从下一个计划时间生效，不立即补跑。行情使用 4 小时已收盘 K 线。模型请求期间不锁住止损线程，执行前重新读取风控状态；分析未结束时不会并发运行另一轮。

所有 API 先 `POST /api/auth/login`（JSON `username` / `password`）获取会话 Cookie。写请求必须带与 `TIDE_PUBLIC_ORIGIN` 相同的 `Origin`。Basic Auth 已移除。

- `GET /api/ai/snapshot`：行情和持仓快照。
- `POST /api/ai/order`：`{"action":"open_long|open_short|close","pair":"BTC/USDT","reason":"…"}`。
- `POST /api/ai/run?execute=false`：分析；`true` 会按风控执行。
- `GET /api/orders/pending`：待核对订单。结果不确定时不要手动重复下单。
- `GET /api/ai/log`：决策记录。

Rust 程序同时提供 MCP stdio 模式，不需要 Python：

```bash
claude mcp add tide-trade -e PANEL_PASS=后台密码 -- /path/to/tide mcp
```

可选环境变量：`PANEL_URL`、`PANEL_USER`。MCP 客户端自动登录、保存会话并发送正确的 Origin。Linux 发布包用于 Linux；Windows 本地 MCP 需要通过 WSL 运行，或用 Cargo 构建对应平台二进制。
