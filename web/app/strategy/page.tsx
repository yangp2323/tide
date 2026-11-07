"use client";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useFeedback } from "@/components/Feedback";
import { api, post } from "@/lib/api";
import { base, price, sign, vol } from "@/lib/fmt";
import { useOv } from "@/lib/store";
import AiControls from "@/components/AiControls";
import { Card, Empty, MsgBox, Skeleton, useAction } from "@/lib/ui";

type Market = "spot" | "futures";
type Draft = { market: Market; leverage: number; min_leverage: number; pairs: string[] };
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
  const riskAction = useAction();
  const [risk, setRisk] = useState<{ min_confidence: number; daily_profit_target_pct: number; max_daily_loss_pct: number; max_drawdown_pct: number; max_consecutive_losses: number } | null>(null);

  const [draft, setDraft] = useState<Draft | null>(null);
  const [mk, setMk] = useState<Record<string, Row[]>>({});
  const [mkErr, setMkErr] = useState("");
  const [q, setQ] = useState("");

  const saved: Draft = useMemo(
    () => ({
      market: d.market === "futures" ? "futures" : "spot",
      leverage: d.leverage ?? 1,
      min_leverage: d.min_leverage ?? 1,
      pairs: d.pairs ?? [],
    }),
    [d.market, d.leverage, d.min_leverage, d.pairs],
  );

  const savedRef = useRef(saved);
  savedRef.current = saved;

  const cur = draft ?? saved;
  const fut = cur.market === "futures";
  const maxPairs = d.max_pairs ?? 12;
  const maxLev = d.max_leverage ?? 25;
  const openCap = d.max_open_limit ?? 5;
  // 止损由 AI 给出（默认参考 2.5×ATR），限制在 2%–10%；系统按止损距离逐笔下调杠杆，使止损不超过强平距离的 60%。
  const stopLo = 0.02;
  const stopHi = 0.1;
  const levFor = (stop: number) => Math.max(1, Math.min(cur.leverage, Math.floor(0.6 / stop + 1e-9)));

  useEffect(() => {
    api<any>("/api/ai")
      .then((r) => {
        const c = r.config;
        setRisk({
          min_confidence: c.min_confidence,
          daily_profit_target_pct: c.daily_profit_target_pct ?? 0,
          max_daily_loss_pct: c.max_daily_loss_pct,
          max_drawdown_pct: c.max_drawdown_pct ?? 20,
          max_consecutive_losses: c.max_consecutive_losses ?? 5,
        });
      })
      .catch(() => {});
  }, []);

  async function saveRisk() {
    if (!risk) return;
    await riskAction.run("risk", async () => {
      await post("/api/ai/config", risk);
      return { kind: "good", text: "已保存，下一轮分析起生效。" };
    });
  }

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
          next.min_leverage = 1;
        }
      }
      next.leverage = clamp(next.leverage || 1, 1, maxLev);
      // 最低杠杆不能高于最高杠杆：调低上限时一起压下来
      next.min_leverage = clamp(next.min_leverage || 1, 1, next.leverage);
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
    if (a.leverage !== b.leverage && b.market === "futures") out.push(`最高杠杆 ${a.leverage}× → ${b.leverage}×`);
    if (a.min_leverage !== b.min_leverage && b.market === "futures")
      out.push(`最低杠杆 ${a.min_leverage}× → ${b.min_leverage}×`);
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
    if (fut && cur.leverage >= 5) {
      const ok = await confirm({
        title: `确定用最高 ${cur.leverage} 倍杠杆？`,
        body: `AI 在 ${cur.min_leverage}×–${cur.leverage}× 之间选择杠杆，并自行决定每笔投入多少可用资金、同时最多 ${openCap} 笔。单笔没有亏损上限：仓位 = 投入的保证金 × 杠杆，止损时亏损约为仓位 × 止损距离。系统会按止损距离下调杠杆，保证止损先于强平。`,
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
  if (fut && cur.leverage >= 5)
    warn.push(`跳空或滑点可能让止损成交价比触发价更差；杠杆越高，留给止损的空间越小。`);
  const ch = changes();
  return (
    <>
      <AiControls />
      <Card title="交易策略" note="市场、杠杆、方向和仓位的上限 · 4 小时行情">
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
            <b>最高杠杆</b>
            <div className="lev-top">
              <b>{cur.leverage}×</b>
              <span>
                {!fut ? "现货没有杠杆" : cur.leverage === 1 ? "不加杠杆" : "上限，AI 在此范围内选择，系统按止损距离再下调"}
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
            <b style={{ display: "block", marginTop: 16 }}>最低杠杆</b>
            <div className="lev-top">
              <b>{cur.min_leverage}×</b>
              <span>
                {!fut
                  ? "现货没有杠杆"
                  : cur.min_leverage === 1
                    ? "不设下限"
                    : "AI 选的杠杆低于它会被抬高；止损较宽、安全杠杆更低时以安全值为准"}
              </span>
            </div>
            <input
              type="range"
              min={1}
              max={Math.max(cur.leverage, 1)}
              step={1}
              value={cur.min_leverage}
              disabled={!fut || cur.leverage <= 1}
              aria-label="最低杠杆倍数"
              onChange={(e) => update({ min_leverage: +e.target.value })}
            />
            <div className="lev-ticks">
              <span>1×</span>
              <span>{cur.leverage}×</span>
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
            <p>
              {fut ? "做多还是做空、同时持有几笔、每笔投入多少可用资金，都由 AI 根据行情灵活分配，没有开关。" : "现货只能做多。同时持有几笔、每笔投入多少可用资金，由 AI 根据行情灵活分配，没有开关。"}
            </p>
            <div className="risk" style={{ marginTop: 12 }}>
              <div>
                <span>同时最多持有（系统上限）</span>
                <b>{openCap} 笔</b>
              </div>
              <div>
                <span>每笔可投入</span>
                <b>最多可用资金的 95%，单笔不设亏损上限</b>
              </div>
            </div>
            <p>AI 看到每个币的最小下单额，资金不够的币不会提出开仓。当日亏损、回撤和连续亏损这些账户级保护在本页的风控保护里设置，触发后会停止开仓。</p>
          </div>
        </div>
        {warn.length > 0 && (
          <div className="msg warn" style={{ marginTop: 14 }}>
            {warn.join("\n")}
          </div>
        )}
      </Card>

      <Card title="风控保护" note="账户级的安全线，触发后停止开仓" className="ai-settings">
        {!risk ? (
          <Skeleton lines={3} />
        ) : (
          <>
            <div className="ai-fields">
              <div>
                <label className="f" htmlFor="rkConf">
                  最低置信度（低于不执行）
                </label>
                <input id="rkConf" type="number" step={0.05} min={0} max={1} value={risk.min_confidence} onChange={(e) => setRisk({ ...risk, min_confidence: +e.target.value })} />
              </div>
              <div>
                <label className="f" htmlFor="rkTarget">
                  每日盈利目标 %（0 = 不设）
                </label>
                <input id="rkTarget" type="number" step={0.5} min={0} max={100} value={risk.daily_profit_target_pct} onChange={(e) => setRisk({ ...risk, daily_profit_target_pct: +e.target.value })} />
                <p className="hint">占当日起始权益（UTC）的比例，写进 AI 提示词作为当天目标。只是目标，系统不会因此强制开仓，达到后也不停止。</p>
              </div>
              <div>
                <label className="f" htmlFor="rkLoss">
                  当日亏损达到 % 停止开仓
                </label>
                <input id="rkLoss" type="number" step={0.5} min={0.5} max={50} value={risk.max_daily_loss_pct} onChange={(e) => setRisk({ ...risk, max_daily_loss_pct: +e.target.value })} />
                <p className="hint">已实现亏损加上持仓的浮亏，占当日起始权益的比例。</p>
              </div>
              <div>
                <label className="f" htmlFor="rkDrawdown">
                  账户回撤达到 % 暂停机器人（0 = 不限）
                </label>
                <input id="rkDrawdown" type="number" step={1} min={0} max={90} value={risk.max_drawdown_pct} onChange={(e) => setRisk({ ...risk, max_drawdown_pct: +e.target.value })} />
                <p className="hint">从机器人每次启动时的权益算起，触发后自动暂停，已有仓位的止损仍然执行。</p>
              </div>
              <div>
                <label className="f" htmlFor="rkStreak">
                  连续亏损 N 笔停止开仓（0 = 不限）
                </label>
                <input id="rkStreak" type="number" step={1} min={0} max={50} value={risk.max_consecutive_losses} onChange={(e) => setRisk({ ...risk, max_consecutive_losses: +e.target.value })} />
                <p className="hint">只统计机器人启动之后平掉的单，重新启动即可恢复。</p>
              </div>
            </div>
            <div className="row">
              <button onClick={saveRisk} disabled={riskAction.busy === "risk"}>
                {riskAction.busy === "risk" ? "保存中…" : "保存"}
              </button>
            </div>
            <MsgBox m={riskAction.msg} />
          </>
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
