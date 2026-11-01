"""在 Freqtrade 容器里跑一次回测，最后一行输出 RESULT {json}。
参数：JSON {market: spot|futures, pairs, max_open, leverage, short, wallet}"""
import json
import logging
import os
import subprocess
import sys
import tempfile
from datetime import datetime, timedelta, timezone

logging.disable(logging.WARNING)
os.chdir("/freqtrade")  # Freqtrade 按当前目录找 user_data

from freqtrade.configuration import Configuration  # noqa: E402
from freqtrade.enums import RunMode  # noqa: E402
from freqtrade.optimize.backtesting import Backtesting  # noqa: E402

p = json.loads(sys.argv[1])
futures = p.get("market") == "futures"
TIMEFRAME = "4h"
now = datetime.now(timezone.utc)
start = (now - timedelta(days=730)).strftime("%Y%m%d")
dl_start = (now - timedelta(days=730 + 40)).strftime("%Y%m%d")  # 预留 EMA200 的预热数据

# 回测用的覆盖配置：市场类型、杠杆、做空
override = {
    "trading_mode": "futures" if futures else "spot",
    "margin_mode": "isolated" if futures else "",
    "panel_leverage": p.get("leverage", 1) if futures else 1,
    "panel_short": bool(p.get("short")) if futures else False,
    "exchange": {"pair_whitelist": p["pairs"]},
}
with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
    json.dump(override, f)
CONFIG = ["/freqtrade/user_data/config.json", f.name]

try:
    # 只补缺的数据；合约会顺带下载资金费率和标记价格
    subprocess.run(["freqtrade", "download-data", "--config", CONFIG[0], "--config", CONFIG[1],
                    "--timeframes", TIMEFRAME, "--timerange", f"{dl_start}-", "--pairs", *p["pairs"]],
                   check=True, capture_output=True, stdin=subprocess.DEVNULL)

    cfg = Configuration({
        "config": CONFIG, "strategy": "TrendFollow", "timeframe": TIMEFRAME, "timerange": f"{start}-",
        "fee": 0.0006 if futures else 0.001, "export": "none", "pairs": p["pairs"],
        "max_open_trades": p["max_open"], "stake_amount": "unlimited", "dry_run_wallet": p["wallet"],
    }, RunMode.BACKTEST).get_config()
    bt = Backtesting(cfg)
    bt.start()
finally:
    os.unlink(f.name)

r = bt.results["strategy"]["TrendFollow"]
pairs = [{
    "pair": x["key"], "trades": x["trades"], "profit": x["profit_total"],
    "profit_abs": x["profit_total_abs"], "winrate": x.get("winrate"),
} for x in r["results_per_pair"] if x["key"] != "TOTAL"]
exits = {x["key"]: x["trades"] for x in r.get("exit_reason_summary", []) if x["key"] != "TOTAL"}
curve, total = [], p["wallet"]
for d in r.get("daily_profit") or []:
    total += d[1]
    curve.append([d[0], round(total, 2)])

print("RESULT " + json.dumps({
    "start": r["backtest_start"][:10], "end": r["backtest_end"][:10],
    "wallet": p["wallet"], "final": r["final_balance"],
    "profit": r["profit_total"], "max_drawdown": r["max_drawdown_account"],
    "trades": r["total_trades"], "winrate": r.get("winrate"),
    "long_abs": r.get("profit_total_long_abs"), "short_abs": r.get("profit_total_short_abs"),
    "liquidations": exits.get("liquidation", 0), "exits": exits,
    "market_change": r["market_change"], "pairs": pairs, "curve": curve,
}, default=str))
