"use client";
import { useCallback, useEffect, useState } from "react";
import { api } from "@/lib/api";
import { WHY, base, money, pct, price, sign, when } from "@/lib/fmt";
import { useOv } from "@/lib/store";
import { Card, Empty, SideTag, Skeleton } from "@/lib/ui";

const PER = 30;

export default function TradesPage() {
  const { d } = useOv();
  const [page, setPage] = useState(0);
  const [data, setData] = useState<any>(null);
  const [err, setErr] = useState("");
  const [loading, setLoading] = useState(true);
  const [tick, setTick] = useState(0);

  const fut = d.market === "futures";

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const r = await api<any>(`/api/trades?limit=${PER}&offset=${page * PER}`);
      setData(r);
      setErr("");
    } catch (e: any) {
      setErr(e?.message || "加载失败");
    } finally {
      setLoading(false);
    }
  }, [page]);

  useEffect(() => {
    load();
  }, [load, tick]);

  const total = data?.total_trades ?? 0;
  const pages = Math.max(1, Math.ceil(total / PER));
  const rows: any[] = data?.trades || [];

  return (
    <Card
      title="已平仓交易"
      note={data ? `共 ${total} 笔` : ""}
      right={
        <button className="ghost sm" onClick={() => setTick((t) => t + 1)} disabled={loading}>
          {loading ? "加载中…" : "刷新"}
        </button>
      }
    >
      {err ? (
        <Empty>{err}</Empty>
      ) : loading && !data ? (
        <Skeleton lines={6} />
      ) : rows.length ? (
        <>
          <div className="scroll">
            <table className="tbl">
              <thead>
                <tr>
                  <th>币</th>
                  <th>开仓</th>
                  <th>平仓</th>
                  <th className="r">开仓价</th>
                  <th className="r">平仓价</th>
                  {fut && <th className="r">杠杆</th>}
                  <th className="r">盈亏 USDT</th>
                  <th className="r">收益率</th>
                  <th>原因</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((t) => (
                  <tr key={t.trade_id}>
                    <td className="p">
                      {base(t.pair)}
                      {t.trading_mode === "futures" ? <SideTag short={t.is_short} /> : null}
                    </td>
                    <td>{when(t.open_date)}</td>
                    <td>{when(t.close_date)}</td>
                    <td className="r">{price(t.open_rate)}</td>
                    <td className="r">{price(t.close_rate)}</td>
                    {fut && <td className="r">{t.leverage}×</td>}
                    <td className={"r " + sign(t.profit_abs)}>
                      {t.profit_abs > 0 ? "+" : ""}
                      {money(t.profit_abs)}
                    </td>
                    <td className={"r " + sign(t.profit_ratio)}>{pct(t.profit_ratio)}</td>
                    <td className="t">{WHY[t.exit_reason] || t.exit_reason}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <div className="pager">
            <span>
              第 {page + 1} / {pages} 页
            </span>
            <button className="ghost sm" disabled={page === 0} onClick={() => setPage((p) => Math.max(0, p - 1))}>
              上一页
            </button>
            <button
              className="ghost sm"
              disabled={page + 1 >= pages}
              onClick={() => setPage((p) => p + 1)}
            >
              下一页
            </button>
          </div>
        </>
      ) : (
        <Empty>还没有平仓的交易</Empty>
      )}
    </Card>
  );
}
