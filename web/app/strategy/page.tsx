"use client";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useFeedback } from "@/components/Feedback";
import { api, post } from "@/lib/api";
import { base, price, sign, vol } from "@/lib/fmt";
import { useOv } from "@/lib/store";
import { Card, Empty, MsgBox, Skeleton, useAction } from "@/lib/ui";

type Market = "spot" | "futures";
type Draft = { market: Market; leverage: number; short: boolean; max_open: number; risk_per_trade_pct: number; pairs: string[] };
type Row = {
  pair: string;
  base: string;
  price: number | null;
  change: number | null;
  volume: number;
  max_lev: number | null;
};

const toFutures = (p: string) => (p.includes(":") ? p : p + ":USDT");
const toSpot = (p: string) => p.split(":")[0];
const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), hi);

export default function StrategyPage() {
  const { d, loaded } = useOv();
  const { confirm } = useFeedback();
  const { busy, msg, setMsg, run } = useAction();

  const [draft, setDraft] = useState<Draft | null>(null);
  const [mk, setMk] = useState<Record<string, Row[]>>({});
  const [mkErr, setMkErr] = useState("");
  const [q, setQ] = useState("");

  const saved: Draft = useMemo(
    () => ({
      market: d.market === "futures" ? "futures" : "spot",
      leverage: d.leverage ?? 1,
      short: !!d.short,
      max_open: d.max_open ?? 3,
      risk_per_trade_pct: d.risk_per_trade_pct ?? 1,
      pairs: d.pairs ?? [],
    }),
    [d.market, d.leverage, d.short, d.max_open, d.risk_per_trade_pct, d.pairs],
  );

  const savedRef = useRef(saved);
  savedRef.current = saved;

  const cur = draft ?? saved;
  const fut = cur.market === "futures";
  const maxPairs = d.max_pairs ?? 12;
  const maxOpen = d.max_open_limit ?? 5;
  const maxLev = d.max_leverage ?? 25;
  // 止损为 2.5×ATR，限制在 2%–10%；系统按止损距离逐笔下调杠杆，使止损不超过强平距离的 60%。
  const stopLo = 0.02;
  const stopHi = 0.1;
  const levFor = (stop: number) => Math.max(1, Math.min(cur.leverage, Math.floor(0.6 / stop + 1e-9)));

  const loadingMk = useRef<Set<string>>(new Set());

  const ensureMarkets = useCallback(async (m: Market) => {
    if (mk[m] || loadingMk.current.has(m)) return;
    loadingMk.current.add(m);
    try {
      const rows = await api<Row[]>(`/api/markets?market=${m}`);
      setMk((prev) => ({ ...prev, [m]: rows }));
      setMkErr("");
    } catch (e: any) {
      setMkErr(e?.message || "读取 Bybit 币种失败");
    } finally {
      loadingMk.current.delete(m);
    }
  }, [mk]);

  useEffect(() => {
    if (loaded && d.pairs) ensureMarkets(cur.market);
  }, [loaded, d.pairs, cur.market, ensureMarkets]);

  // 有未保存的改动时离开页面提醒
  const dirty = !!draft && changes().length > 0;
  useEffect(() => {
    if (!dirty) return;
    const onBefore = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = "";
    };
    window.addEventListener("beforeunload", onBefore);
    return () => window.removeEventListener("beforeunload", onBefore);
  });

  function update(ch: Partial<Draft>) {
    if (ch.market) ensureMarkets(ch.market);
    setDraft((prev) => {
      const from = prev ?? savedRef.current;
      const next: Draft = { ...from, ...ch };
      if (ch.market && ch.market !== from.market) {
        next.pairs = from.pairs.map(ch.market === "futures" ? toFutures : toSpot);
        if (ch.market === "spot") {
          next.leverage = 1;
          next.short = false;
        }
      }
      next.max_open = clamp(next.max_open || 1, 1, maxOpen);
      next.leverage = clamp(next.leverage || 1, 1, maxLev);
      next.risk_per_trade_pct = clamp(Math.round((next.risk_per_trade_pct || 1) * 10) / 10, 0.1, 5);
      return next;
    });
  }

  function reset() {
    setDraft(null);
    setMsg(null);
  }

  function changes(): string[] {
    const a = saved;
    const b = cur;
    const out: string[] = [];
    if (a.market !== b.market) out.push(b.market === "futures" ? "改成合约" : "改成现货");
    if (a.leverage !== b.leverage && b.market === "futures") out.push(`杠杆 ${a.leverage}× → ${b.leverage}×`);
    if (a.short !== b.short && b.market === "futures") out.push(b.short ? "开启做空" : "关闭做空");
    if (a.max_open !== b.max_open) out.push(`持仓 ${a.max_open} → ${b.max_open} 笔`);
    if (a.risk_per_trade_pct !== b.risk_per_trade_pct) out.push(`每笔风险 ${a.risk_per_trade_pct}% → ${b.risk_per_trade_pct}%`);
    if (a.market === b.market) {
      const sa = new Set(a.pairs);
      const sb = new Set(b.pairs);
      const add = b.pairs.filter((p) => !sa.has(p));
      const del = a.pairs.filter((p) => !sb.has(p));
      if (add.length) out.push("加 " + add.map(base).join("、"));
      if (del.length) out.push("去掉 " + del.map(base).join("、"));
    }
    return out;
  }

  function togglePair(p: string) {
    const pairs = [...cur.pairs];
    const i = pairs.indexOf(p);
    if (i >= 0) pairs.splice(i, 1);
    else if (pairs.length >= maxPairs) {
      setMsg({ kind: "warn", text: `最多选 ${maxPairs} 个币` });
      return;
    } else pairs.push(p);
    setMsg(null);
    update({ pairs });
  }

  async function save() {
    const ch = changes();
    if ((fut && cur.leverage >= 5) || cur.risk_per_trade_pct > 2) {
      const ok = await confirm({
        title: cur.risk_per_trade_pct > 2 ? `确定每笔承担 ${cur.risk_per_trade_pct}% 风险？` : `确定用最高 ${cur.leverage} 倍杠杆？`,
        body: `每次止损约亏总资金的 ${cur.risk_per_trade_pct}%（不含费用和滑点），${cur.max_open} 笔同时止损约亏 ${(cur.risk_per_trade_pct * cur.max_open).toFixed(1)}%。杠杆只影响占用的保证金，系统会按止损距离逐笔下调，保证止损先于强平。`,
        ok: "确定",
        danger: true,
      });
      if (!ok) return;
    }
    await run("save", async () => {
      await post("/api/strategy", cur);
      setDraft(null);
      return {
        kind: "good",
        text: `已保存，机器人已按新设置运行：${ch.join("，")}。新加的币要几分钟准备 K 线数据。`,
      };
    });
  }

  if (!loaded) {
    return (
      <>
        <Card>
          <Skeleton lines={5} />
        </Card>
      </>
    );
  }

  const rows0 = mk[cur.market];
  const sel = new Set(cur.pairs);
  const query = q.trim().toUpperCase();
  let list: Row[] = rows0
    ? query
      ? rows0.filter((m) => m.base.includes(query))
      : rows0.slice(0, 40)
    : [];
  if (!query && rows0) list = [...rows0.filter((m) => sel.has(m.pair) && !list.includes(m)), ...list];

  const warn: string[] = [];
  if (cur.risk_per_trade_pct * cur.max_open > 6)
    warn.push(`${cur.max_open} 笔同时止损约亏总资金的 ${(cur.risk_per_trade_pct * cur.max_open).toFixed(1)}%，超过一般建议的 6%。`);
  if (fut && cur.leverage >= 5)
    warn.push(`跳空或滑点可能让止损成交价比触发价更差；杠杆越高，留给止损的空间越小。`);
  const ch = changes();
  return (
    <>
      <Card title="交易配置" note="AI 交易 · 4 小时行情 · 2.5×ATR 止损">
        <div className="fields">
          <div className="field">
            <b>市场</b>
            <div className="seg" role="radiogroup" aria-label="市场">
              <button className={cur.market === "spot" ? "on" : ""} onClick={() => update({ market: "spot" })}>
                现货
              </button>
              <button
                className={cur.market === "futures" ? "on" : ""}
                onClick={() => update({ market: "futures" })}
              >
                USDT 永续合约
              </button>
            </div>
            <p>
              {fut
                ? "逐仓：每笔仓位的保证金独立，一笔爆仓不会牵连其他仓位和账户余额。止损单直接挂在 Bybit 上。"
                : "只能做多，没有杠杆，不会爆仓。止损由机器人执行。"}
            </p>
          </div>

          <div className={"field lev" + (fut ? "" : " off")}>
            <b>杠杆</b>
            <div className="lev-top">
              <b>{cur.leverage}×</b>
              <span>
                {!fut ? "现货没有杠杆" : cur.leverage === 1 ? "不加杠杆" : "上限，系统按止损距离逐笔下调"}
              </span>
            </div>
            <input
              type="range"
              min={1}
              max={maxLev}
              step={1}
              value={cur.leverage}
              disabled={!fut}
              aria-label="杠杆倍数"
              onChange={(e) => update({ leverage: +e.target.value })}
            />
            <div className="lev-ticks">
              <span>1×</span>
              <span>{Math.round(maxLev / 3)}×</span>
              <span>{Math.round(maxLev * 2 / 3)}×</span>
              <span>{maxLev}×</span>
            </div>
            <div className="risk">
              <div>
                <span>实际杠杆（止损 2% → 10%）</span>
                <b>{fut ? `${levFor(stopLo)}× → ${levFor(stopHi)}×` : "1×"}</b>
              </div>
              <div>
                <span>价格反向多少会爆仓</span>
                <b>{fut ? `约 ${(100 / levFor(stopLo)).toFixed(1)}% 起，止损先触发` : "不会爆仓"}</b>
              </div>
            </div>
          </div>

          <div className="field">
            <b>方向和仓位</b>
            <label className="toggle">
              <input
                type="checkbox"
                checked={cur.short}
                disabled={!fut}
                onChange={(e) => update({ short: e.target.checked })}
              />
              <span>允许做空（跌势里开空单）</span>
            </label>
            <p>{fut ? "允许 AI 在判断下跌趋势有正预期收益时开空单。" : "现货不能做空，切到合约才能开。"}</p>
            <div style={{ marginTop: 14, display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
              <span className="muted" style={{ fontSize: 13 }}>
                同时最多持有
              </span>
              <span className="stepper">
                <button aria-label="减少" onClick={() => update({ max_open: cur.max_open - 1 })}>
                  −
                </button>
                <output>{cur.max_open}</output>
                <button aria-label="增加" onClick={() => update({ max_open: cur.max_open + 1 })}>
                  +
                </button>
              </span>
              <span className="muted" style={{ fontSize: 13 }}>
                笔
              </span>
            </div>
            <p>这是持仓数量上限，不要求开满。每笔保证金不超过预留 5% 后按剩余名额平分的资金。</p>
            <div style={{ marginTop: 14, display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
              <span className="muted" style={{ fontSize: 13 }}>
                每笔最多亏
              </span>
              <span className="stepper">
                <button aria-label="减少" onClick={() => update({ risk_per_trade_pct: cur.risk_per_trade_pct - (cur.risk_per_trade_pct > 1 ? 0.5 : 0.1) })}>
                  −
                </button>
                <output>{cur.risk_per_trade_pct}%</output>
                <button aria-label="增加" onClick={() => update({ risk_per_trade_pct: cur.risk_per_trade_pct + (cur.risk_per_trade_pct >= 1 ? 0.5 : 0.1) })}>
                  +
                </button>
              </span>
              <span className="muted" style={{ fontSize: 13 }}>
                总资金
              </span>
            </div>
            <p>仓位按「每笔风险 ÷ 止损距离」计算：止损越宽，仓位越小，止损时亏损都约为总资金的这个比例。资金太少达不到交易所最低下单额时不会开仓。</p>
          </div>
        </div>
        {warn.length > 0 && (
          <div className="msg warn" style={{ marginTop: 14 }}>
            {warn.join("\n")}
          </div>
        )}
      </Card>

      <Card title="交易哪些币" note={`${cur.pairs.length} / ${maxPairs} 个 · ${fut ? "USDT 永续合约" : "现货"}`}>
        <div className="chips">
          {cur.pairs.length ? (
            cur.pairs.map((p) => {
              const isNew = saved.market === cur.market && !saved.pairs.includes(p);
              return (
                <span key={p} className={"chip" + (isNew ? " new" : "")}>
                  {base(p)}
                  <button aria-label={`移除 ${base(p)}`} onClick={() => togglePair(p)}>
                    ×
                  </button>
                </span>
              );
            })
          ) : (
            <span className="empty" style={{ padding: "4px 0" }}>
              一个都没选
            </span>
          )}
        </div>

        <div className="picker">
          <input
            placeholder="搜索 Bybit，例如 DOGE、SUI、PEPE"
            autoComplete="off"
            value={q}
            onChange={(e) => setQ(e.target.value)}
            aria-label="搜索币种"
          />
          <div className="m m-head">
            <span>币种</span>
            <span className="r">价格</span>
            <span className="r">24 小时</span>
            <span className="r vol">成交额</span>
            <span className="r lv">最高杠杆</span>
            <span />
          </div>
          <div className="mk">
            {mkErr ? (
              <Empty>{mkErr}</Empty>
            ) : !rows0 ? (
              <Skeleton lines={6} />
            ) : list.length ? (
              list.slice(0, 80).map((m) => {
                const on = sel.has(m.pair);
                const tooLow = fut && m.max_lev && m.max_lev < cur.leverage;
                return (
                  <div key={m.pair} className={"m" + (on ? " sel" : "")}>
                    <span className="b">
                      {m.base}
                      {m.volume < 5e6 && <small>成交少</small>}
                      {tooLow && <small>最高 {m.max_lev}×</small>}
                    </span>
                    <span className="r">{price(m.price)}</span>
                    <span className={"r " + sign(m.change)}>
                      {m.change == null ? "—" : (m.change > 0 ? "+" : "") + m.change.toFixed(1) + "%"}
                    </span>
                    <span className="r vol dim">{vol(m.volume)}</span>
                    <span className="r lv dim">{fut && m.max_lev ? `${m.max_lev}×` : "—"}</span>
                    <button className={on ? "ghost" : ""} aria-label={on ? `移除 ${m.base}` : `添加 ${m.base}`} onClick={() => togglePair(m.pair)}>
                      {on ? "−" : "+"}
                    </button>
                  </div>
                );
              })
            ) : (
              <Empty>
                Bybit {fut ? "合约" : "现货"}没有「{query}」
              </Empty>
            )}
          </div>
        </div>
      </Card>

      <MsgBox m={msg} />

      <div className="savebar">
        <span className="diff">
          {ch.length ? (
            <>
              待保存：<b>{ch.join("，")}</b>
            </>
          ) : (
            "设置没有改动"
          )}
        </span>
        <span className="sp" />
        {ch.length > 0 && (
          <button className="ghost" onClick={reset}>
            撤销改动
          </button>
        )}
        <button className="primary" onClick={save} disabled={!ch.length || !cur.pairs.length || busy === "save"}>
          {busy === "save" ? "保存中…" : "保存并应用"}
        </button>
      </div>
    </>
  );
}
