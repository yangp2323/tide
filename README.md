# Tide · Bybit 量化交易

Bybit 自动交易（现货或 USDT 永续合约），全部跑在东京服务器上（38.175.201.84），电脑关机不影响。

## 管理后台（随时可访问，手机也能用）

**https://trade.juyouagent.com**（juyouagent.com 也能打开；必须输入 https://，用 IP 访问会被拒绝），登录账号密码在 `deploy/secrets/服务器信息.md`

左侧菜单：

| 页面 | 做什么 |
| --- | --- |
| 概览 | 当前状态、距离 K 线收盘的倒计时、账户和收益、每个币离开仓还差什么、近 30 天每日盈亏 |
| 持仓 | 当前持仓（合约会显示杠杆、爆仓价、止损价），可以手动市价平仓 |
| 交易记录 | 全部已平仓交易，分页 |
| AI 交易 | 让大模型（Claude 或 OpenAI 兼容接口）每根 K 线收盘后分析并开平仓，带硬性风控和决策记录 |
| 策略与选币 | 现货 / USDT 永续合约、杠杆 1–10 倍、是否做空、同时持仓数、从 Bybit 选币（最多 12 个）、回测近两年 |
| 账户与资金 | Bybit API Key、机器人最多可动用的 USDT |
| 通知 | Telegram |
| 运行日志 | 机器人最近 300 条日志 |

- **只跑实盘**（模拟盘已移除）：机器人直接用 Bybit 账户里的真钱交易
- Key 权限：现货只勾「现货 - 交易」，合约勾「合约 - 订单」和「合约 - 持仓」；都不要勾提现，IP 白名单填服务器 IP
- 资金上限选填，不填就用账户里全部 USDT；都要保证每笔至少 10 USDT（同时持仓 3 笔时至少 32 USDT），余额不够时机器人不会开仓
- 合约用**逐仓**：每笔保证金独立，一笔爆仓不牵连其他仓位。止损单挂在 Bybit 上，机器人停了也有效
- 现货的止损由机器人执行（Freqtrade 不支持 Bybit 现货交易所止损单），机器人暂停时不会止损
- 有持仓时不能改市场、杠杆、做空，要先平仓

## 策略

`strategies/TrendFollow.py`：4 小时趋势跟踪，默认 BTC/ETH/SOL 现货只做多（后台里可改）。
- 做多：价格在 200 均线上方，20 均线上穿 50 均线，ADX > 20；死叉平仓
- 做空（仅合约、需开启）：价格在 200 均线下方，20 均线下穿 50 均线，ADX > 20；金叉平仓
- 止损和移动止盈按**价格**算：不论几倍杠杆，价格反向 8% 止损；盈利 6% 后启动 3% 移动止盈。所以 3 倍杠杆一次止损亏掉保证金的 24%
- 杠杆上限 10 倍：再高的话价格还没走到 8% 就先爆仓

回测（BTC/ETH/SOL，近两年，本金 1000，最多 3 笔；现货手续费 0.1%，合约 0.06% 并含资金费率）：

| 方式 | 收益 | 最大回撤 |
| --- | --- | --- |
| 现货只做多 | +6.97% | 6.2% |
| 合约 1× 只做多 | +4.06% | 7.3% |
| 合约 2× 只做多 | +8.57% | 14.6% |
| 合约 3× 只做多 | +10.72% | 21.8% |
| 合约 5× 只做多 | +13.85% | 34.4% |
| 合约 1× 多空 | -1.68% | 17.1% |
| 合约 3× 多空 | -7.88% | 46.0% |
| 合约 5× 多空 | -24.40% | 66.6% |
| 合约 10× 多空 | -72.20% | 92.6% |

做空部分整体亏钱；杠杆越高，收益涨得慢、回撤涨得快。加更多小币也会变差（现货主流 8 个 -0.58%，成交额前 10 个 -7.03%）。样本少，不代表未来。

## AI 交易

后台「AI 交易」页开启后，`TrendFollow` 的均线信号停用，改由大模型决定开仓、平仓（`deploy/freqtrade/panel/ai.py`）：
- 每 4 小时（或每小时）K 线收盘 3 分钟后，把账户、持仓、每个币最近 60 根 K 线和指标发给大模型，它返回 JSON 决策，后台用市价单执行
- **硬性风控**（代码里检查，大模型绕不过）：只能交易已选的币、持仓上限、每天开仓次数上限、当日亏损达到上限停止开仓、低于置信度不执行、现货不能做空
- 每笔金额、杠杆、价格反向 8% 止损、移动止盈仍由系统执行
- 决策记录存在服务器 `user_data/ai/decisions.jsonl`，配了 Telegram 会推送

**外部接入**（都要后台登录账号，Basic Auth）：
- `GET /api/ai/snapshot` 行情快照、`POST /api/ai/order` 下单（`{"action": "open_long|open_short|close", "pair": "BTC/USDT", "reason": "…"}`，同样过风控）、`POST /api/ai/run?execute=false` 让服务器上的模型分析一次、`GET /api/ai/log` 决策记录
- MCP（给 Claude Code / Claude Desktop 用）：`mcp/trade_mcp.py`，需要本地 `pip install mcp httpx`
  ```bash
  claude mcp add bybit-trade -e PANEL_PASS=后台密码 -- python D:/coding/tide/mcp/trade_mcp.py
  ```

## 服务器上的结构

`/opt/tide`：Docker 跑五个容器——`tide-bot`（交易机器人）、`tide-panel`（内部 FastAPI：策略、回测、AI）、`tide-web`（Next.js：页面、登录、`/api` 入口）、`tide-caddy`（HTTPS）、`tide-sni`（443 端口按域名分流：juyouagent.com 走控制台，其余走 sing-box 代理）。
Key 和 Telegram 只存在服务器 `user_data/config-private.json`（权限 600）。

公网只认 Next.js：Caddy 反代到 `web:3000`，登录态是 HMAC 签名 Cookie（`__Host-tide_session`，12 小时）；`web` 的 Route Handler 带内部令牌转发给 `panel:8000`，Python 服务不对外暴露。

重新部署（`node_modules` 有几百 MB，用 tar 打包再传）：
```bash
tar -czf deploy.tgz --exclude=node_modules --exclude=.next --exclude=out -C deploy/freqtrade . \
  && scp deploy.tgz strategies/TrendFollow.py root@38.175.201.84:/root/ \
  && ssh root@38.175.201.84 "tar -xzf /root/deploy.tgz -C /opt/tide && mv -f /root/TrendFollow.py /opt/tide/ && rm -f /root/deploy.tgz && bash /opt/tide/setup.sh"
```

`setup.sh` 可重复执行：不会覆盖 `.env`、Key、交易记录；`web` 是挂载源码 + `next dev`，只改页面时直接改服务器 `/opt/tide/web` 就会热更新，不用重启。
