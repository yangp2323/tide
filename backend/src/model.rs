use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Hard limits the AI cannot exceed. Direction, how many positions to hold and how much to
/// risk on each are the AI's decisions inside them.
pub const MAX_OPEN: usize = 5;
pub const MAX_RISK_PER_TRADE_PCT: f64 = 2.0;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub market: String,
    pub pairs: Vec<String>,
    pub max_open: usize,
    pub leverage: u32,
    pub short: bool,
    pub capital: Option<f64>,
    /// Maximum loss per trade at its stop, as a percentage of trading equity.
    pub risk_per_trade_pct: f64,
    /// Most open positions in the same direction; 0 means no limit. Correlated
    /// coins move together, so this caps how concentrated the book can be.
    pub max_same_direction: usize,
    pub running: bool,
    pub ai: AiConfig,
    pub exchange: Credentials,
    pub telegram: Telegram,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            market: "spot".into(),
            pairs: vec!["BTC/USDT".into(), "ETH/USDT".into(), "SOL/USDT".into()],
            max_open: MAX_OPEN,
            leverage: 1,
            short: false,
            capital: None,
            risk_per_trade_pct: MAX_RISK_PER_TRADE_PCT,
            max_same_direction: 0,
            running: false,
            ai: AiConfig::default(),
            exchange: Credentials::default(),
            telegram: Telegram::default(),
        }
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Credentials {
    pub key: String,
    pub secret: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Telegram {
    pub enabled: bool,
    pub token: String,
    pub chat_id: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    pub provider: String,
    pub api_protocol: String,
    pub reasoning_effort: String,
    pub claude_thinking: String,
    pub claude_effort: String,
    pub claude_thinking_budget: u32,
    /// Request server-sent events instead of one JSON body, so long thinking
    /// keeps the connection busy and the larger Claude output limit is safe.
    pub stream: bool,
    pub model: String,
    pub api_key: String,
    pub base_url: String,
    pub enabled: bool,
    pub interval: u32,
    pub delay_seconds: u32,
    pub min_confidence: f64,
    pub max_daily_loss_pct: f64,
    /// Pause the bot when equity falls this far below its high-water mark; 0 disables it.
    pub max_drawdown_pct: f64,
    /// Block new entries after this many losing trades in a row; 0 disables it.
    pub max_consecutive_losses: u32,
    /// Analyse and record decisions on schedule without placing any order.
    pub shadow: bool,
    /// Let the AI review its closed trades and keep its own observations in the knowledge base.
    pub review: bool,
    /// Saved API channels. The model fields above always mirror the active one, so code that
    /// reads them (and an older build reading the same file) sees the channel in use.
    pub channels: Vec<Channel>,
    pub active_channel: String,
}
/// One way of reaching a model: provider, endpoint, key and model options.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub api_protocol: String,
    pub reasoning_effort: String,
    pub claude_thinking: String,
    pub claude_effort: String,
    pub claude_thinking_budget: u32,
    pub stream: bool,
    pub model: String,
    pub api_key: String,
    pub base_url: String,
}
impl Default for Channel {
    fn default() -> Self {
        let a = AiConfig::default();
        Self {
            id: String::new(),
            name: String::new(),
            provider: a.provider,
            api_protocol: a.api_protocol,
            reasoning_effort: a.reasoning_effort,
            claude_thinking: a.claude_thinking,
            claude_effort: a.claude_effort,
            claude_thinking_budget: a.claude_thinking_budget,
            stream: a.stream,
            model: a.model,
            api_key: String::new(),
            base_url: String::new(),
        }
    }
}
impl AiConfig {
    fn current_channel(&self, id: &str, name: &str) -> Channel {
        Channel {
            id: id.into(),
            name: name.into(),
            provider: self.provider.clone(),
            api_protocol: self.api_protocol.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            claude_thinking: self.claude_thinking.clone(),
            claude_effort: self.claude_effort.clone(),
            claude_thinking_budget: self.claude_thinking_budget,
            stream: self.stream,
            model: self.model.clone(),
            api_key: self.api_key.clone(),
            base_url: self.base_url.clone(),
        }
    }
    /// The same settings with a different channel's model fields, for validating a channel.
    pub fn with_channel(&self, ch: &Channel) -> AiConfig {
        let mut a = self.clone();
        a.provider = ch.provider.clone();
        a.api_protocol = ch.api_protocol.clone();
        a.reasoning_effort = ch.reasoning_effort.clone();
        a.claude_thinking = ch.claude_thinking.clone();
        a.claude_effort = ch.claude_effort.clone();
        a.claude_thinking_budget = ch.claude_thinking_budget;
        a.stream = ch.stream;
        a.model = ch.model.clone();
        a.api_key = ch.api_key.clone();
        a.base_url = ch.base_url.clone();
        a
    }
    pub fn active(&self) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == self.active_channel)
    }
    /// A config written before channels existed becomes one channel; then the flat model
    /// fields are set from whichever channel is active.
    pub fn normalize(&mut self) {
        if self.channels.is_empty() {
            let ch = self.current_channel("default", "默认");
            self.channels.push(ch);
        }
        if self.active().is_none() {
            self.active_channel = self.channels[0].id.clone();
        }
        let active = self.active().cloned().unwrap();
        let mirrored = self.with_channel(&active);
        *self = mirrored;
    }
}
impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: "anthropic".into(),
            api_protocol: "chat_completions".into(),
            reasoning_effort: "default".into(),
            claude_thinking: "default".into(),
            claude_effort: "default".into(),
            claude_thinking_budget: 4096,
            stream: false,
            model: "claude-opus-5-5".into(),
            api_key: String::new(),
            base_url: String::new(),
            enabled: false,
            interval: 240,
            delay_seconds: 180,
            min_confidence: 0.6,
            max_daily_loss_pct: 5.0,
            max_drawdown_pct: 20.0,
            max_consecutive_losses: 5,
            shadow: false,
            review: true,
            channels: vec![],
            active_channel: String::new(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Trade {
    pub trade_id: i64,
    pub pair: String,
    pub market: String,
    pub is_short: bool,
    pub leverage: u32,
    pub amount: f64,
    pub open_rate: f64,
    pub stake_amount: f64,
    pub open_date: String,
    pub close_rate: Option<f64>,
    pub close_date: Option<String>,
    pub profit_abs: f64,
    pub profit_ratio: f64,
    pub stop_loss_abs: f64,
    /// Take-profit price chosen by the AI; 0 means none.
    #[serde(default)]
    pub take_profit_abs: f64,
    pub peak: f64,
    pub enter_tag: String,
    pub exit_reason: Option<String>,
    #[serde(default)]
    pub entry_fee: f64,
    /// Initial stop distance as a price fraction; 0 for trades opened before ATR stops.
    #[serde(default)]
    pub stop_dist: f64,
    /// Why the AI opened this trade; shown back to it on later runs.
    #[serde(default)]
    pub entry_reason: String,
    /// Open trade: funding already booked into closed slices. Closed trade: its own
    /// funding (negative is a cost). Futures only.
    #[serde(default)]
    pub funding: f64,
}
impl Trade {
    pub fn mark(&self, price: f64) -> Value {
        let mut v = serde_json::to_value(self).unwrap();
        let pnl = (price - self.open_rate) * self.amount * if self.is_short { -1.0 } else { 1.0 };
        v["current_rate"] = json!(price);
        v["profit_abs"] = json!(pnl - self.entry_fee);
        v["profit_ratio"] = json!((pnl - self.entry_fee) / self.stake_amount);
        v["is_open"] = json!(self.close_date.is_none());
        v
    }
}
pub fn num(v: &Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0.0)
}
pub fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}
pub fn day() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}
pub fn mask(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        let c: Vec<_> = s.chars().collect();
        Some(if c.len() > 8 {
            format!(
                "{}…{}",
                c[..4].iter().collect::<String>(),
                c[c.len() - 4..].iter().collect::<String>()
            )
        } else {
            "••••".into()
        })
    }
}
