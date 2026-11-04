"use client";
import { useEffect, useState } from "react";
import { base, money, pct, price, sign } from "@/lib/fmt";

const R = 44;
const CIRC = 2 * Math.PI * R;

function msOf(tf: string) {
  const n = parseInt(tf, 10) || 4;
  const unit = tf.endsWith("m") ? 60e3 : tf.endsWith("h") ? 3600e3 : 86400e3;
  return n * unit;
}

/** K 线收盘倒计时：策略只在收盘那一刻判断开平仓 */
export function Clock({ timeframe }: { timeframe?: string }) {
  const tf = timeframe || "4h";
  const ms = msOf(tf);
  const [left, setLeft] = useState<number | null>(null);
  const [next, setNext] = useState(0);

  useEffect(() => {
    const step = msOf(tf);
    const tick = () => {
      const now = Date.now();
      const n = Math.ceil(now / step) * step;
      setLeft(n - now);
      setNext(n);
    };
    tick();
    const t = setInterval(tick, 1000);
    return () => clearInterval(t);
  }, [tf]);

  const l = left ?? 0;
  const hh = Math.floor(l / 3600e3);
  const mm = Math.floor((l % 3600e3) / 60e3);
  const ss = Math.floor((l % 60e3) / 1e3);
  const at = next
    ? new Date(next).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })
    : "—";

  return (
    <div className="clock" title="策略只在每根 K 线收盘时判断开仓、平仓">
      <svg viewBox="0 0 100 100" aria-hidden="true">
        <circle cx="50" cy="50" r={R} fill="none" stroke="#e6ebef" strokeWidth="8" />
        <circle
          cx="50"
          cy="50"
          r={R}
          fill="none"
          stroke="#121a26"
          strokeWidth="8"
          strokeLinecap="round"
          strokeDasharray={CIRC.toFixed(1)}
          strokeDashoffset={left == null ? CIRC : (CIRC * l) / ms}
          style={{ transition: "stroke-dashoffset .35s linear" }}
        />
      </svg>
      <div className="c-in">
        <b>
          {left == null
            ? "—"
            : `${hh}:${String(mm).padStart(2, "0")}:${String(ss).padStart(2, "0")}`}
        </b>
        <span>{at} 收盘</span>
      </div>
    </div>
  );
}

type W = {
  pair: string;
  state: string;
  price?: number | null;
  side?: "long" | "short";
  above200?: boolean;
  dist200?: number;
  golden?: boolean;
  gap?: number;
  adx?: number;
  trending?: boolean;
  holding_short?: boolean;
};

function Gate({ on, down, name, val }: { on?: boolean; down?: boolean; name: string; val: string }) {
  return (
    <div className={"g" + (on ? " on" : "") + (down ? " s" : "")}>
      <b>
        {on ? "✓" : "·"} {name}
      </b>
      <span>{val}</span>
    </div>
  );
}

/** 展示每个币的行情指标和 AI 交易状态。 */
export function WatchRow({ x, short }: { x: W; short: boolean }) {
  if (x.state === "loading" || x.state === "nodata") {
    return (
      <div className="w">
        <div className="pair">
          {base(x.pair)}
          <small>{price(x.price)}</small>
        </div>
        <div className="gates" />
        <div className="say">K 线数据准备中，新加的币要几分钟</div>
      </div>
    );
  }

  const isShort = x.side === "short" && short;
  const words: Record<string, string> = {
    holding: x.holding_short ? "持有空单" : "持有多单",
    waiting_ai: "等待 AI 分析",
  };
  const cls = x.state.startsWith("signal") ? "sig" : x.state === "holding" ? "hold" : "";

  return (
    <div className={"w " + cls}>
      <div className="pair">
        {base(x.pair)}
        <small>{price(x.price)}</small>
      </div>
      <div className="gates">
        <Gate on={x.above200} down={isShort && !x.above200} name="EMA200" val={pct(x.dist200, 1)} />
        <Gate on={x.golden} down={isShort && x.golden} name="EMA20/50" val={pct(x.gap, 2)} />
        <Gate on={x.trending} down={isShort && x.trending} name="ADX" val={Math.round(x.adx || 0).toString()} />
      </div>
      <div className="say">{words[x.state] || ""}</div>
    </div>
  );
}

type Day = { date: string; abs_profit: number; trade_count: number };

/** 近 30 天每日盈亏柱状图 */
export function DailyBars({ days }: { days: Day[] }) {
  const rows = [...(days || [])].reverse();
  if (!rows.length) return <div className="empty">还没有每日盈亏记录</div>;
  const mx = Math.max(1e-9, ...rows.map((x) => Math.abs(x.abs_profit)));
  return (
    <>
      <div className="bars">
        {rows.map((x) => (
          <i
            key={x.date}
            className={sign(x.abs_profit)}
            style={{ height: `${x.abs_profit ? Math.max(4, (Math.abs(x.abs_profit) / mx) * 100) : 2}%` }}
            title={`${x.date}  ${money(x.abs_profit)} USDT · ${x.trade_count} 笔`}
          />
        ))}
      </div>
      <div className="bars-axis">
        <span>{rows[0]?.date.slice(5)}</span>
        <span>{rows[rows.length - 1]?.date.slice(5)}</span>
      </div>
    </>
  );
}

/** 回测资金曲线 */
export function Curve({ curve, start }: { curve: [string, number][]; start: number }) {
  if (!curve || curve.length < 2) return null;
  const W = 800;
  const H = 150;
  const vs = curve.map((x) => x[1]);
  const lo = Math.min(start, ...vs);
  const hi = Math.max(start, ...vs);
  const span = hi - lo || 1;
  const X = (i: number) => ((i / (curve.length - 1)) * W).toFixed(1);
  const Y = (v: number) => (H - 6 - ((v - lo) / span) * (H - 12)).toFixed(1);
  const pts = curve.map((x, i) => `${X(i)},${Y(x[1])}`).join(" ");
  const col = vs[vs.length - 1] >= start ? "var(--up)" : "var(--down)";

  return (
    <svg className="curve" viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" role="img" aria-label="资金曲线">
      <line
        x1="0"
        x2={W}
        y1={Y(start)}
        y2={Y(start)}
        stroke="var(--faint)"
        strokeDasharray="4 4"
        vectorEffect="non-scaling-stroke"
      />
      <polygon points={`0,${Y(start)} ${pts} ${W},${Y(start)}`} fill={col} opacity=".08" />
      <polyline
        points={pts}
        fill="none"
        stroke={col}
        strokeWidth="2"
        vectorEffect="non-scaling-stroke"
        strokeLinejoin="round"
      />
    </svg>
  );
}
