export type Channel = {
  id: string;
  name: string;
  provider: string;
  api_protocol: string;
  reasoning_effort: string;
  claude_thinking: string;
  claude_effort: string;
  claude_thinking_budget: number;
  stream: boolean;
  model: string;
  base_url: string;
  api_key: string | null;
};

export type Cfg = {
  channels: Channel[];
  active_channel: string;
  provider: string;
  api_protocol: string;
  reasoning_effort: string;
  claude_thinking: string;
  claude_effort: string;
  claude_thinking_budget: number;
  stream: boolean;
  model: string;
  base_url: string;
  api_key: string | null;
  interval: number;
  delay_seconds: number;
  schedule_mode: "fixed" | "ai";
  ai_min_minutes: number;
  ai_max_minutes: number;
  min_confidence: number;
  daily_profit_target_pct: number;
  max_daily_loss_pct: number;
  max_drawdown_pct: number;
  max_consecutive_losses: number;
  shadow: boolean;
  review: boolean;
  hooks: Hooks;
  enabled: boolean;
};

export type Hooks = {
  enabled: boolean;
  adverse_move: boolean;
  profit_milestone: boolean;
  position_closed: boolean;
  big_move_pct: number;
  min_gap_minutes: number;
  max_per_hour: number;
  webhook_token: string | null;
  webhook_set: boolean;
};

export const HOOK_LABEL: Record<string, string> = {
  adverse_move: "浮亏逼近止损",
  profit_milestone: "浮盈走出一个止损距离",
  position_closed: "仓位被止损/止盈平掉",
  big_move: "价格大幅波动",
  webhook: "外部 Webhook",
  ai_watch: "AI 自设条件",
};

export type Watch = {
  type: string;
  pair: string;
  price: number;
  interval?: string;
  note: string;
  set_utc?: string;
  expires_utc?: string;
};

const WATCH_KIND: Record<string, string> = {
  price_above: "现价涨到",
  price_below: "现价跌到",
  close_above: "收盘高于",
  close_below: "收盘低于",
};

/** One AI-set condition in words, e.g. "ETH 1h 收盘高于 2450". */
export function watchText(w: Watch) {
  const coin = (w.pair || "").split("/")[0];
  return `${coin}${w.interval ? " " + w.interval : ""} ${WATCH_KIND[w.type] || w.type} ${w.price}`;
}

export type AiData = {
  config: Cfg;
  state: { running: boolean; last_run: string | null; next_run: string | null; next_reason?: string | null; last_error: string | null };
  watches?: Watch[];
  position_count: number;
  max_open: number;
  log: any[];
};

export type UsageDay = {
  date: string;
  calls: number;
  failed: number;
  by_source: Record<string, number>;
  input: number;
  cache_read: number;
  cache_write: number;
  output: number;
};

export const DEFAULT_MODEL: Record<string, string> = { anthropic: "claude-opus-5-5", openai: "deepseek-chat" };
