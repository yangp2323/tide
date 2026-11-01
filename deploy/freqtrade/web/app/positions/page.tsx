"use client";
import { useCallback, useEffect, useState } from "react";
import { useFeedback } from "@/components/Feedback";
import { api, post } from "@/lib/api";
import { base, money, pct, price, sign, when } from "@/lib/fmt";
import { useOv } from "@/lib/store";
import { Card, Empty, SideTag, Skeleton, useAction } from "@/lib/ui";

export default function PositionsPage() {
  const { d, loaded, refresh } = useOv();
  const { confirm, toast } = useFeedback();
  const { busy, run } = useAction();
  const [hold, setHold] = useState<any>(null);
  const [holdErr, setHoldErr] = useState("");
  const [holdLoading, setHoldLoading] = useState(true);

  const open: any[] = d.open || [];
  const fut = d.market === "futures";

  const loadHold = useCallback(async (quiet = false) => {
    if (!quiet) setHoldLoading(true);
    try {
      const h = await api<any>("/api/holdings");
      setHold(h);
      setHoldErr("");
    } catch (e: any) {
      setHoldErr(e?.message || "读取失败");
    } finally {
      setHoldLoading(false);
    }
  }, []);

  useEffect(() => {
    loadHold(true);
    const t = setInterval(() => {
      if (document.visibilityState === "visible") loadHold(true);
    }, 20000);
    return () => clearInterval(t);
  }, [loadHold]);

  async function exitTrade(t: any) {
    const ok = await confirm({
      title: `按市价平掉 ${base(t.pair)}？`,
      body: "按当前市价成交，实际价格可能和这里显示的有少量偏差。",
      ok: "市价平仓",
      danger: true,
    });
    if (!ok) return;
    await run(`exit-${t.trade_id}`, async () => {
      await post(`/api/trades/${t.trade_id}/exit`);
      toast(`${base(t.pair)} 已提交市价平仓`);
      await refresh();
      loadHold(true);
    }, false);
  }

  return (
    <>
      <Card title="当前持仓" note={open.length ? `${open.length} / ${d.max_open} 笔` : ""}>
        {!loaded ? (
          <Skeleton lines={4} />
        ) : open.length ? (
          <div className="scroll">
            <table className="tbl">
              <thead>
                <tr>
                  <th>币</th>
                  <th>开仓时间</th>
                  <th className="r">开仓价</th>
                  <th className="r">现价</th>
                  {fut && <th className="r">杠杆</th>}
                  {fut && <th className="r">爆仓价</th>}
                  <th className="r">止损价</th>
                  <th className="r">{fut ? "保证金" : "投入"}</th>
                  <th className="r">浮动盈亏</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {open.map((t) => (
                  <tr key={t.trade_id}>
                    <td className="p">
                      {base(t.pair)}
                      {fut ? <SideTag short={t.is_short} /> : null}
                    </td>
                    <td>{when(t.open_date)}</td>
                    <td className="r">{price(t.open_rate)}</td>
                    <td className="r">{price(t.current_rate)}</td>
                    {fut && <td className="r">{t.leverage}×</td>}
                    {fut && <td className="r down">{price(t.liquidation_price)}</td>}
                    <td className="r">{price(t.stop_loss_abs)}</td>
                    <td className="r">{money(t.stake_amount)}</td>
                    <td className={"r " + sign(t.profit_abs)}>
                      {money(t.profit_abs)} · {pct(t.profit_ratio)}
                    </td>
                    <td className="r">
                      <button
                        className="ghost sm"
                        disabled={busy === `exit-${t.trade_id}`}
                        onClick={() => exitTrade(t)}
                      >
                        市价平仓
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : (
          <Empty>现在没有持仓。4 小时策略出手少，几天不交易是正常的。</Empty>
        )}
      </Card>

      <Card
        title="Bybit 账户实际持仓"
        note={hold ? `账户权益 ${money(hold.equity)} · 可动用 ${money(hold.available)}` : ""}
        right={
          <button className="ghost sm" onClick={() => loadHold()} disabled={holdLoading}>
            {holdLoading ? "读取中…" : "刷新"}
          </button>
        }
        style={{ marginTop: 16 }}
      >
        <p className="hint">直接读取 Bybit 账户，包括你手动开的仓位，不限于机器人下的单。只读，这里不会下单。</p>
        {holdErr ? (
          <Empty>读取失败：{holdErr}</Empty>
        ) : holdLoading && !hold ? (
          <Skeleton lines={4} />
        ) : hold ? (
          <>
            {hold.positions?.length ? (
              <div className="scroll" style={{ marginTop: 12 }}>
                <table className="tbl">
                  <thead>
                    <tr>
                      <th>合约</th>
                      <th className="r">数量</th>
                      <th className="r">杠杆</th>
                      <th className="r">开仓价</th>
                      <th className="r">现价</th>
                      <th className="r">强平价</th>
                      <th className="r">离强平</th>
                      <th className="r">止盈 / 止损</th>
                      <th className="r">保证金</th>
                      <th className="r">浮动盈亏</th>
                    </tr>
                  </thead>
                  <tbody>
                    {hold.positions.map((p: any) => (
                      <tr key={p.symbol + p.side}>
                        <td className="p">
                          {String(p.symbol).replace("USDT", "")}
                          <SideTag short={p.side === "short"} />
                        </td>
                        <td className="r">{p.size}</td>
                        <td className="r">{p.leverage}×</td>
                        <td className="r">{price(p.entry)}</td>
                        <td className="r">{price(p.mark)}</td>
                        <td className="r down">{p.liq ? price(p.liq) : "—"}</td>
                        <td className={"r" + (p.liq_dist_pct != null && p.liq_dist_pct < 10 ? " down" : "")}>
                          {p.liq_dist_pct != null ? p.liq_dist_pct.toFixed(1) + "%" : "—"}
                        </td>
                        <td className="r">
                          {p.tp ? price(p.tp) : "无"} /{" "}
                          {p.sl ? price(p.sl) : <span className="down">未设置</span>}
                        </td>
                        <td className="r">{money(p.margin)}</td>
                        <td className={"r " + sign(p.pnl)}>{money(p.pnl)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ) : null}

            {hold.coins?.length ? (
              <div className="scroll" style={{ marginTop: 12 }}>
                <table className="tbl">
                  <thead>
                    <tr>
                      <th>币种</th>
                      <th className="r">数量</th>
                      <th className="r">折合 USDT</th>
                      <th className="r">冻结</th>
                    </tr>
                  </thead>
                  <tbody>
                    {hold.coins.map((c: any) => (
                      <tr key={c.coin}>
                        <td className="p">{c.coin}</td>
                        <td className="r">{c.amount}</td>
                        <td className="r">{money(c.usd)}</td>
                        <td className="r">{c.locked || 0}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ) : null}

            {hold.orders?.length ? (
              <div className="scroll" style={{ marginTop: 12 }}>
                <table className="tbl">
                  <thead>
                    <tr>
                      <th>挂单</th>
                      <th>类型</th>
                      <th className="r">数量</th>
                      <th className="r">价格 / 触发价</th>
                      <th>状态</th>
                    </tr>
                  </thead>
                  <tbody>
                    {hold.orders.map((o: any, i: number) => (
                      <tr key={`${o.symbol}-${i}`}>
                        <td className="p">
                          {String(o.symbol).replace("USDT", "")} {o.side === "Buy" ? "买" : "卖"}
                        </td>
                        <td>{o.stop_type || o.type}</td>
                        <td className="r">{o.qty}</td>
                        <td className="r">
                          {o.trigger ? "触发 " + price(o.trigger) : o.price ? price(o.price) : "市价"}
                        </td>
                        <td>{o.status}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ) : null}

            {!hold.positions?.length && !hold.coins?.length && !hold.orders?.length && (
              <Empty>Bybit 账户里没有币、仓位和挂单。</Empty>
            )}
          </>
        ) : null}
      </Card>
    </>
  );
}
