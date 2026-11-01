"use client";
import Link from "next/link";
import { Clock, DailyBars, WatchRow } from "@/components/widgets";
import { base, money, pct, sign, when } from "@/lib/fmt";
import { useOv } from "@/lib/store";
import { Card, Empty, SideTag, Skeleton } from "@/lib/ui";

export default function OverviewPage() {
  const { d, loaded } = useOv();

  const open: any[] = d.open || [];
  const watch: any[] = d.watch || [];
  const pairs: string[] = d.pairs || [];
  const fut = d.market === "futures";
  const p = d.profit || {};

  const sigL = watch.filter((x) => x.state === "signal_long").map((x) => base(x.pair));
  const sigS = watch.filter((x) => x.state === "signal_short").map((x) => base(x.pair));

  let headline: React.ReactNode = "正在连接机器人…";
  if (loaded) {
    if (d.state === "offline") headline = "机器人没有响应，可能正在重启";
    else if (d.state === "stopped")
      headline = (
        <>
          机器人已暂停，<em>不会开仓，也不会止损</em>
        </>
      );
    else if (sigL.length || sigS.length) {
      const parts: React.ReactNode[] = [];
      if (sigL.length) parts.push(<span key="l"><em>{sigL.join("、")}</em> 出现做多信号</span>);
      if (sigS.length) parts.push(<span key="s"><em>{sigS.join("、")}</em> 出现做空信号</span>);
      headline = (
        <>
          {parts.map((x, i) => (
            <span key={i}>{i > 0 ? "，" : null}{x}</span>
          ))}
          ，下一根 K 线开盘时开仓
        </>
      );
    } else if (open.length) {
      headline = `持有 ${open.map((t) => base(t.pair) + (t.is_short ? "（空）" : "")).join("、")}，盯着其余 ${Math.max(pairs.length - open.length, 0)} 个币`;
    } else {
      headline = `空仓，盯着 ${pairs.length} 个币等待开仓机会`;
    }
  }

  const closedCount = p.winning_trades || 0;
  const n = (p.winning_trades || 0) + (p.losing_trades || 0);

  return (
    <div className="overview-dashboard">
      <div className="grid g3 overview-top">
        <Card>
          <div className="hero">
            <div style={{ flex: 1, minWidth: 0 }}>
              <h3>{headline}</h3>
              <p className="hint">
                {d.error ||
                  `策略只在每根 ${d.timeframe || "4h"} K 线收盘时判断开仓和平仓，右边是距离下一次收盘的时间。`}
              </p>
            </div>
            <Clock timeframe={d.timeframe} />
          </div>
        </Card>

        <Card
          title="当前设置"
          right={
            <Link href="/strategy/" className="note">
              修改
            </Link>
          }
        >
          {!loaded ? (
            <Skeleton lines={4} />
          ) : (
            <div className="kv">
              <span className="muted">市场</span>
              <b>{fut ? "USDT 永续合约（逐仓）" : "现货"}</b>
              <span className="muted">杠杆</span>
              <b className="num">{d.leverage}×</b>
              <span className="muted">方向</span>
              <b>{d.short ? "做多 + 做空" : "只做多"}</b>
              <span className="muted">持仓</span>
              <b>最多 {d.max_open} 笔</b>
              <span className="muted">币种</span>
              <b className="pairs">{pairs.map(base).join(" · ") || "—"}</b>
              <span className="muted">止损</span>
              <b>{fut ? "挂在 Bybit 上" : "由机器人执行"}</b>
            </div>
          )}
        </Card>
      </div>

      <div className="kpis">
        <div className="kpi">
          <div className="k">机器人账户</div>
          <div className="v">
            {d.balance ? (
              <>
                {money(d.balance.total)}
                <small>{d.balance.symbol || "USDT"}</small>
              </>
            ) : (
              "—"
            )}
          </div>
          <div className="s">{d.capital ? `上限 ${d.capital} USDT` : "账户全部余额"}</div>
        </div>
        <div className="kpi">
          <div className="k">已平仓盈亏</div>
          <div className="v">
            {p.profit_closed_coin == null ? (
              "—"
            ) : (
              <span className={sign(p.profit_closed_coin)}>
                {p.profit_closed_coin > 0 ? "+" : ""}
                {money(p.profit_closed_coin)}
              </span>
            )}
          </div>
          <div className="s">{p.profit_closed_ratio_mean == null ? "\u00a0" : `平均每笔 ${pct(p.profit_closed_ratio_mean)}`}</div>
        </div>
        <div className="kpi">
          <div className="k">交易笔数 · 胜率</div>
          <div className="v">
            {p.closed_trade_count == null ? (
              "—"
            ) : (
              <>
                {p.closed_trade_count}
                <small>{n ? Math.round((closedCount / n) * 100) + "%" : "—"}</small>
              </>
            )}
          </div>
          <div className="s">{p.closed_trade_count ? `赚 ${p.winning_trades} · 亏 ${p.losing_trades}` : "还没有平仓"}</div>
        </div>
        <div className="kpi">
          <div className="k">最大回撤</div>
          <div className="v">{p.max_drawdown == null ? "—" : (p.max_drawdown * 100).toFixed(2) + "%"}</div>
          <div className="s">从账户最高点算</div>
        </div>
      </div>

      <Card
        title="盯盘"
        note={d.short ? "价格在 200 线上方找做多机会，下方找做空机会" : "三个条件都满足、并且 20 线刚刚上穿 50 线时开多"}
        className="overview-watch"
      >
        {!loaded ? (
          <Skeleton lines={3} height={40} />
        ) : watch.length ? (
          <div className="watch">
            {watch.map((x) => (
              <WatchRow key={x.pair} x={x} short={!!d.short} />
            ))}
          </div>
        ) : (
          <Empty>{d.state === "offline" ? "机器人离线" : "还没选币"}</Empty>
        )}
        <div className="legend">
          {`趋势：价格和 200 线的距离 · 交叉：20 线和 50 线的距离 · 力度：ADX 大于 ${d.adx_min ?? 20} 才算有趋势`}
        </div>
      </Card>

      <div className="grid g2 overview-bottom">
        <Card title="近 30 天每日盈亏">{loaded ? <DailyBars days={d.daily || []} /> : <Skeleton lines={5} />}</Card>

        <Card
          title="最近交易"
          right={
            <Link href="/trades/" className="note">
              全部
            </Link>
          }
        >
          {!loaded ? (
            <Skeleton lines={4} />
          ) : (d.recent || []).length ? (
            <div className="scroll">
              <table className="tbl">
                <thead>
                  <tr>
                    <th>币</th>
                    <th>平仓</th>
                    <th className="r">盈亏</th>
                  </tr>
                </thead>
                <tbody>
                  {(d.recent || []).map((t: any) => (
                    <tr key={t.trade_id + t.close_date}>
                      <td className="p">
                        {base(t.pair)}
                        {fut ? <SideTag short={t.is_short} /> : null}
                      </td>
                      <td>{when(t.close_date)}</td>
                      <td className={"r " + sign(t.profit_abs)}>
                        {money(t.profit_abs)} · {pct(t.profit_ratio)}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ) : (
            <Empty>还没有平仓的交易</Empty>
          )}
        </Card>
      </div>
    </div>
  );
}
