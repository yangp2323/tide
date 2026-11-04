/** 数字、时间、价格的统一格式化。 */
export const money = (v: number | null | undefined, d = 2) =>
  v == null ? "—" : Number(v).toLocaleString("en-US", { minimumFractionDigits: d, maximumFractionDigits: d });

export const pct = (r: number | null | undefined, d = 2) =>
  r == null ? "—" : (r > 0 ? "+" : "") + (r * 100).toFixed(d) + "%";

export const sign = (v: number | null | undefined) => (v && v > 0 ? "up" : v && v < 0 ? "down" : "");

export const when = (s?: string | null) => (s ? s.replace("T", " ").slice(5, 16) : "—");

export const price = (v: number | null | undefined) =>
  v == null ? "—" : v >= 1000 ? money(v, 1) : v >= 1 ? money(v, 3) : Number(v).toPrecision(4);

export const vol = (v: number) =>
  v >= 1e8 ? (v / 1e8).toFixed(1) + " 亿" : v >= 1e4 ? Math.round(v / 1e4) + " 万" : String(Math.round(v));

export const base = (p: string) => (p || "").split("/")[0];

/** 服务器时间（UTC）→ 本地 MM-DD HH:mm */
export const utc = (s?: string | null) =>
  !s
    ? "—"
    : new Date(s.replace(" ", "T") + "Z").toLocaleString("zh-CN", {
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
      });

/** 本地时分秒 */
export const hms = (ms: number) => new Date(ms).toLocaleTimeString("zh-CN", { hour12: false });

export const WHY: Record<string, string> = {
  exit_signal: "策略信号平仓",
  stop_loss: "止损",
  stoploss_on_exchange: "交易所止损",
  trailing_stop_loss: "移动止盈",
  force_exit: "手动或 AI 平仓",
  roi: "止盈",
  liquidation: "爆仓",
};

export const AI_ACT: Record<string, string> = { open_long: "开多", open_short: "开空", close: "平仓" };
export const AI_STATUS: Record<string, string> = { executed: "已执行", blocked: "风控拦截", failed: "失败" };
export const AI_SOURCE: Record<string, string> = { auto: "定时", manual: "手动", mcp: "外部接口", review: "复盘", hook: "事件触发" };
