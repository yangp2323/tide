//! Hooks: events that start an AI analysis between scheduled runs, plus an external webhook.
//! A hook only wakes the analysis. What the AI may do is unchanged: the same risk limits and
//! checks apply, and a webhook's text is passed on as untrusted data, never as an instruction.
//! In AI scheduling mode the model also sets its own wake-up conditions (watches): price levels
//! and candle closes checked here. A watch only wakes the analysis, exactly like a hook.
use crate::{
    ai::clip,
    model::{now, HookConfig, Trade},
    App,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use subtle::ConstantTimeEq;

const MAX_PENDING: usize = 20;
const SAMPLE_EVERY: i64 = 60;
const WEBHOOK_PER_HOUR: usize = 30;
pub const MAX_WATCHES: usize = 10;
const WATCH_KEY: &str = "ai_watches";

/// A condition the model asked to be woken by. It fires once and is then removed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Watch {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub pair: String,
    pub price: f64,
    /// Candle interval for the close conditions: 15m, 1h or 4h.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<String>,
    pub note: String,
    pub set_at: i64,
    pub expires_at: i64,
    /// Open time (ms) of the newest closed candle already compared, so each close is judged once.
    #[serde(default)]
    pub checked_close: i64,
}
/// The model's current watches and the ones it asked for last time that were refused.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WatchList {
    pub watches: Vec<Watch>,
    pub rejected: Vec<Value>,
}

fn interval_minutes(interval: &str) -> Option<i64> {
    match interval {
        "15m" => Some(15),
        "1h" => Some(60),
        "4h" => Some(240),
        _ => None,
    }
}
pub fn utc(at: i64) -> String {
    chrono::DateTime::from_timestamp(at, 0)
        .unwrap_or_default()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// Checks the watches a run returned. `prices` holds each whitelisted pair's real-time price
/// from that run's snapshot. Invalid entries are refused one by one with the reason, so one
/// mistake does not drop the rest.
pub fn parse_watches(
    raw: &[Value],
    pairs: &[String],
    prices: &HashMap<String, f64>,
    at: i64,
) -> (Vec<Watch>, Vec<Value>) {
    let mut ok: Vec<Watch> = vec![];
    let mut rejected = vec![];
    for (i, w) in raw.iter().enumerate() {
        match check_watch(w, pairs, prices, at, i, ok.len()) {
            Ok(watch) => ok.push(watch),
            Err(why) => {
                let mut r = json!({});
                for k in ["type", "pair", "price", "interval"] {
                    if !w[k].is_null() {
                        r[k] = w[k].clone();
                    }
                }
                if let Some(n) = w["note"].as_str() {
                    r["note"] = json!(clip(n, 200));
                }
                r["rejected_because"] = json!(why);
                rejected.push(r);
            }
        }
    }
    (ok, rejected)
}
fn check_watch(
    w: &Value,
    pairs: &[String],
    prices: &HashMap<String, f64>,
    at: i64,
    i: usize,
    accepted: usize,
) -> std::result::Result<Watch, &'static str> {
    let kind = w["type"].as_str().unwrap_or("");
    let price_kind = matches!(kind, "price_above" | "price_below");
    if !price_kind && !matches!(kind, "close_above" | "close_below") {
        return Err("type 必须是 price_above、price_below、close_above 或 close_below");
    }
    let given = w["pair"].as_str().unwrap_or("").to_uppercase();
    let pair = pairs
        .iter()
        .find(|p| **p == given || p.split(':').next() == Some(given.as_str()))
        .ok_or("pair 不在白名单内")?;
    let price = w["price"]
        .as_f64()
        .filter(|p| p.is_finite() && *p > 0.0)
        .ok_or("price 必须是正数")?;
    let interval = if price_kind {
        None
    } else {
        let iv = w["interval"].as_str().unwrap_or("");
        interval_minutes(iv).ok_or("收盘条件的 interval 必须是 15m、1h 或 4h")?;
        Some(iv.to_string())
    };
    if price_kind {
        let last = prices
            .get(pair)
            .copied()
            .filter(|p| *p > 0.0)
            .ok_or("读不到该币现价，不能设价格条件")?;
        if kind == "price_above" && price <= last {
            return Err("price_above 的价格必须高于现价");
        }
        if kind == "price_below" && price >= last {
            return Err("price_below 的价格必须低于现价");
        }
    }
    let hours = match &w["expires_hours"] {
        Value::Null => 24.0,
        v => v
            .as_f64()
            .filter(|h| h.is_finite())
            .ok_or("expires_hours 必须是数字")?,
    }
    .clamp(1.0, 24.0);
    if accepted >= MAX_WATCHES {
        return Err("条件数量超过上限");
    }
    Ok(Watch {
        id: format!("w{}-{}", at, i),
        kind: kind.into(),
        pair: pair.clone(),
        price,
        interval,
        note: clip(w["note"].as_str().unwrap_or(""), 200),
        set_at: at,
        expires_at: at + (hours * 3600.0) as i64,
        checked_close: 0,
    })
}

impl Watch {
    /// Has the real-time price reached a price condition?
    pub fn price_hit(&self, price: f64) -> bool {
        match self.kind.as_str() {
            "price_above" => price >= self.price,
            "price_below" => price <= self.price,
            _ => false,
        }
    }
    /// For a close condition: the open time (ms) of the newest candle that has closed by `at`,
    /// if it closed after the watch was set and has not been compared yet.
    pub fn close_due(&self, at: i64) -> Option<i64> {
        let step = interval_minutes(self.interval.as_deref()?)? * 60;
        let open = at.div_euclid(step) * step - step;
        (open + step > self.set_at && open * 1000 > self.checked_close).then_some(open * 1000)
    }
    pub fn close_hit(&self, close: f64) -> bool {
        match self.kind.as_str() {
            "close_above" => close > self.price,
            "close_below" => close < self.price,
            _ => false,
        }
    }
    /// How the watch reads for the model and the page.
    pub fn view(&self) -> Value {
        let mut v = json!({"type":self.kind,"pair":self.pair,"price":self.price,"note":self.note,"set_utc":utc(self.set_at),"expires_utc":utc(self.expires_at)});
        if let Some(iv) = &self.interval {
            v["interval"] = json!(iv);
        }
        v
    }
}

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
        // How far the stop is from entry now: it moves when the AI tightens it or it trails.
        let to_stop = if t.stop_loss_abs > 0.0 {
            let gap = if t.is_short {
                t.stop_loss_abs - t.open_rate
            } else {
                t.open_rate - t.stop_loss_abs
            };
            gap / t.open_rate
        } else {
            stop
        };
        let favorable = if t.is_short {
            (t.open_rate - price) / t.open_rate
        } else {
            (price - t.open_rate) / t.open_rate
        };
        let mut out = vec![];
        let side = if t.is_short { "short" } else { "long" };
        if h.adverse_move {
            if to_stop <= 0.0 {
                // The stop is already at or past entry; there is no loss left to warn about.
            } else if -favorable >= 0.6 * to_stop {
                if self.fired.insert((t.trade_id, "adverse")) {
                    out.push(json!({"type":"adverse_move","pair":t.pair,"side":side,"loss_pct_of_price":-favorable*100.0,"stop_distance_pct":to_stop*100.0,"detail":"the trade has lost most of the way to its current stop"}));
                }
            } else if -favorable < 0.3 * to_stop {
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
    pub fn watch_list(&self) -> Result<WatchList> {
        Ok(match self.store.state(WATCH_KEY)? {
            Some(s) => serde_json::from_str(&s)?,
            None => WatchList::default(),
        })
    }
    /// Replace the watches with the ones a run returned.
    pub fn set_watches(&self, list: &WatchList) -> Result<()> {
        let _guard = self.hooks.lock().unwrap();
        self.store
            .set_state(WATCH_KEY, &serde_json::to_string(list)?)
    }
    /// Queue an event if its kind is enabled. The model's own watches follow the scheduling
    /// mode instead of the hook switches, because the model set them.
    pub fn push_hook(&self, event: Value) {
        let Ok(c) = self.store.config() else { return };
        let h = &c.ai.hooks;
        let kind = event["type"].as_str().unwrap_or("");
        let allowed = if kind == "ai_watch" {
            c.ai.ai_schedule()
        } else {
            h.enabled
                && match kind {
                    "position_closed" => h.position_closed,
                    "adverse_move" => h.adverse_move,
                    "profit_milestone" => h.profit_milestone,
                    "big_move" => h.big_move_pct > 0.0,
                    _ => true,
                }
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

    /// Compare the model's watches with the prices just sampled and with newly closed candles.
    /// Fired and expired watches are removed; the list is re-read before writing, so a run that
    /// replaced it meanwhile is not undone.
    pub async fn check_watches(&self, prices: &HashMap<String, f64>, at: i64) -> Result<()> {
        let c = self.store.config()?;
        let list = self.watch_list()?;
        if list.watches.is_empty() {
            return Ok(());
        }
        let ex = self.exchange(&c);
        let mut gone = HashSet::new();
        let mut checked = HashMap::new();
        let mut events = vec![];
        for w in &list.watches {
            if at >= w.expires_at {
                gone.insert(w.id.clone());
                continue;
            }
            if w.interval.is_none() {
                if let Some(price) = prices.get(&w.pair).filter(|p| w.price_hit(**p)) {
                    gone.insert(w.id.clone());
                    events.push(
                        json!({"type":"ai_watch","pair":w.pair,"condition":w.view(),"price":price}),
                    );
                }
                continue;
            }
            let Some(due) = w.close_due(at) else { continue };
            let minutes = interval_minutes(w.interval.as_deref().unwrap_or("")).unwrap_or(60);
            let candles = match ex
                .candles(&c.market, &w.pair, minutes as u32, 3, None)
                .await
            {
                Ok(cs) => cs,
                Err(e) => {
                    self.store
                        .log("WARNING", &format!("检查 AI 条件时读取 K 线失败：{}", e));
                    continue;
                }
            };
            // The exchange can lag a few seconds behind the close; try again next minute.
            let Some(last) = candles.last().filter(|k| k.time >= due) else {
                continue;
            };
            checked.insert(w.id.clone(), last.time);
            if w.close_hit(last.close) {
                gone.insert(w.id.clone());
                events.push(json!({"type":"ai_watch","pair":w.pair,"condition":w.view(),"close":last.close,"candle_open_utc":utc(last.time / 1000)}));
            }
        }
        if gone.is_empty() && checked.is_empty() {
            return Ok(());
        }
        {
            let _guard = self.hooks.lock().unwrap();
            let mut fresh = self.watch_list()?;
            fresh.watches.retain(|w| !gone.contains(&w.id));
            for w in fresh.watches.iter_mut() {
                if let Some(t) = checked.get(&w.id) {
                    w.checked_close = *t;
                }
            }
            self.store
                .set_state(WATCH_KEY, &serde_json::to_string(&fresh)?)?;
        }
        for e in events {
            self.push_hook(e);
        }
        Ok(())
    }

    /// Record prices about once a minute, raise `big_move` events and check the model's watches.
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
        let mut prices = HashMap::new();
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
                prices.insert(pair.clone(), price);
                if h.enabled && h.big_move_pct > 0.0 {
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
        if c.ai.ai_schedule() {
            self.check_watches(&prices, at).await?;
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
        // Alerts are often JSON without a message field, or a bare number: pass on what was sent.
        let sent = body.to_string();
        let message = match body["message"].as_str().map(str::trim) {
            Some(m) if !m.is_empty() => m,
            _ => match &body {
                Value::String(s) => s.trim(),
                Value::Object(o) if !o.is_empty() => sent.as_str(),
                Value::Number(_) | Value::Bool(_) => sent.as_str(),
                _ => "",
            },
        };
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
            // The model's own watches need prices too, even with the built-in hooks off.
            let watching = c.ai.hooks.enabled || c.ai.ai_schedule();
            if !watching || !c.ai.enabled || !c.running || c.ai.api_key.is_empty() {
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
            "stop_loss_abs":if short{105.0}else{95.0},"peak":100.0,"enter_tag":"ai","exit_reason":null,"stop_dist":0.05
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
    fn a_tightened_stop_moves_the_warning_with_it() {
        let h = HookConfig::default();
        let mut tight = trade(false);
        tight.stop_loss_abs = 99.0;
        let mut s = HookState::default();
        assert!(s.position_events(&h, &tight, 99.5).is_empty());
        assert_eq!(
            s.position_events(&h, &tight, 99.3)[0]["type"],
            "adverse_move"
        );
        // A stop already on the profit side leaves nothing to warn about.
        let mut locked = trade(false);
        locked.stop_loss_abs = 101.0;
        assert!(HookState::default()
            .position_events(&h, &locked, 98.0)
            .is_empty());
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

#[cfg(test)]
mod watch_tests {
    use super::*;

    fn setup() -> (Vec<String>, HashMap<String, f64>) {
        let pairs = vec!["ETH/USDT:USDT".to_string(), "BTC/USDT:USDT".to_string()];
        let prices = HashMap::from([("ETH/USDT:USDT".to_string(), 2400.0)]);
        (pairs, prices)
    }

    #[test]
    fn watches_are_checked_one_by_one() {
        let (pairs, prices) = setup();
        let raw = vec![
            json!({"type":"price_above","pair":"eth/usdt","price":2450,"note":"突破","expires_hours":8}),
            json!({"type":"price_below","pair":"ETH/USDT:USDT","price":2450}),
            json!({"type":"price_above","pair":"DOGE/USDT:USDT","price":1}),
            json!({"type":"close_above","pair":"BTC/USDT:USDT","price":80000,"interval":"1h","expires_hours":99}),
            json!({"type":"close_below","pair":"BTC/USDT:USDT","price":79000,"interval":"2h"}),
            json!({"type":"price_above","pair":"BTC/USDT:USDT","price":90000}),
            json!({"type":"price_above","pair":"ETH/USDT:USDT","price":-1}),
            json!({"type":"buy","pair":"ETH/USDT:USDT","price":2500}),
        ];
        let (ok, rejected) = parse_watches(&raw, &pairs, &prices, 1000);
        assert_eq!(ok.len(), 2);
        assert_eq!(ok[0].pair, "ETH/USDT:USDT");
        assert_eq!(ok[0].expires_at, 1000 + 8 * 3600);
        assert_eq!(ok[1].interval.as_deref(), Some("1h"));
        assert_eq!(ok[1].expires_at, 1000 + 24 * 3600);
        let why: Vec<_> = rejected
            .iter()
            .map(|r| r["rejected_because"].as_str().unwrap())
            .collect();
        assert_eq!(why.len(), 6);
        assert!(why[0].contains("低于现价"));
        assert!(why[1].contains("白名单"));
        assert!(why[2].contains("interval"));
        assert!(why[3].contains("读不到"));
        // At most MAX_WATCHES are kept.
        let many: Vec<_> = (0..12)
            .map(|i| json!({"type":"price_above","pair":"ETH/USDT:USDT","price":2500 + i}))
            .collect();
        let (ok, rejected) = parse_watches(&many, &pairs, &prices, 0);
        assert_eq!((ok.len(), rejected.len()), (MAX_WATCHES, 2));
    }

    #[test]
    fn price_and_close_conditions_fire_on_their_own_terms() {
        let (pairs, prices) = setup();
        let raw = vec![
            json!({"type":"price_above","pair":"ETH/USDT:USDT","price":2450}),
            json!({"type":"close_below","pair":"ETH/USDT:USDT","price":2300,"interval":"1h"}),
        ];
        let at = 10 * 3600 + 600;
        let (ok, _) = parse_watches(&raw, &pairs, &prices, at);
        assert!(!ok[0].price_hit(2449.9) && ok[0].price_hit(2450.0) && ok[0].price_hit(2600.0));
        // No close yet since it was set; the 11:00 candle closes at 12:00.
        let mut close = ok[1].clone();
        assert_eq!(close.close_due(at + 600), None);
        assert_eq!(close.close_due(12 * 3600 + 5), Some(11 * 3600 * 1000));
        assert!(close.close_hit(2299.0) && !close.close_hit(2300.0));
        // Once compared, the same candle is not checked again.
        close.checked_close = 11 * 3600 * 1000;
        assert_eq!(close.close_due(12 * 3600 + 65), None);
        assert_eq!(close.close_due(13 * 3600 + 5), Some(12 * 3600 * 1000));
    }
}
