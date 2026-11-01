"""Freqtrade 管理后台：策略与选币（现货/合约、杠杆、做空）、回测、Bybit Key、Telegram、实盘资金上限。登录密码由 Caddy 负责。"""
import base64
import json
import sys
import math
import os
import subprocess
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

import ccxt
import uvicorn
from fastapi import FastAPI, HTTPException
from fastapi.responses import FileResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel

UD = "/freqtrade/user_data"
CFG = f"{UD}/config.json"
PRIV = f"{UD}/config-private.json"
FT = "http://freqtrade:8080/api/v1"
SERVER_IP = os.environ.get("SERVER_IP", "")
MIN_STAKE = 10       # 每笔至少 10 USDT（合约是保证金）：Bybit 下限 5 USDT，留出止损和手续费的余量
MAX_PAIRS = 12
MAX_OPEN = 5
# 价格反向 8% 止损；超过 10 倍时，价格走不到 8% 就先爆仓，止损失效
MAX_LEVERAGE = 10
PRICE_STOP = 0.08
ADX_MIN = 20         # 与 TrendFollow.adx_min 默认值一致
# 稳定币和 USDT 几乎没有波动，趋势策略在它们身上只会白交手续费
STABLE = {"USDC", "USDE", "USD1", "RLUSD", "FDUSD", "DAI", "PYUSD", "TUSD", "USDD", "BUSD", "USDP", "EURI", "USDR", "USDQ"}

app = FastAPI(docs_url=None, redoc_url=None, openapi_url=None)
app.mount("/fonts", StaticFiles(directory="/panel/fonts"), name="fonts")


def min_capital(max_open):
    """机器人至少要能动用多少 USDT，保证每笔都不低于 MIN_STAKE"""
    return math.ceil(MIN_STAKE * max_open / 0.95)


def load(path):
    try:
        with open(path) as f:
            return json.load(f)
    except FileNotFoundError:
        return {}


def save(path, data):
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(data, f, indent=2, ensure_ascii=False)
    os.chmod(tmp, 0o600)
    os.replace(tmp, path)


def ft(path, method="GET", body=None):
    """调用 Freqtrade 自带的 REST API"""
    a = load(CFG)["api_server"]
    auth = base64.b64encode(f"{a['username']}:{a['password']}".encode()).decode()
    req = urllib.request.Request(
        FT + path, method=method,
        data=json.dumps(body).encode() if body is not None else (b"" if method == "POST" else None),
        headers={"Content-Type": "application/json", "Authorization": f"Basic {auth}"},
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            return json.load(r)
    except urllib.error.HTTPError as e:
        try:
            body = json.load(e)
            detail = body.get("error") or body.get("detail")
        except Exception:
            detail = None
        raise HTTPException(400, f"机器人返回错误：{detail or e.code}")
    except urllib.error.URLError:
        raise HTTPException(503, "机器人没有响应，可能正在重启，稍等再试")


def reload_bot():
    """让机器人重新读取配置文件；原来是暂停状态的，重载后保持暂停"""
    try:
        was_stopped = ft("/show_config")["state"] == "stopped"
    except Exception:
        was_stopped = False
    try:
        ft("/reload_config", "POST")
    except Exception:
        pass
    time.sleep(3)
    for _ in range(60):
        try:
            if ft("/show_config")["state"] in ("running", "stopped"):
                break
        except Exception:
            pass
        time.sleep(2)
    if was_stopped:
        ft("/stop", "POST")


def mask(s):
    return f"{s[:4]}…{s[-4:]}" if s else None


def market_of(cfg):
    return "futures" if cfg.get("trading_mode") == "futures" else "spot"


def db_url(market):
    # 现货、合约各用一个交易库，互不混淆
    name = "live" + (".futures" if market == "futures" else "")
    return f"sqlite:////freqtrade/user_data/tradesv3.{name}.sqlite"


def open_trades():
    try:
        return ft("/status")
    except HTTPException:
        return []


# ---------- Bybit ----------

def check_key(key, secret, market):
    b = ccxt.bybit({"apiKey": key, "secret": secret,
                    "options": {"defaultType": "swap" if market == "futures" else "spot"}})
    try:
        info = b.privateGetV5UserQueryApi()["result"]
        total = b.fetch_balance().get("total") or {}
    except ccxt.AuthenticationError:
        raise HTTPException(400, "Key 或 Secret 不对，请重新复制")
    except ccxt.PermissionDenied:
        raise HTTPException(400, f"Bybit 拒绝访问：这个 Key 绑定的 IP 不是本服务器，请把 IP 白名单改成 {SERVER_IP}")
    except Exception as e:
        raise HTTPException(400, f"连接 Bybit 失败：{e}")
    perms = info.get("permissions") or {}
    ips = info.get("ips") or []
    contract = perms.get("ContractTrade") or []
    return {
        "withdraw": "Withdraw" in json.dumps(perms),
        "spot": "SpotTrade" in (perms.get("Spot") or []),
        "futures": "Order" in contract and "Position" in contract,
        "read_only": str(info.get("readOnly")) == "1",
        "unified": str(info.get("uta")) == "1",
        "ip_bound": bool(ips) and ips != ["*"],
        "ips": ips,
        "usdt": float(total.get("USDT") or 0),
        "balance": {c: v for c, v in total.items() if v},
    }


def need_perm(r, market):
    if r["read_only"]:
        return "这个 Key 是只读的，机器人没法下单"
    if market == "futures" and not r["futures"]:
        return "这个 Key 没有合约交易权限。请在 Bybit 给它勾上「合约 - 订单」和「合约 - 持仓」"
    if market == "spot" and not r["spot"]:
        return "这个 Key 没有现货交易权限。请在 Bybit 给它勾上「现货 - 交易」"
    return None


class KeyIn(BaseModel):
    key: str
    secret: str


@app.post("/api/key")
def set_key(k: KeyIn):
    key, secret = k.key.strip(), k.secret.strip()
    if not key or not secret:
        raise HTTPException(400, "Key 和 Secret 都要填")
    market = market_of(load(CFG))
    r = check_key(key, secret, market)
    if r["withdraw"]:
        raise HTTPException(400, "这个 Key 开了提现权限，为了安全不保存。请到 Bybit 删掉它，重新创建一个不带提现权限的 Key")
    if msg := need_perm(r, market):
        raise HTTPException(400, msg)
    priv = load(PRIV)
    priv["exchange"] = {"key": key, "secret": secret}
    save(PRIV, priv)
    reload_bot()
    return r


# ---------- Telegram ----------

def tg(token, method, **params):
    req = urllib.request.Request(
        f"https://api.telegram.org/bot{token}/{method}",
        data=json.dumps(params).encode(), headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            return json.load(r)["result"]
    except urllib.error.HTTPError as e:
        try:
            desc = json.load(e).get("description", "")
        except Exception:
            desc = str(e)
        if "Unauthorized" in desc or e.code == 404:
            raise HTTPException(400, "Token 不对，请从 @BotFather 重新复制完整的 Token")
        raise HTTPException(400, f"Telegram 返回错误：{desc}")
    except urllib.error.URLError as e:
        raise HTTPException(400, f"连不上 Telegram：{e.reason}")


class TgIn(BaseModel):
    token: str


@app.post("/api/telegram")
def set_tg(t: TgIn):
    token = t.token.strip()
    if ":" not in token:
        raise HTTPException(400, "Token 格式不对，应该像 123456789:AAH... 这样")
    me = tg(token, "getMe")
    priv = load(PRIV)
    old = priv.get("telegram", {})
    if old.get("token") == token and old.get("chat_id"):
        chat_id = old["chat_id"]
    else:
        chats = {}
        for u in tg(token, "getUpdates"):
            chat = (u.get("message") or u.get("edited_message") or {}).get("chat", {})
            if chat.get("type") == "private":
                chats[chat["id"]] = chat.get("username") or chat.get("first_name")
        if not chats:
            raise HTTPException(400, f"还没收到你的消息。请在 Telegram 打开 @{me['username']}，点 Start，然后再点连接")
        if len(chats) > 1:
            raise HTTPException(400, "有不止一个人给这个 Bot 发过消息。别人能通过 Bot 控制机器人，请新建一个只有你用的 Bot")
        chat_id = next(iter(chats))
    tg(token, "sendMessage", chat_id=chat_id, text="✅ 已连接 Freqtrade，开仓、平仓会推送到这里。")
    priv["telegram"] = {"enabled": True, "token": token, "chat_id": str(chat_id)}
    save(PRIV, priv)
    reload_bot()
    return {"bot": me["username"]}


@app.post("/api/telegram/test")
def test_tg():
    t = load(PRIV).get("telegram", {})
    if not t.get("enabled"):
        raise HTTPException(400, "还没设置 Telegram")
    tg(t["token"], "sendMessage", chat_id=t["chat_id"], text="🔔 测试消息：通知正常。")
    return {}


@app.delete("/api/telegram")
def del_tg():
    priv = load(PRIV)
    priv.pop("telegram", None)
    save(PRIV, priv)
    reload_bot()
    return {}


# ---------- 资金上限 ----------

class CapitalIn(BaseModel):
    capital: int | None = None      # 不填 = 用账户里全部 USDT


@app.post("/api/capital")
def set_capital(m: CapitalIn):
    cfg = load(CFG)
    n = cfg.get("max_open_trades", 3)
    if m.capital:
        if m.capital < min_capital(n):
            raise HTTPException(400, f"同时持有 {n} 笔时，至少要 {min_capital(n)} USDT，"
                                     f"这样每笔才有 {MIN_STAKE} USDT 以上（Bybit 每笔最少 5 USDT）")
        ex = load(PRIV).get("exchange", {})
        if ex.get("key"):
            r = check_key(ex["key"], ex["secret"], market_of(cfg))
            if m.capital > r["usdt"]:
                raise HTTPException(400, f"账户里只有 {r['usdt']:.2f} USDT，少于你填的 {m.capital}")
        cfg["available_capital"] = m.capital
    else:
        cfg.pop("available_capital", None)
    save(CFG, cfg)
    reload_bot()
    return {}


@app.post("/api/bot/{action}")
def bot(action: str):
    if action not in ("start", "stop"):
        raise HTTPException(404)
    return ft(f"/{action}", "POST")


@app.post("/api/trades/{trade_id}/exit")
def exit_trade(trade_id: int):
    return ft("/forceexit", "POST", {"tradeid": trade_id})


# ---------- 币种 ----------

_markets = {}


def markets(market):
    """Bybit 的 USDT 现货或 USDT 永续合约（去掉稳定币），按 24 小时成交额排序，缓存 10 分钟"""
    c = _markets.get(market)
    if not c or time.time() - c["t"] > 600:
        futures = market == "futures"
        b = ccxt.bybit({"options": {"defaultType": "swap" if futures else "spot"}})
        b.load_markets()
        tickers = b.fetch_tickers(params={"category": "linear"} if futures else {})
        rows = []
        for m in b.markets.values():
            ok = (m.get("swap") and m.get("linear") and m["settle"] == "USDT") if futures else m["spot"]
            if not (ok and m["quote"] == "USDT" and m["active"]) or m["base"] in STABLE:
                continue
            t = tickers.get(m["symbol"], {})
            rows.append({"pair": m["symbol"], "base": m["base"], "price": t.get("last"),
                         "change": t.get("percentage"), "volume": t.get("quoteVolume") or 0,
                         "max_lev": (m.get("limits", {}).get("leverage") or {}).get("max")})
        rows.sort(key=lambda r: -r["volume"])
        c = _markets[market] = {"t": time.time(), "rows": rows}
    return c["rows"]


@app.get("/api/markets")
def get_markets(market: str = "spot"):
    if market not in ("spot", "futures"):
        raise HTTPException(400, "market 只能是 spot 或 futures")
    try:
        return markets(market)
    except Exception as e:
        raise HTTPException(503, f"读取 Bybit 币种列表失败：{e}")


class StrategyIn(BaseModel):
    market: str
    pairs: list[str]
    max_open: int
    leverage: int = 1
    short: bool = False


def validate_strategy(s: StrategyIn):
    if s.market not in ("spot", "futures"):
        raise HTTPException(400, "市场只能是现货或合约")
    pairs = list(dict.fromkeys(x.strip().upper() for x in s.pairs if x.strip()))
    stable = [x for x in pairs if x.split("/")[0] in STABLE]
    if stable:
        raise HTTPException(400, f"{'、'.join(stable)} 是稳定币，价格基本不动，趋势策略在上面只会白交手续费")
    known = {r["pair"]: r for r in markets(s.market)}
    bad = [x for x in pairs if x not in known]
    if bad:
        where = "USDT 永续合约" if s.market == "futures" else "现货"
        raise HTTPException(400, f"Bybit {where}没有这些交易对：{'、'.join(bad)}")
    if not pairs:
        raise HTTPException(400, "至少选一个币")
    if len(pairs) > MAX_PAIRS:
        raise HTTPException(400, f"最多选 {MAX_PAIRS} 个币")
    if not 1 <= s.max_open <= MAX_OPEN:
        raise HTTPException(400, f"同时持仓数要在 1 到 {MAX_OPEN} 之间")
    lev, short = (s.leverage, s.short) if s.market == "futures" else (1, False)
    if not 1 <= lev <= MAX_LEVERAGE:
        raise HTTPException(400, f"杠杆要在 1 到 {MAX_LEVERAGE} 倍之间。超过 {MAX_LEVERAGE} 倍时，价格还没走到 8% 止损就会先爆仓")
    low = [x for x in pairs if (known[x]["max_lev"] or MAX_LEVERAGE) < lev]
    if low:
        raise HTTPException(400, f"{'、'.join(low)} 在 Bybit 最多只能开 {min(known[x]['max_lev'] for x in low):g} 倍")
    return pairs, lev, short


@app.post("/api/strategy")
def set_strategy(s: StrategyIn):
    pairs, lev, short = validate_strategy(s)
    cfg = load(CFG)
    cur_market = market_of(cfg)
    held = open_trades()
    if held and (s.market != cur_market or lev != cfg.get("panel_leverage", 1)
                 or short != cfg.get("panel_short", False)):
        raise HTTPException(400, f"还有 {len(held)} 笔持仓。改市场、杠杆或做空之前，先在「持仓」里把它们平掉。"
                                 "只改币种和持仓数不受影响")
    if s.market != cur_market:
        # 换市场前确认 Key 有新市场的交易权限，否则机器人切过去就没法下单
        ex = load(PRIV).get("exchange", {})
        if not ex.get("key"):
            raise HTTPException(400, "先在「账户与资金」里保存 Bybit API Key")
        if msg := need_perm(check_key(ex["key"], ex["secret"], s.market), s.market):
            raise HTTPException(400, msg)
    if "available_capital" in cfg and cfg["available_capital"] < min_capital(s.max_open):
        raise HTTPException(400, f"资金上限 {cfg['available_capital']} USDT 不够分成 {s.max_open} 笔，"
                                 f"至少要 {min_capital(s.max_open)} USDT。请减少同时持仓数，或在「账户与资金」里提高上限")
    futures = s.market == "futures"
    cfg["trading_mode"] = s.market
    cfg["margin_mode"] = "isolated" if futures else ""
    cfg["panel_leverage"] = lev
    cfg["panel_short"] = short
    cfg["exchange"]["pair_whitelist"] = pairs
    cfg["max_open_trades"] = s.max_open
    cfg["db_url"] = db_url(s.market)
    save(CFG, cfg)
    reload_bot()
    return {}


def watch(pairs, open_by_pair, timeframe, short_on):
    """每个币离 TrendFollow 的开仓条件还差多少"""
    out = []
    for pair in pairs:
        row = {"pair": pair}
        try:
            r = ft(f"/pair_candles?pair={urllib.parse.quote(pair)}&timeframe={timeframe}&limit=2")
            last = dict(zip(r["columns"], r["data"][-1]))
        except Exception:
            out.append({**row, "state": "loading"})
            continue
        close, e20, e50, e200, adx = (last.get(k) for k in ("close", "ema20", "ema50", "ema200", "adx"))
        row["price"] = close
        if None in (close, e20, e50, e200, adx):
            out.append({**row, "state": "nodata"})
            continue
        up = close > e200
        row.update(
            side="long" if up else "short",
            above200=up, dist200=(close - e200) / e200,
            golden=e20 > e50, gap=(e20 - e50) / e50,
            adx=adx, trending=adx > ADX_MIN,
        )
        if pair in open_by_pair:
            row["state"] = "holding"
            row["holding_short"] = open_by_pair[pair]
        elif last.get("enter_long") == 1:
            row["state"] = "signal_long"
        elif last.get("enter_short") == 1 and short_on:
            row["state"] = "signal_short"
        elif not up and not short_on:
            row["state"] = "below"
        elif up == row["golden"]:
            row["state"] = "missed"
        else:
            row["state"] = "waiting"
        out.append(row)
    return out


# ---------- 回测 ----------

_bt_lock = threading.Lock()


@app.post("/api/backtest")
def backtest(s: StrategyIn, wallet: int = 1000):
    pairs, lev, short = validate_strategy(s)
    if not _bt_lock.acquire(blocking=False):
        raise HTTPException(409, "已经有一个回测在跑，等它结束")
    try:
        r = subprocess.run(
            ["python", "/panel/backtest.py", json.dumps({
                "market": s.market, "pairs": pairs, "max_open": s.max_open,
                "leverage": lev, "short": short, "wallet": max(wallet, 100)})],
            capture_output=True, text=True, timeout=300, stdin=subprocess.DEVNULL, cwd="/freqtrade",
        )
    except subprocess.TimeoutExpired:
        raise HTTPException(504, "回测超过 5 分钟没跑完，少选几个币再试")
    finally:
        _bt_lock.release()
    for line in reversed(r.stdout.splitlines()):
        if line.startswith("RESULT "):
            return json.loads(line[7:])
    err = (r.stderr or r.stdout).strip().splitlines()
    raise HTTPException(500, "回测失败：" + (err[-1] if err else "没有输出"))


# ---------- 读数据 ----------

@app.get("/api/trades")
def trades(offset: int = 0, limit: int = 50):
    return ft(f"/trades?limit={min(max(limit, 1), 200)}&offset={max(offset, 0)}")


@app.get("/api/logs")
def logs(limit: int = 200):
    return ft(f"/logs?limit={min(max(limit, 1), 500)}")


@app.get("/api/overview")
def overview():
    cfg, priv = load(CFG), load(PRIV)
    t = priv.get("telegram", {})
    market = market_of(cfg)
    out = {
        "server_ip": SERVER_IP,
        "key": mask(priv.get("exchange", {}).get("key")),
        "telegram": bool(t.get("enabled") and t.get("chat_id")),
        "capital": cfg.get("available_capital"),
        "market": market,
        "leverage": cfg.get("panel_leverage", 1) if market == "futures" else 1,
        "short": bool(cfg.get("panel_short")) and market == "futures",
        "pairs": cfg.get("exchange", {}).get("pair_whitelist", []),
        "max_open": cfg.get("max_open_trades", 3),
        "min_stake": MIN_STAKE,
        "max_pairs": MAX_PAIRS,
        "max_open_limit": MAX_OPEN,
        "max_leverage": MAX_LEVERAGE,
        "price_stop": PRICE_STOP,
        "adx_min": ADX_MIN,
        "ai": bool(cfg.get("panel_ai")),
    }
    out["min_capital"] = min_capital(out["max_open"])
    try:
        c = ft("/show_config")
        out.update(state=c["state"], strategy=c["strategy"], timeframe=c["timeframe"])
        out["open"] = ft("/status")
        out["watch"] = watch(out["pairs"], {x["pair"]: x.get("is_short", False) for x in out["open"]},
                             c["timeframe"], out["short"])
        out["profit"] = ft("/profit")
        out["daily"] = ft("/daily?timescale=30")["data"]
        out["recent"] = ft("/trades?limit=5")["trades"]
        b = ft("/balance")
        out["balance"] = {"total": b.get("total_bot") or b.get("total"), "symbol": b.get("stake")}
    except Exception as e:
        out.update(state="offline", error=str(e))
    return out


# AI 交易（ai.py）：行情快照、大模型决策、硬性风控下单
# ---------- Bybit 账户实际持仓（含手动开的仓位，只读） ----------

_hold = {"t": 0, "d": None}


def fnum(x):
    try:
        return float(x)
    except (TypeError, ValueError):
        return 0.0


@app.get("/api/holdings")
def holdings():
    if _hold["d"] and time.time() - _hold["t"] < 8:
        return _hold["d"]
    ex = load(PRIV).get("exchange", {})
    if not ex.get("key"):
        raise HTTPException(400, "还没保存 Bybit API Key")
    b = ccxt.bybit({"apiKey": ex["key"], "secret": ex["secret"]})
    try:
        w = b.privateGetV5AccountWalletBalance({"accountType": "UNIFIED"})["result"]["list"][0]
        pos = b.privateGetV5PositionList({"category": "linear", "settleCoin": "USDT", "limit": 50})["result"]["list"]
        ords = []
        for cat, extra in (("linear", {"settleCoin": "USDT"}), ("spot", {})):
            ords += [{**o, "category": cat} for o in
                     b.privateGetV5OrderRealtime({"category": cat, "limit": 50, **extra})["result"]["list"]]
    except ccxt.PermissionDenied:
        raise HTTPException(400, "Bybit 拒绝访问，检查 Key 的 IP 白名单和权限")
    except Exception as e:
        raise HTTPException(502, f"读取 Bybit 失败：{e}")
    coins = [{"coin": c["coin"], "amount": fnum(c["walletBalance"]), "usd": fnum(c["usdValue"]),
              "locked": fnum(c.get("locked"))}
             for c in w["coin"] if fnum(c["walletBalance"]) > 0]
    positions = []
    for p in pos:
        if fnum(p["size"]) <= 0:
            continue
        mark, liq = fnum(p["markPrice"]), fnum(p["liqPrice"])
        positions.append({
            "symbol": p["symbol"], "side": "long" if p["side"] == "Buy" else "short", "size": fnum(p["size"]),
            "leverage": fnum(p["leverage"]), "entry": fnum(p["avgPrice"]), "mark": mark, "liq": liq,
            "liq_dist_pct": abs(mark - liq) / mark * 100 if liq and mark else None,
            "value": fnum(p["positionValue"]), "margin": fnum(p["positionIM"]), "pnl": fnum(p["unrealisedPnl"]),
            "tp": fnum(p.get("takeProfit")) or None, "sl": fnum(p.get("stopLoss")) or None,
        })
    orders = [{"symbol": o["symbol"], "category": o["category"], "side": o["side"], "type": o["orderType"],
               "qty": fnum(o["qty"]), "price": fnum(o["price"]), "trigger": fnum(o.get("triggerPrice")) or None,
               "status": o["orderStatus"], "stop_type": o.get("stopOrderType") or None} for o in ords]
    d = {"equity": fnum(w["totalEquity"]), "available": fnum(w.get("totalAvailableBalance")),
         "coins": coins, "positions": positions, "orders": orders}
    _hold.update(t=time.time(), d=d)
    return d


import ai  # noqa: E402

ai.init(sys.modules[__name__])
app.include_router(ai.router)


@app.get("/")
def index():
    return FileResponse("/panel/index.html", headers={"Cache-Control": "no-store"})


if __name__ == "__main__":
    uvicorn.run(app, host="0.0.0.0", port=8000, log_level="warning")
