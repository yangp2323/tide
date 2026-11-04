use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub market: String,
    pub pairs: Vec<String>,
    pub max_open: usize,
    pub leverage: u32,
    pub short: bool,
    pub capital: Option<f64>,
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
            max_open: 3,
            leverage: 1,
            short: false,
            capital: None,
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
    pub model: String,
    pub api_key: String,
    pub base_url: String,
    pub enabled: bool,
    pub interval: u32,
    pub delay_seconds: u32,
    pub min_confidence: f64,
    pub max_daily_loss_pct: f64,
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
            model: "claude-opus-5-5".into(),
            api_key: String::new(),
            base_url: String::new(),
            enabled: false,
            interval: 240,
            delay_seconds: 180,
            min_confidence: 0.6,
            max_daily_loss_pct: 5.0,
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
    pub peak: f64,
    pub enter_tag: String,
    pub exit_reason: Option<String>,
    #[serde(default)]
    pub entry_fee: f64,
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
