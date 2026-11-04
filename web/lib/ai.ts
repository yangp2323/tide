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
  min_confidence: number;
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
};

export type AiData = {
  config: Cfg;
  state: { running: boolean; last_run: string | null; next_run: string | null; last_error: string | null };
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
