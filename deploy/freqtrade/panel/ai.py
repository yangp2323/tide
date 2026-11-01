"""AI 交易：把行情和持仓交给大模型分析，由它决定开仓、平仓。
不论下单来自定时任务、后台按钮还是外部 MCP，都要先过 guard() 的硬性风控，大模型绕不过去。
止损（价格反向 8%）、移动止盈、杠杆、每笔金额仍由 TrendFollow 和 Freqtrade 执行，大模型改不了。"""
import json
import os
import threading
import time
import urllib.parse
from datetime import datetime, timezone

from fastapi import APIRouter, HTTPException
from pydantic import BaseModel

router = APIRouter(prefix="/api/ai")
core = None          # app 模块，由 init() 注入（app.py 以 __main__ 运行，不能反向 import）

LOG_DIR = "/freqtrade/user_data/ai"
LOG = f"{LOG_DIR}/decisions.jsonl"
CANDLES = 60         # 每个币给大模型看最近多少根 K 线
DEFAULTS = {
    "provider": "anthropic", "api_key": "", "base_url": "", "model": "claude-opus-5-5",
    "enabled": False, "interval": 240,
    "min_confidence": 0.6, "max_entries_per_day": 3, "max_daily_loss_pct": 5.0,
}
# 支持 effort 参数、支持服务端 fallbacks 的 Claude 模型
EFFORT_MODELS = ("claude-opus-5", "claude-sonnet-5", "claude-fable-5", "claude-haiku-5")
FALLBACK_MODELS = {"claude-fable-5-1", "claude-opus-5-5", "claude-opus-5", "claude-sonnet-5-5"}

SCHEMA = {
    "type": "object",
    "properties": {
        "analysis": {"type": "string"},
        "actions": {"type": "array", "items": {
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["open_long", "open_short", "close"]},
                "pair": {"type": "string"},
                "confidence": {"type": "number"},
                "reason": {"type": "string"},
            },
            "required": ["action", "pair", "confidence", "reason"],
            "additionalProperties": False,
        }},
    },
    "required": ["analysis", "actions"],
    "additionalProperties": False,
}

SYSTEM = """You are the trading decision-maker for a small automated crypto account on Bybit.
Each run you receive a JSON snapshot: account state, risk limits, open positions, and recent closed candles with indicators for every tradable pair.

Decide what to do right now. Possible actions:
- open_long / open_short: open a new position on a pair (open_short only when "short_allowed" is true)
- close: close an existing open position on a pair
Doing nothing is a valid and often the best answer: return an empty actions list.

Fixed by the system, not by you: position size (equity split evenly across max_open slots), leverage, a hard stop-loss when price moves 8% against the position, and a 3% trailing stop that activates after 6% price profit. Plan around these: an entry needs a realistic path to more than 8% before an 8% adverse move, after fees.

Only use pairs listed in the snapshot. Do not open a pair that already has a position. Respect the remaining slots and entries left today; orders beyond the limits are rejected.
confidence is your probability (0 to 1) that the trade works out; orders below min_confidence are not executed.
Base decisions on the data given. Write "analysis" and every "reason" in Simplified Chinese, concise: market read first, then why you act or wait.
Respond with only the JSON object."""

_run_lock = threading.Lock()
_state = {"running": False, "last_run": None, "next_run": None, "last_error": None}


def init(app_module):
    global core
    core = app_module
    os.makedirs(LOG_DIR, exist_ok=True)
    threading.Thread(target=_scheduler, daemon=True).start()


def conf():
    return {**DEFAULTS, **core.load(core.PRIV).get("ai", {})}


def today():
    return datetime.now(timezone.utc).strftime("%Y-%m-%d")


def read_log(limit=50):
    try:
        with open(LOG, encoding="utf-8") as f:
            lines = f.readlines()
    except FileNotFoundError:
        return []
    out = []
    for line in reversed(lines[-limit:]):
        try:
            out.append(json.loads(line))
        except ValueError:
            pass
    return out


def write_log(rec):
    with open(LOG, "a", encoding="utf-8") as f:
        f.write(json.dumps(rec, ensure_ascii=False) + "\n")


def entries_today():
    """今天（UTC）实际成交的 AI / MCP 开仓次数"""
    n, day = 0, today()
    for r in read_log(500):
        if not r.get("time", "").startswith(day):
            continue
        n += sum(1 for a in r.get("actions", []) if a.get("status") == "executed" and a["action"] != "close")
    return n


def _round(v):
    if isinstance(v, float):
        return float(f"{v:.6g}")
    return v


# ---------- 行情快照 ----------

def snapshot():
    cfg, c = core.load(core.CFG), conf()
    market = core.market_of(cfg)
    futures = market == "futures"
    show = core.ft("/show_config")
    tf = show["timeframe"]
    open_ = core.ft("/status")
    bal = core.ft("/balance")
    daily = core.ft("/daily?timescale=1")["data"]
    pnl_today = daily[0]["abs_profit"] if daily else 0.0
    equity = bal.get("total_bot") or bal.get("total") or 0
    start_equity = equity - pnl_today
    max_open = cfg.get("max_open_trades", 3)

    pairs = []
    for pair in cfg.get("exchange", {}).get("pair_whitelist", []):
        try:
            r = core.ft(f"/pair_candles?pair={urllib.parse.quote(pair)}&timeframe={tf}&limit={CANDLES}")
        except HTTPException:
            pairs.append({"pair": pair, "error": "no data yet"})
            continue
        keep = ["date", "open", "high", "low", "close", "volume", "ema20", "ema50", "ema200", "adx", "rsi", "atr"]
        idx = [r["columns"].index(k) for k in keep if k in r["columns"]]
        cols = [r["columns"][i] for i in idx]
        rows = [[_round(row[i]) if cols[j] != "date" else str(row[i])[:16] for j, i in enumerate(idx)]
                for row in r["data"]]
        pairs.append({"pair": pair, "columns": cols, "candles": rows})

    return {
        "time_utc": datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M"),
        "exchange": "bybit",
        "market": "USDT perpetual futures" if futures else "spot",
        "timeframe": tf,
        "leverage": cfg.get("panel_leverage", 1) if futures else 1,
        "short_allowed": futures and bool(cfg.get("panel_short")),
        "bot_state": show["state"],
        "account": {
            "equity_usdt": _round(float(equity)),
            "realized_pnl_today_usdt": _round(float(pnl_today)),
            "realized_pnl_today_pct": _round(pnl_today / start_equity * 100) if start_equity else 0,
        },
        "risk_limits": {
            "max_open": max_open,
            "slots_left": max(0, max_open - len(open_)),
            "max_entries_per_day": c["max_entries_per_day"],
            "entries_left_today": max(0, c["max_entries_per_day"] - entries_today()),
            "max_daily_loss_pct": c["max_daily_loss_pct"],
            "min_confidence": c["min_confidence"],
            "stop_loss_price_pct": core.PRICE_STOP * 100,
        },
        "open_positions": [{
            "pair": t["pair"], "side": "short" if t.get("is_short") else "long",
            "open_rate": t["open_rate"], "current_rate": t.get("current_rate"),
            "profit_pct": _round((t.get("profit_ratio") or 0) * 100),
            "stop_loss_rate": t.get("stop_loss_abs"),
            "opened_utc": t["open_date"][:16], "entry_tag": t.get("enter_tag"),
        } for t in open_],
        "pairs": pairs,
    }


# ---------- 硬性风控 ----------

def guard(action, pair, confidence, source, snap):
    """返回 None 表示允许；否则返回拒绝原因"""
    c = conf()
    held = {p["pair"]: p for p in snap["open_positions"]}
    if action == "close":
        return None if pair in held else f"{pair} 没有持仓"
    if not c["enabled"]:
        return "AI 交易没有开启"
    if snap["bot_state"] != "running":
        return "机器人已暂停"
    if pair not in {p["pair"] for p in snap["pairs"]}:
        return f"{pair} 不在交易币种里"
    if action == "open_short" and not snap["short_allowed"]:
        return "当前不允许做空（只有合约并开启做空时才可以）"
    if pair in held:
        return f"{pair} 已经有持仓"
    lim = snap["risk_limits"]
    if lim["slots_left"] <= 0:
        return f"已达到同时持仓上限 {lim['max_open']} 笔"
    if entries_today() >= c["max_entries_per_day"]:
        return f"今天已开仓 {c['max_entries_per_day']} 次，达到每日上限"
    if snap["account"]["realized_pnl_today_pct"] <= -c["max_daily_loss_pct"]:
        return f"今天已亏 {-snap['account']['realized_pnl_today_pct']:.2f}%，达到当日亏损上限 {c['max_daily_loss_pct']}%"
    if source == "auto" and confidence < c["min_confidence"]:
        return f"置信度 {confidence:.2f} 低于设定的 {c['min_confidence']}"
    return None


def execute(action, pair, snap):
    # 都用市价单：决策基于收盘后的数据，挂限价单可能一直不成交
    if action == "close":
        trade = next(t for t in core.ft("/status") if t["pair"] == pair)
        try:
            core.ft("/forceexit", "POST", {"tradeid": trade["trade_id"], "ordertype": "market"})
        except HTTPException:
            # 开仓单还没成交时，forceexit 会撤单并删掉这笔交易，但仍返回错误；仓位没了就算成功
            if any(t["pair"] == pair for t in core.ft("/status")):
                raise
    else:
        core.ft("/forceenter", "POST", {"pair": pair, "side": "short" if action == "open_short" else "long",
                                        "ordertype": "market", "entry_tag": "ai"})


def apply_actions(actions, source, snap):
    """按顺序执行：先平仓再开仓，开仓后刷新持仓状态，保证后面的风控检查用到最新数据"""
    out = []
    for a in sorted(actions, key=lambda a: a["action"] != "close"):
        rec = {**a}
        why = guard(a["action"], a["pair"], a.get("confidence", 1.0), source, snap)
        if why:
            rec.update(status="blocked", note=why)
        else:
            try:
                execute(a["action"], a["pair"], snap)
                rec["status"] = "executed"
                snap = snapshot()
            except HTTPException as e:
                rec.update(status="failed", note=e.detail)
            except Exception as e:
                rec.update(status="failed", note=str(e))
        out.append(rec)
    return out


# ---------- 大模型 ----------

def parse(text, pairs):
    text = text.strip()
    if text.startswith("```"):
        text = text.split("\n", 1)[1].rsplit("```", 1)[0]
    d = json.loads(text)
    if not isinstance(d.get("analysis"), str) or not isinstance(d.get("actions"), list):
        raise ValueError("返回的 JSON 缺少 analysis 或 actions")
    acts = []
    for a in d["actions"]:
        if a.get("action") not in ("open_long", "open_short", "close"):
            raise ValueError(f"未知动作：{a.get('action')}")
        pair = str(a.get("pair", "")).upper()
        # 允许模型省略合约后缀，例如 BTC/USDT → BTC/USDT:USDT
        pair = next((p for p in pairs if p == pair or p.split(":")[0] == pair), pair)
        acts.append({"action": a["action"], "pair": pair, "confidence": float(a.get("confidence", 0)),
                     "reason": str(a.get("reason", ""))})
    return d["analysis"], acts


def call_llm(c, snap):
    user = "Snapshot:\n" + json.dumps(snap, ensure_ascii=False, separators=(",", ":"))
    model = c["model"].strip()
    if c["provider"] == "anthropic":
        import anthropic
        client = anthropic.Anthropic(api_key=c["api_key"], base_url=c["base_url"] or None, timeout=600)
        output_config = {"format": {"type": "json_schema", "schema": SCHEMA}}
        if model.startswith(EFFORT_MODELS):
            output_config["effort"] = "high"
        kw = dict(model=model, max_tokens=16000, system=SYSTEM, output_config=output_config,
                  messages=[{"role": "user", "content": user}])
        if model in FALLBACK_MODELS and not c["base_url"]:
            # 安全分类器拒答时，由服务端按拒答类别自动换模型重试
            resp = client.beta.messages.create(betas=["server-side-fallback-2026-07-01"], fallbacks="default", **kw)
        else:
            resp = client.messages.create(**kw)
        if resp.stop_reason == "refusal":
            raise RuntimeError("模型拒绝了这次请求")
        if resp.stop_reason == "max_tokens":
            raise RuntimeError("输出超长被截断")
        text = next(b.text for b in resp.content if b.type == "text")
        usage = {"input": resp.usage.input_tokens, "output": resp.usage.output_tokens}
    else:
        from openai import OpenAI
        client = OpenAI(api_key=c["api_key"], base_url=c["base_url"] or None, timeout=600)
        resp = client.chat.completions.create(
            model=model, response_format={"type": "json_object"},
            messages=[{"role": "system", "content": SYSTEM + "\n\nJSON schema:\n" + json.dumps(SCHEMA)},
                      {"role": "user", "content": user}],
        )
        text = resp.choices[0].message.content or ""
        usage = {"input": resp.usage.prompt_tokens, "output": resp.usage.completion_tokens} if resp.usage else {}
    return text, usage


def notify(rec):
    """有动作时推送到 Telegram（如果配置了）"""
    t = core.load(core.PRIV).get("telegram", {})
    acts = [a for a in rec.get("actions", []) if a.get("status")]
    if not (t.get("enabled") and t.get("chat_id")) or not (acts or rec.get("error")):
        return
    name = {"open_long": "开多", "open_short": "开空", "close": "平仓"}
    st = {"executed": "✅ 已执行", "blocked": "⛔ 风控拦截", "failed": "❌ 失败"}
    lines = [f"🤖 AI 决策（{rec['source']}）"]
    if rec.get("error"):
        lines.append(f"❌ {rec['error']}")
    for a in acts:
        lines.append(f"{st[a['status']]} {name[a['action']]} {a['pair']}：{a.get('reason', '')}"
                     + (f"\n　{a['note']}" if a.get("note") else ""))
    try:
        core.tg(t["token"], "sendMessage", chat_id=t["chat_id"], text="\n".join(lines))
    except Exception:
        pass


def run(execute_orders, source):
    """让大模型分析一次；execute_orders=False 只看建议不下单"""
    c = conf()
    if not c["api_key"]:
        raise HTTPException(400, "先填大模型的 API Key")
    if not _run_lock.acquire(blocking=False):
        raise HTTPException(409, "上一次分析还没结束")
    _state["running"] = True
    rec = {"time": datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M:%S"), "source": source,
           "model": c["model"], "execute": execute_orders}
    try:
        snap = snapshot()
        t0 = time.time()
        text, rec["usage"] = call_llm(c, snap)
        rec["seconds"] = round(time.time() - t0, 1)
        rec["analysis"], acts = parse(text, [p["pair"] for p in snap["pairs"]])
        rec["actions"] = apply_actions(acts, source, snap) if execute_orders else acts
        _state["last_error"] = None
    except HTTPException as e:
        rec["error"] = e.detail
    except Exception as e:
        rec["error"] = f"{type(e).__name__}: {e}"
    finally:
        _state["running"] = False
        _state["last_run"] = rec["time"]
        _run_lock.release()
    if rec.get("error"):
        _state["last_error"] = rec["error"]
    write_log(rec)
    if execute_orders:
        notify(rec)
    return rec


def _next_run(interval):
    """对齐 K 线收盘，再等 3 分钟让机器人把新 K 线算完"""
    step = interval * 60
    return (int(time.time()) // step + 1) * step + 180


def _scheduler():
    nxt = None
    while True:
        time.sleep(20)
        try:
            c = conf()
            if not c["enabled"] or not c["api_key"]:
                nxt = _state["next_run"] = None
                continue
            if nxt is None:
                nxt = _next_run(c["interval"])
            _state["next_run"] = datetime.fromtimestamp(nxt, timezone.utc).strftime("%Y-%m-%d %H:%M:%S")
            if time.time() >= nxt:
                nxt = _next_run(c["interval"])
                try:
                    run(True, "auto")
                except HTTPException:
                    pass
        except Exception as e:
            _state["last_error"] = f"定时任务出错：{e}"


# ---------- 接口 ----------

def public(c):
    return {**{k: v for k, v in c.items() if k != "api_key"}, "api_key": core.mask(c["api_key"])}


@router.get("")
def get_ai():
    return {"config": public(conf()), "state": _state, "entries_today": entries_today(), "log": read_log(20)}


class AiConfigIn(BaseModel):
    provider: str
    api_key: str | None = None      # 不传就保留原来的
    base_url: str = ""
    model: str
    interval: int = 240
    min_confidence: float = 0.6
    max_entries_per_day: int = 3
    max_daily_loss_pct: float = 5.0


@router.post("/config")
def set_config(a: AiConfigIn):
    if a.provider not in ("anthropic", "openai"):
        raise HTTPException(400, "provider 只能是 anthropic 或 openai")
    if not a.model.strip():
        raise HTTPException(400, "填写模型名称")
    if a.interval not in (60, 240):
        raise HTTPException(400, "分析频率只能是每小时或每 4 小时")
    if not 0 <= a.min_confidence <= 1:
        raise HTTPException(400, "最低置信度在 0 到 1 之间")
    if not 1 <= a.max_entries_per_day <= 20:
        raise HTTPException(400, "每日开仓次数在 1 到 20 之间")
    if not 0.5 <= a.max_daily_loss_pct <= 50:
        raise HTTPException(400, "当日亏损上限在 0.5% 到 50% 之间")
    priv = core.load(core.PRIV)
    old = {**DEFAULTS, **priv.get("ai", {})}
    new = {**old, **a.model_dump(exclude={"api_key"}), "model": a.model.strip(), "base_url": a.base_url.strip()}
    if a.api_key:
        new["api_key"] = a.api_key.strip()
    priv["ai"] = new
    core.save(core.PRIV, priv)
    return public(new)


class EnableIn(BaseModel):
    enabled: bool


@router.post("/enable")
def enable(e: EnableIn):
    """开启后 TrendFollow 自己的均线信号停用，由 AI 决定开平仓；止损和移动止盈照旧"""
    priv = core.load(core.PRIV)
    c = {**DEFAULTS, **priv.get("ai", {})}
    if e.enabled and not c["api_key"]:
        raise HTTPException(400, "先保存大模型的 API Key")
    c["enabled"] = e.enabled
    priv["ai"] = c
    core.save(core.PRIV, priv)
    cfg = core.load(core.CFG)
    cfg["panel_ai"] = e.enabled
    cfg["force_entry_enable"] = e.enabled
    core.save(core.CFG, cfg)
    core.reload_bot()
    return {}


@router.post("/run")
def run_now(execute: bool = False):
    if execute and not conf()["enabled"]:
        raise HTTPException(400, "先开启 AI 交易，才能让它下单")
    return run(execute, "manual")


@router.get("/snapshot")
def get_snapshot():
    return snapshot()


class OrderIn(BaseModel):
    action: str
    pair: str
    reason: str = ""


@router.post("/order")
def order(o: OrderIn):
    """外部（MCP / 脚本）下单入口，同样经过硬性风控"""
    if o.action not in ("open_long", "open_short", "close"):
        raise HTTPException(400, "action 只能是 open_long、open_short 或 close")
    snap = snapshot()
    pairs = [p["pair"] for p in snap["pairs"]] + [p["pair"] for p in snap["open_positions"]]
    _, acts = parse(json.dumps({"analysis": "", "actions": [
        {"action": o.action, "pair": o.pair, "confidence": 1.0, "reason": o.reason}]}), pairs)
    rec = {"time": datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M:%S"), "source": "mcp",
           "model": "external", "execute": True, "analysis": o.reason,
           "actions": apply_actions(acts, "mcp", snap)}
    write_log(rec)
    notify(rec)
    a = rec["actions"][0]
    if a["status"] != "executed":
        raise HTTPException(400, ("风控拦截：" if a["status"] == "blocked" else "下单失败：") + a["note"])
    return a


@router.get("/log")
def get_log(limit: int = 50):
    return read_log(min(max(limit, 1), 500))
