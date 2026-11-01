"""本地预览：把 out/ 静态站点和一套假数据一起跑起来，不连服务器就能看界面。

    pnpm build
    python tools/preview.py            # 默认 http://127.0.0.1:8123
    python tools/preview.py 9000

只用于看样式和交互，所有 /api 返回的都是写死的样例数据，下单类接口一律返回 {}。
"""
import json
import mimetypes
import os
import sys
from datetime import datetime, timedelta
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "out")

PAIRS = ["BTC/USDT:USDT", "ETH/USDT:USDT", "SOL/USDT:USDT", "XRP/USDT:USDT"]


def overview():
    today = datetime.now().date()
    daily = []
    for i in range(30):  # 和 freqtrade /daily 一样：最新的在前
        d = today - timedelta(days=i)
        profit = round(((i * 37) % 11 - 4) * 1.7, 2)
        daily.append({"date": d.isoformat(), "abs_profit": profit, "trade_count": 1 if profit else 0})
    return {
        "server_ip": "203.0.113.42",
        "key": "aBcd…wXyZ",
        "telegram": True,
        "capital": 300,
        "market": "futures",
        "leverage": 3,
        "short": True,
        "pairs": PAIRS,
        "max_open": 3,
        "min_stake": 10,
        "max_pairs": 12,
        "max_open_limit": 5,
        "max_leverage": 10,
        "price_stop": 0.08,
        "adx_min": 20,
        "ai": True,
        "min_capital": 32,
        "state": "running",
        "strategy": "TrendFollow",
        "timeframe": "4h",
        "open": [{
            "trade_id": 1, "pair": "BTC/USDT:USDT", "is_short": False,
            "open_date": "2026-10-31T08:00:00", "open_rate": 68250.5, "current_rate": 70120.3,
            "liquidation_price": 46500.2, "stop_loss_abs": 62790.5, "stake_amount": 100,
            "profit_abs": 2.74, "profit_ratio": 0.0274, "leverage": 3,
        }],
        "watch": [
            {"pair": "BTC/USDT:USDT", "price": 70120.3, "state": "holding", "above200": True,
             "dist200": 0.082, "golden": True, "gap": 0.014, "adx": 28.4, "trending": True,
             "holding_short": False},
            {"pair": "ETH/USDT:USDT", "price": 3521.8, "state": "waiting", "above200": True,
             "dist200": 0.041, "golden": False, "gap": -0.006, "adx": 24.1, "trending": True},
            {"pair": "SOL/USDT:USDT", "price": 182.4, "state": "signal_long", "above200": True,
             "dist200": 0.031, "golden": True, "gap": 0.004, "adx": 26.7, "trending": True},
            {"pair": "XRP/USDT:USDT", "price": 2.413, "state": "missed", "above200": False,
             "dist200": -0.02, "golden": False, "gap": -0.011, "adx": 18.2, "trending": False},
        ],
        "profit": {
            "profit_closed_coin": 37.42, "profit_closed_ratio_mean": 0.0173, "closed_trade_count": 12,
            "winning_trades": 7, "losing_trades": 5, "max_drawdown": 0.068,
        },
        "daily": daily,
        "recent": [
            {"trade_id": 9, "pair": "SOL/USDT:USDT", "is_short": False,
             "close_date": "2026-10-29T12:00:00", "profit_abs": 4.31, "profit_ratio": 0.043},
            {"trade_id": 8, "pair": "ETH/USDT:USDT", "is_short": True,
             "close_date": "2026-10-27T04:00:00", "profit_abs": -2.18, "profit_ratio": -0.0218},
            {"trade_id": 7, "pair": "BTC/USDT:USDT", "is_short": False,
             "close_date": "2026-10-24T20:00:00", "profit_abs": 6.05, "profit_ratio": 0.0605},
        ],
        "balance": {"total": 412.6, "symbol": "USDT"},
    }


def trades(total=48):
    out = []
    for i in range(30):
        profit = round(((i * 13) % 17 - 6) * 1.3, 2)
        out.append({
            "trade_id": 100 - i, "pair": PAIRS[i % len(PAIRS)], "is_short": i % 3 == 1,
            "trading_mode": "futures", "leverage": 3,
            "open_date": f"2026-09-{(i % 28) + 1:02d}T04:00:00",
            "close_date": f"2026-10-{(i % 28) + 1:02d}T08:00:00",
            "open_rate": 68000 + i * 13, "close_rate": 68100 + i * 13,
            "profit_abs": profit, "profit_ratio": profit / 100,
            "exit_reason": ["exit_signal", "stop_loss", "trailing_stop_loss", "force_exit"][i % 4],
        })
    return {"trades": out, "total_trades": total}


def markets(market="futures"):
    names = ["BTC", "ETH", "SOL", "XRP", "DOGE", "SUI", "PEPE", "AVAX", "LINK", "TON", "ADA", "ARB"]
    rows = []
    for i, n in enumerate(names):
        rows.append({
            "pair": f"{n}/USDT:USDT" if market == "futures" else f"{n}/USDT",
            "base": n, "price": [70120.3, 3521.8, 182.4, 2.413, 0.3124, 3.812, 0.0000112][i % 7],
            "change": ((i * 7) % 11 - 5) * 1.3, "volume": (i + 1) * 3.4e7,
            "max_lev": [100, 100, 50, 50, 25, 25, 10][i % 7],
        })
    return rows


def ai():
    return {
        "config": {
            "provider": "anthropic", "api_key": "sk-a…9f2c", "base_url": "", "model": "claude-opus-5-5",
            "enabled": True, "interval": 240, "min_confidence": 0.6, "max_entries_per_day": 3,
            "max_daily_loss_pct": 5.0,
        },
        "state": {"running": False, "last_run": "2026-11-01 12:03:00",
                  "next_run": "2026-11-01 16:03:00", "last_error": None},
        "entries_today": 1,
        "log": [{
            "time": "2026-11-01 12:03:07", "source": "auto", "model": "claude-opus-5-5", "execute": True,
            "analysis": "BTC 在 200 线上方震荡，4 小时级别均线多头排列，但 ADX 只有 28，动能一般。ETH 刚死叉，暂时不动。",
            "seconds": 8.4,
            "actions": [
                {"action": "open_long", "pair": "SOL/USDT:USDT", "confidence": 0.72, "reason": "SOL 放量上穿 50 线，跟随大盘", "status": "executed"},
                {"action": "open_short", "pair": "ETH/USDT:USDT", "confidence": 0.41, "reason": "跌势但动能不足", "status": "blocked", "note": "置信度 0.41 低于设定的 0.6"},
            ],
        }, {
            "time": "2026-11-01 08:03:02", "source": "mcp", "model": "external", "execute": True,
            "analysis": "手动平仓", "actions": [{"action": "close", "pair": "XRP/USDT:USDT", "confidence": 1, "reason": "达到目标位", "status": "executed"}],
        }],
    }


def holdings():
    return {
        "equity": 412.6, "available": 289.4,
        "coins": [{"coin": "USDT", "amount": 289.4, "usd": 289.4, "locked": 0},
                  {"coin": "BTC", "amount": 0.0012, "usd": 84.1, "locked": 0}],
        "positions": [{"symbol": "BTCUSDT", "side": "long", "size": 0.0043, "leverage": 3,
                       "entry": 68250.5, "mark": 70120.3, "liq": 46500.2, "liq_dist_pct": 33.7,
                       "value": 301.5, "margin": 100, "pnl": 8.04, "tp": None, "sl": 62790.5}],
        "orders": [{"symbol": "BTCUSDT", "category": "linear", "side": "Sell", "type": "Market",
                    "qty": 0.0043, "price": 0, "trigger": 62790.5, "status": "Untriggered",
                    "stop_type": "StopLoss"}],
    }


def logs():
    rows = []
    base = datetime.now() - timedelta(hours=6)
    msg = [
        ("INFO", "Bot heartbeat, state: running"),
        ("INFO", "Checking BTC/USDT:USDT ..."),
        ("WARNING", "Unable to fetch ticker for XRP/USDT:USDT, retrying in 5s"),
        ("INFO", "Entering long for SOL/USDT:USDT at 182.4"),
        ("ERROR", "Order failed: insufficient margin"),
        ("INFO", "Trailing stop updated for BTC/USDT:USDT"),
    ]
    for i in range(60):
        t = base + timedelta(minutes=i * 6)
        level, text = msg[i % len(msg)]
        rows.append([t.strftime("%Y-%m-%d %H:%M:%S"), "freqtrade.worker", 0, level, text])
    return {"logs": rows}


MOCK = {
    "/api/overview": lambda: overview(),
    "/api/trades": lambda: trades(),
    "/api/markets": lambda: markets(),
    "/api/holdings": lambda: holdings(),
    "/api/ai": lambda: ai(),
    "/api/logs": lambda: logs(),
}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def send_json(self, obj):
        body = json.dumps(obj, ensure_ascii=False).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        path = self.path.split("?", 1)[0]
        if path.startswith("/api/"):
            fn = MOCK.get(path)
            if not fn:
                self.send_json({})
                return
            try:
                self.send_json(fn())
            except Exception as e:  # noqa: BLE001
                self.send_json({"detail": f"mock error: {e}"})
            return
        self.send_static(path)

    def do_POST(self):
        self.send_json({})

    def do_DELETE(self):
        self.send_json({})

    def send_static(self, path):
        rel = path.lstrip("/")
        full = os.path.normpath(os.path.join(ROOT, rel))
        if not full.startswith(os.path.abspath(ROOT)):
            self.send_error(403)
            return
        if os.path.isdir(full):
            full = os.path.join(full, "index.html")
        elif not os.path.exists(full) and os.path.exists(full + ".html"):
            full += ".html"
        if not os.path.exists(full):
            self.send_error(404)
            return
        ctype = mimetypes.guess_type(full)[0] or "application/octet-stream"
        with open(full, "rb") as f:
            body = f.read()
        self.send_response(200)
        self.send_header("Content-Type", ctype + ("; charset=utf-8" if ctype.startswith("text/") or ctype.endswith("javascript") else ""))
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8123
    if not os.path.isdir(ROOT):
        sys.exit("先运行 pnpm build 生成 out/")
    print(f"预览地址 http://127.0.0.1:{port}")
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
