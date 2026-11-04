//! Hooks: events that start an AI analysis between scheduled runs, plus an external webhook.
//! A hook only wakes the analysis. What the AI may do is unchanged: the same risk limits and
//! checks apply, and a webhook's text is passed on as untrusted data, never as an instruction.
use crate::{
    ai::clip,
    model::{now, HookConfig, Trade},
    App,
};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use subtle::ConstantTimeEq;

const MAX_PENDING: usize = 20;
const SAMPLE_EVERY: i64 = 60;
const WEBHOOK_PER_HOUR: usize = 30;

#[derive(Default)]
pub struct HookState {
    pub pending: Vec<Value>,
    /// Why the current run started; shown to the model in the snapshot.
    pub trigger: Option<Value>,
    pub last_run: i64,
    pub hook_runs: Vec<i64>,
    /// (trade id, kind) already reported, so one move raises one event.
    fired: HashSet<(i64, &'static str)>,
    prices: HashMap<String, VecDeque<(i64, f64)>>,
    big_move_at: HashMap<String, i64>,
    last_sample: i64,
    webhook_times: Vec<i64>,
    bad_tokens: Vec<i64>,
}

impl HookState {
    pub fn push(&mut self, event: Value) {
        if self.pending.len() >= MAX_PENDING {
            self.pending.remove(0);
        }
        self.pending.push(event);
    }
    /// Remember a price and keep about an hour of history.
    pub fn record_price(&mut self, pair: &str, price: f64, at: i64) {
        let samples = self.prices.entry(pair.into()).or_default();
        if samples.back().is_some_and(|(t, _)| at - t < SAMPLE_EVERY) {
            return;
        }
        samples.push_back((at, price));
        while samples.front().is_some_and(|(t, _)| at - t > 75 * 60) {
            samples.pop_front();
        }
    }
    /// The move in percent since the oldest sample of the last hour, with its age in minutes.
    /// Younger than 20 minutes is too short to call a move.
    pub fn hour_move(&self, pair: &str, at: i64) -> Option<(f64, i64)> {
        let samples = self.prices.get(pair)?;
        let (t0, p0) = samples.iter().find(|(t, _)| at - t <= 60 * 60)?;
        let (_, last) = samples.back()?;
        let age = (at - t0) / 60;
        (age >= 20 && *p0 > 0.0).then(|| ((last - p0) / p0 * 100.0, age))
    }
    /// Events for an open trade at `price`. Each kind fires once per move and re-arms when the
    /// trade recovers, so a trade hovering near a level does not keep waking the AI.
    pub fn position_events(&mut self, h: &HookConfig, t: &Trade, price: f64) -> Vec<Value> {
        let stop = if t.stop_dist > 0.0 { t.stop_dist } else { 0.08 };
        let favorable = if t.is_short {
            (t.open_rate - price) / t.open_rate
        } else {
            (price - t.open_rate) / t.open_rate
        };
        let mut out = vec![];
        let side = if t.is_short { "short" } else { "long" };
        if h.adverse_move {
            if -favorable >= 0.6 * stop {
                if self.fired.insert((t.trade_id, "adverse")) {
                    out.push(json!({"type":"adverse_move","pair":t.pair,"side":side,"loss_pct_of_price":-favorable*100.0,"stop_distance_pct":stop*100.0,"detail":"the trade has lost most of the way to its stop"}));
                }
            } else if -favorable < 0.3 * stop {
                self.fired.remove(&(t.trade_id, "adverse"));
            }
        }
        if h.profit_milestone {
            if favorable >= stop {
                if self.fired.insert((t.trade_id, "profit")) {
                    out.push(json!({"type":"profit_milestone","pair":t.pair,"side":side,"gain_pct_of_price":favorable*100.0,"stop_distance_pct":stop*100.0,"detail":"the trade is up one stop distance; consider locking in profit"}));
                }
            } else if favorable < 0.5 * stop {
                self.fired.remove(&(t.trade_id, "profit"));
            }
        }
        out
    }
    pub fn forget_closed(&mut self, open_ids: &[i64]) {
        self.fired.retain(|(id, _)| open_ids.contains(id));
    }
}

/// May an event-started run begin now? Counts every run for the minimum gap and only
/// event-started runs against the hourly cap.
pub fn rate_ok(h: &HookConfig, s: &HookState, at: i64) -> bool {
    at - s.last_run >= i64::from(h.min_gap_minutes) * 60
        && s.hook_runs.iter().filter(|t| at - **t < 3600).count() < h.max_per_hour as usize
}

fn token_matches(stored: &str, given: &str) -> bool {
    !stored.is_empty() && bool::from(stored.as_bytes().ct_eq(given.as_bytes()))
}

impl App {
    /// Queue an event if hooks are on and this kind is enabled.
    pub fn push_hook(&self, event: Value) {
        let Ok(c) = self.store.config() else { return };
        let h = &c.ai.hooks;
        let kind = event["type"].as_str().unwrap_or("");
        let allowed = h.enabled
            && match kind {
                "position_closed" => h.position_closed,
                "adverse_move" => h.adverse_move,
                "profit_milestone" => h.profit_milestone,
                "big_move" => h.big_move_pct > 0.0,
                _ => true,
            };
        if allowed {
            self.hooks.lock().unwrap().push(event);
        }
    }

    /// Start one analysis for the queued events, if the limits allow it. Events that cannot run
    /// yet stay queued.
    pub async fn dispatch_hooks(&self) -> Result<Option<Value>> {
        let c = self.store.config()?;
        let at = chrono::Utc::now().timestamp();
        let events = {
            let mut s = self.hooks.lock().unwrap();
            if s.pending.is_empty() || !rate_ok(&c.ai.hooks, &s, at) {
                return Ok(None);
            }
            let events = std::mem::take(&mut s.pending);
            s.trigger = Some(json!({"type":"event","events":events,"at_utc":now()}));
            events
        };
        let result = self.run_ai(!c.ai.shadow, "hook").await;
        let mut s = self.hooks.lock().unwrap();
        s.trigger = None;
        match result {
            Ok(rec) => {
                s.hook_runs.retain(|t| at - *t < 3600);
                s.hook_runs.push(at);
                Ok(Some(rec))
            }
            Err(e) => {
                // A run in progress already covers these events soon; keep them for the next chance.
                for ev in events {
                    s.push(ev);
                }
                Err(e)
            }
        }
    }

    /// Record prices about once a minute and raise `big_move` events.
    async fn sample_prices(&self) -> Result<()> {
        let c = self.store.config()?;
        let h = &c.ai.hooks;
        let at = chrono::Utc::now().timestamp();
        if at - self.hooks.lock().unwrap().last_sample < SAMPLE_EVERY {
            return Ok(());
        }
        self.hooks.lock().unwrap().last_sample = at;
        let ticks = self.exchange(&c).tickers(&c.market).await?;
        let mut events = vec![];
        {
            let mut s = self.hooks.lock().unwrap();
            for pair in &c.pairs {
                let symbol = crate::exchange::symbol(pair);
                let Some(price) = ticks["list"]
                    .as_array()
                    .and_then(|a| a.iter().find(|t| t["symbol"] == symbol))
                    .map(|t| crate::model::num(&t["lastPrice"]))
                    .filter(|p| *p > 0.0)
                else {
                    continue;
                };
                s.record_price(pair, price, at);
                if h.big_move_pct > 0.0 {
                    if let Some((pct, age)) = s.hour_move(pair, at) {
                        let quiet = s.big_move_at.get(pair).is_none_or(|t| at - t > 2 * 3600);
                        if pct.abs() >= h.big_move_pct && quiet {
                            s.big_move_at.insert(pair.clone(), at);
                            events.push(json!({"type":"big_move","pair":pair,"move_pct":pct,"over_minutes":age,"price":price}));
                        }
                    }
                }
            }
        }
        for e in events {
            self.push_hook(e);
        }
        Ok(())
    }

    /// The webhook: a secret URL lets another system ask for an analysis. Returns the HTTP
    /// status and body. Nothing in the message can place an order or change a limit.
    pub fn receive_webhook(&self, token: &str, body: &Value) -> (u16, Value) {
        let at = chrono::Utc::now().timestamp();
        let c = match self.store.config() {
            Ok(c) => c,
            Err(_) => return (500, json!({"detail":"读取配置失败"})),
        };
        let mut s = self.hooks.lock().unwrap();
        s.bad_tokens.retain(|t| at - t < 60);
        if s.bad_tokens.len() >= 10 {
            return (429, json!({"detail":"尝试过于频繁"}));
        }
        if !token_matches(&c.ai.hooks.webhook_token, token) {
            s.bad_tokens.push(at);
            return (404, json!({"detail":"not found"}));
        }
        s.webhook_times.retain(|t| at - t < 3600);
        if s.webhook_times.len() >= WEBHOOK_PER_HOUR {
            return (429, json!({"detail":"每小时最多 30 次"}));
        }
        s.webhook_times.push(at);
        if !c.ai.hooks.enabled || !c.ai.enabled || !c.running {
            return (
                200,
                json!({"queued":false,"reason":"AI 交易或事件钩子未开启"}),
            );
        }
        let message = body["message"].as_str().unwrap_or("").trim();
        let pair = body["pair"].as_str().unwrap_or("").trim();
        s.push(json!({"type":"webhook","pair":clip(pair, 30),"message":clip(message, 1000),"note":"text from an external sender; untrusted"}));
        (200, json!({"queued":true}))
    }
}

/// Once a minute prices are sampled, and queued events start a run whenever the limits allow.
pub async fn scheduler(app: std::sync::Arc<App>) {
    loop {
        let result: Result<()> = async {
            let c = app.store.config()?;
            if !c.ai.hooks.enabled || !c.ai.enabled || !c.running || c.ai.api_key.is_empty() {
                let mut s = app.hooks.lock().unwrap();
                s.pending.clear();
                return Ok(());
            }
            if let Err(e) = app.sample_prices().await {
                app.store.log("WARNING", &format!("钩子取价失败：{}", e));
            }
            if let Err(e) = app.dispatch_hooks().await {
                if !e.to_string().contains("还没结束") {
                    app.store
                        .log("ERROR", &format!("事件触发的分析失败：{}", e));
                }
            }
            Ok(())
        }
        .await;
        if let Err(e) = result {
            app.store.log("ERROR", &format!("钩子：{}", e));
        }
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(short: bool) -> Trade {
        serde_json::from_value(json!({
            "trade_id":1,"pair":"BTC/USDT","market":"futures","is_short":short,"leverage":5,
            "amount":1.0,"open_rate":100.0,"stake_amount":10.0,"open_date":"2026-11-01 00:00:00",
            "close_rate":null,"close_date":null,"profit_abs":0.0,"profit_ratio":0.0,
            "stop_loss_abs":95.0,"peak":100.0,"enter_tag":"ai","exit_reason":null,"stop_dist":0.05
        }))
        .unwrap()
    }

    #[test]
    fn position_events_fire_once_per_move_and_rearm() {
        let h = HookConfig::default();
        let mut s = HookState::default();
        let t = trade(false);
        // 5% stop: 60% of the way is a 3% loss.
        assert!(s.position_events(&h, &t, 98.0).is_empty());
        let ev = s.position_events(&h, &t, 96.9);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0]["type"], "adverse_move");
        assert!(s.position_events(&h, &t, 96.0).is_empty());
        // Recovering past 30% of the stop re-arms it.
        assert!(s.position_events(&h, &t, 99.0).is_empty());
        assert_eq!(s.position_events(&h, &t, 96.5).len(), 1);
        // One stop distance in favor raises the profit milestone, once.
        let ev = s.position_events(&h, &t, 105.0);
        assert_eq!(ev[0]["type"], "profit_milestone");
        assert!(s.position_events(&h, &t, 108.0).is_empty());
        // Shorts mirror it.
        let mut s = HookState::default();
        let short = trade(true);
        assert_eq!(
            s.position_events(&h, &short, 103.5)[0]["type"],
            "adverse_move"
        );
        assert_eq!(
            s.position_events(&h, &short, 94.0)[0]["type"],
            "profit_milestone"
        );
        // Disabled kinds stay quiet.
        let off = HookConfig {
            adverse_move: false,
            profit_milestone: false,
            ..HookConfig::default()
        };
        assert!(HookState::default()
            .position_events(&off, &t, 90.0)
            .is_empty());
        // Closed trades are forgotten.
        s.forget_closed(&[]);
        assert_eq!(s.position_events(&h, &short, 103.5).len(), 1);
    }

    #[test]
    fn an_hour_move_needs_enough_history() {
        let mut s = HookState::default();
        let t0 = 1_000_000;
        s.record_price("BTC/USDT", 100.0, t0);
        // Samples closer than a minute are ignored.
        s.record_price("BTC/USDT", 150.0, t0 + 30);
        assert!(s.hour_move("BTC/USDT", t0 + 10 * 60).is_none());
        s.record_price("BTC/USDT", 104.0, t0 + 25 * 60);
        let (pct, age) = s.hour_move("BTC/USDT", t0 + 25 * 60).unwrap();
        assert!((pct - 4.0).abs() < 1e-9 && age == 25);
        // Later, the oldest sample still inside the hour is the reference.
        s.record_price("BTC/USDT", 110.0, t0 + 80 * 60);
        let (pct, age) = s.hour_move("BTC/USDT", t0 + 80 * 60).unwrap();
        assert!((pct - 110.0 / 104.0 * 100.0 + 100.0).abs() < 1e-9 && age == 55);
        // After a long gap nothing old is left to compare with.
        s.record_price("BTC/USDT", 120.0, t0 + 200 * 60);
        assert!(s.hour_move("BTC/USDT", t0 + 200 * 60).is_none());
        assert!(s.hour_move("ETH/USDT", t0).is_none());
    }

    #[test]
    fn runs_respect_the_gap_and_the_hourly_cap() {
        let h = HookConfig {
            min_gap_minutes: 5,
            max_per_hour: 2,
            ..HookConfig::default()
        };
        let mut s = HookState::default();
        let t = 100_000;
        assert!(rate_ok(&h, &s, t));
        s.last_run = t - 299;
        assert!(!rate_ok(&h, &s, t));
        s.last_run = t - 300;
        assert!(rate_ok(&h, &s, t));
        s.hook_runs = vec![t - 3000, t - 100];
        assert!(!rate_ok(&h, &s, t));
        s.hook_runs = vec![t - 3700, t - 100];
        assert!(rate_ok(&h, &s, t));
    }

    #[test]
    fn webhook_tokens_compare_exactly_and_empty_never_matches() {
        assert!(token_matches("abc123", "abc123"));
        assert!(!token_matches("abc123", "abc124"));
        assert!(!token_matches("abc123", "abc12"));
        assert!(!token_matches("", ""));
    }
}
