"""MCP 服务：让 Claude（Claude Code / Claude Desktop）读行情、看持仓、下单。
所有请求都走管理后台的 HTTPS 接口，下单同样经过后台的硬性风控（ai.py 的 guard）。

需要环境变量：
  PANEL_URL   默认 https://trade.juyouagent.com
  PANEL_USER  默认 admin
  PANEL_PASS  后台登录密码（deploy/secrets/服务器信息.md）

注册到 Claude Code：
  claude mcp add bybit-trade -e PANEL_PASS=密码 -- python D:/coding/tide/mcp/trade_mcp.py
"""
import json
import os

import httpx
from mcp.server.fastmcp import FastMCP

URL = os.environ.get("PANEL_URL", "https://trade.juyouagent.com").rstrip("/")
AUTH = (os.environ.get("PANEL_USER", "admin"), os.environ.get("PANEL_PASS", ""))

mcp = FastMCP("bybit-trade")


def call(method, path, **kw):
    r = httpx.request(method, URL + path, auth=AUTH, timeout=600, **kw)
    if r.status_code == 401:
        raise RuntimeError("后台登录失败：检查 PANEL_USER / PANEL_PASS")
    try:
        body = r.json()
    except ValueError:
        body = r.text
    if r.status_code >= 400:
        raise RuntimeError(body.get("detail") if isinstance(body, dict) else body)
    return body


def dump(x):
    return json.dumps(x, ensure_ascii=False, separators=(",", ":"))


@mcp.tool()
def get_market_snapshot() -> str:
    """当前完整行情快照：账户权益和当日盈亏、风控限额（剩余仓位、今日剩余开仓次数）、持仓、
    每个交易币最近 60 根已收盘 K 线（含 EMA20/50/200、ADX、RSI、ATR）。分析和下单前先调用它。"""
    return dump(call("GET", "/api/ai/snapshot"))


@mcp.tool()
def get_status() -> str:
    """机器人概况（只跑实盘）：现货/合约、杠杆、交易币种、持仓、累计收益、AI 交易是否开启。"""
    d = call("GET", "/api/overview")
    keep = ["market", "leverage", "short", "pairs", "max_open", "state", "ai", "open", "profit", "balance"]
    return dump({k: d.get(k) for k in keep})


@mcp.tool()
def get_trades(limit: int = 20) -> str:
    """最近的已平仓交易记录。"""
    return dump(call("GET", f"/api/trades?limit={limit}"))


@mcp.tool()
def get_ai_decisions(limit: int = 10) -> str:
    """服务器上 AI 定时分析和外部下单的历史记录（分析、动作、是否执行、风控拦截原因）。"""
    return dump(call("GET", f"/api/ai/log?limit={limit}"))


@mcp.tool()
def open_position(pair: str, side: str, reason: str) -> str:
    """开仓。pair 用快照里的交易对（如 BTC/USDT，合约是 BTC/USDT:USDT）；side 是 long 或 short（做空只在合约且开启做空时可用）；
    reason 写清楚开仓理由。每笔金额、杠杆、8% 止损由系统决定。需要后台已开启 AI 交易，并会经过风控：
    持仓上限、每日开仓次数、当日亏损上限。会用真钱下单，下单前先确认用户同意。"""
    if side not in ("long", "short"):
        raise ValueError("side 只能是 long 或 short")
    return dump(call("POST", "/api/ai/order", json={"action": f"open_{side}", "pair": pair, "reason": reason}))


@mcp.tool()
def close_position(pair: str, reason: str) -> str:
    """按市价平掉某个交易对的持仓。reason 写清楚平仓理由。会用真钱成交，先确认用户同意。"""
    return dump(call("POST", "/api/ai/order", json={"action": "close", "pair": pair, "reason": reason}))


@mcp.tool()
def run_server_ai(execute: bool = False) -> str:
    """让服务器上配置好的大模型立即分析一次。execute=false 只返回建议；true 会按它的决定下单（经过风控）。"""
    return dump(call("POST", f"/api/ai/run?execute={str(execute).lower()}"))


if __name__ == "__main__":
    mcp.run()
