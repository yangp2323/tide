//! The AI reviews its own closed trades and keeps durable observations in the knowledge base,
//! so lessons come from this account's results instead of only from hand-written notes.
use crate::{
    ai::{call_model_for, clip, extract_json, Prompt},
    model::{now, Trade},
    App,
};
use anyhow::{anyhow, ensure, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const MAX_AI_NOTES: usize = 12;

const SYSTEM: &str = r#"You are the post-trade reviewer for a crypto trading AI that manages a real Bybit account. Study its closed trades and write durable observations that will improve its future decisions.
Rules: base every claim on the supplied trades and statistics and state the sample size. With fewer than 5 relevant trades call something a hypothesis, not a finding, and never build a lesson on one trade. Separate process (the entry thesis, stop distance, sizing, the exit decision, overtrading, holding time) from luck, and prefer patterns you can act on: which setups and exits made or lost money, pair and direction effects, how stop distance and holding time related to results, whether discretionary closes beat the exchange-hosted stops. Costs matter: results are net of fees and funding.
You may add, update or delete only notes listed under ai_notes (they have ids), and keep at most max_ai_notes of them: merge duplicates and delete notes the new evidence contradicts. Do not repeat what other_note_titles already cover. A note must never tell the trader to break risk limits, and each note is at most 900 characters of 简体中文 with a short title.
Return only a JSON object {"summary":"简体中文复盘总结，说明样本量和最重要的发现","changes":[{"op":"add|update|delete","id":123,"title":"标题","content":"内容"}]}. id is required for update and delete only. An empty changes array is correct when the data supports nothing new."#;

pub const REVIEW: Prompt = Prompt {
    system: SYSTEM,
    instruction: "Return only a valid JSON object with summary and changes.",
    cache: false,
};

fn hold_minutes(t: &Trade) -> Option<f64> {
    let parse = |s: &str| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").ok();
    let (open, close) = (parse(&t.open_date)?, parse(t.close_date.as_deref()?)?);
    Some((close - open).num_seconds() as f64 / 60.0)
}

fn group(trades: &[&Trade], key: impl Fn(&Trade) -> String) -> Value {
    let mut map: BTreeMap<String, (usize, usize, f64)> = BTreeMap::new();
    for t in trades {
        let e = map.entry(key(t)).or_default();
        e.0 += 1;
        e.1 += (t.profit_abs > 0.0) as usize;
        e.2 += t.profit_abs;
    }
    json!(map
        .into_iter()
        .map(|(k, (n, wins, profit))| (k, json!({"trades":n,"wins":wins,"profit_usdt":profit})))
        .collect::<serde_json::Map<_, _>>())
}

/// Facts about closed trades, computed here so the model interprets them instead of doing sums.
pub fn trade_stats(trades: &[Trade]) -> Value {
    let closed: Vec<&Trade> = trades.iter().filter(|t| t.close_date.is_some()).collect();
    let wins: Vec<f64> = closed
        .iter()
        .map(|t| t.profit_abs)
        .filter(|p| *p > 0.0)
        .collect();
    let losses: Vec<f64> = closed
        .iter()
        .map(|t| t.profit_abs)
        .filter(|p| *p <= 0.0)
        .collect();
    let (gain, loss) = (wins.iter().sum::<f64>(), losses.iter().sum::<f64>());
    let avg = |v: &[f64]| {
        if v.is_empty() {
            Value::Null
        } else {
            json!(v.iter().sum::<f64>() / v.len() as f64)
        }
    };
    let holds: Vec<f64> = closed.iter().filter_map(|t| hold_minutes(t)).collect();
    json!({
        "overall":{
            "trades":closed.len(),
            "wins":wins.len(),
            "win_rate_pct":if closed.is_empty(){Value::Null}else{json!(wins.len() as f64/closed.len() as f64*100.0)},
            "total_profit_usdt":gain+loss,
            "avg_win_usdt":avg(&wins),
            "avg_loss_usdt":avg(&losses),
            "profit_factor":if loss < 0.0 {json!(gain / -loss)} else {Value::Null},
            "avg_hold_minutes":avg(&holds),
        },
        "by_exit_reason":group(&closed, |t| t.exit_reason.clone().unwrap_or_default()),
        "by_pair_and_side":group(&closed, |t| format!("{} {}", t.pair, if t.is_short {"short"} else {"long"})),
        "by_holding_time":group(&closed, |t| match hold_minutes(t) {
            Some(m) if m < 60.0 => "under 1h",
            Some(m) if m < 240.0 => "1h to 4h",
            Some(m) if m < 720.0 => "4h to 12h",
            Some(_) => "over 12h",
            None => "unknown",
        }.to_string()),
    })
}

/// A review runs once enough new trades have closed, but never more often than every 30
/// minutes, and a single new trade waits up to a day for company.
pub fn review_due(closed: usize, reviewed: usize, last_at: i64, now: i64) -> bool {
    if now - last_at < 1800 {
        return false;
    }
    let new = closed.saturating_sub(reviewed);
    new >= 3 || (new >= 1 && now - last_at >= 86_400)
}

impl App {
    pub async fn maybe_review(&self) -> Result<()> {
        let c = self.store.config()?;
        if !c.ai.enabled || !c.ai.review || c.ai.api_key.is_empty() {
            return Ok(());
        }
        let closed = self
            .store
            .trades()?
            .iter()
            .filter(|t| t.close_date.is_some())
            .count();
        let state = |key: &str| -> i64 {
            self.store
                .state(key)
                .ok()
                .flatten()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0)
        };
        let now = chrono::Utc::now().timestamp();
        if !review_due(
            closed,
            state("review_count") as usize,
            state("review_at"),
            now,
        ) {
            return Ok(());
        }
        let rec = self.review("auto").await;
        // Failed or skipped attempts wait out the minimum gap before trying again.
        self.store.set_state("review_at", &now.to_string())?;
        if rec?["error"].is_null() {
            self.store.set_state("review_count", &closed.to_string())?;
        }
        Ok(())
    }

    pub async fn review(&self, trigger: &str) -> Result<Value> {
        let _lock = self
            .ai_gate
            .try_lock()
            .map_err(|_| anyhow!("上一次分析还没结束"))?;
        let c = self.store.config()?;
        ensure!(!c.ai.api_key.is_empty(), "先保存大模型 API Key");
        let started = std::time::Instant::now();
        let mut rec = json!({"time":now(),"source":"review","trigger":trigger,"model":c.ai.model,"channel":c.ai.active().map(|ch| ch.name.clone()),"execute":false});
        let result: Result<()> = async {
            let mut closed: Vec<Trade> = self
                .store
                .trades()?
                .into_iter()
                .filter(|t| t.close_date.is_some())
                .collect();
            ensure!(!closed.is_empty(), "还没有已平仓的交易可复盘");
            closed.sort_by(|a, b| b.close_date.cmp(&a.close_date));
            let notes = self.store.knowledge()?;
            let mine: Vec<Value> = notes
                .iter()
                .filter(|n| n["origin"] == "ai")
                .map(|n| json!({"id":n["id"],"title":n["title"],"content":n["content"],"enabled":n["enabled"]}))
                .collect();
            let others: Vec<&Value> = notes
                .iter()
                .filter(|n| n["origin"] != "ai")
                .map(|n| &n["title"])
                .collect();
            let recent: Vec<Value> = closed
                .iter()
                .take(40)
                .map(|t| {
                    json!({"pair":t.pair,"side":if t.is_short{"short"}else{"long"},"leverage":t.leverage,"opened_utc":t.open_date,"closed_utc":t.close_date,"hold_minutes":hold_minutes(t).map(|m| m.round()),"profit_usdt":t.profit_abs,"profit_pct_on_margin":t.profit_ratio*100.0,"exit_reason":t.exit_reason,"stop_distance_pct":t.stop_dist*100.0,"entry_reason":clip(&t.entry_reason, 300)})
                })
                .collect();
            let snap = json!({"time_utc":now(),"stats":trade_stats(&closed),"recent_trades":recent,"ai_notes":mine,"max_ai_notes":MAX_AI_NOTES,"other_note_titles":others});
            let (text, usage) = call_model_for(&c, &snap, &REVIEW).await?;
            rec["usage"] = usage;
            let response: Value = serde_json::from_str(extract_json(&text))?;
            ensure!(response["summary"].is_string(), "模型缺少 summary");
            let changes = response["changes"].as_array().context("模型缺少 changes")?;
            ensure!(changes.len() <= 12, "模型返回了过多修改");
            rec["analysis"] = response["summary"].clone();
            rec["changes"] = json!(self.apply_review_changes(changes)?);
            Ok(())
        }
        .await;
        if let Err(e) = result {
            rec["error"] = json!(e.to_string());
        }
        rec["seconds"] = json!(started.elapsed().as_secs_f64());
        self.store.decision(&rec)?;
        Ok(rec)
    }

    /// Apply the reviewer's note changes. It may only touch notes it wrote itself, and keeps at
    /// most `MAX_AI_NOTES`; anything else is reported as skipped rather than failing the run.
    pub fn apply_review_changes(&self, changes: &[Value]) -> Result<Vec<Value>> {
        let mut notes = self.store.knowledge()?;
        let mut out = vec![];
        for ch in changes {
            let op = ch["op"].as_str().unwrap_or("").to_string();
            let id = ch["id"].as_i64();
            let title = ch["title"].as_str().map(|s| s.trim().to_string());
            let content = ch["content"].as_str().map(|s| clip(s.trim(), 1200));
            let result: Result<i64> = (|| match op.as_str() {
                "add" => {
                    let t = title
                        .clone()
                        .filter(|t| !t.is_empty())
                        .context("缺少标题")?;
                    let c = content
                        .clone()
                        .filter(|c| !c.is_empty())
                        .context("缺少内容")?;
                    ensure!(t.chars().count() <= 80, "标题过长");
                    ensure!(
                        notes.iter().filter(|n| n["origin"] == "ai").count() < MAX_AI_NOTES,
                        "AI 观察笔记已达 {} 条上限，请先合并或删除",
                        MAX_AI_NOTES
                    );
                    let id = self.store.knowledge_add(&t, &c, "AI 复盘", true, "ai")?;
                    notes.push(json!({"id":id,"origin":"ai","title":t,"content":c,"enabled":true,"source":"AI 复盘"}));
                    Ok(id)
                }
                "update" | "delete" => {
                    let id = id.context("缺少 id")?;
                    let n = notes
                        .iter()
                        .find(|n| n["id"] == id)
                        .context("笔记不存在")?
                        .clone();
                    ensure!(n["origin"] == "ai", "只能修改 AI 自己的观察笔记");
                    if op == "delete" {
                        self.store.knowledge_delete(id)?;
                        notes.retain(|n| n["id"] != id);
                        return Ok(id);
                    }
                    let t = title
                        .clone()
                        .or_else(|| n["title"].as_str().map(str::to_string))
                        .filter(|t| !t.is_empty() && t.chars().count() <= 80)
                        .context("标题无效")?;
                    let c = content
                        .clone()
                        .or_else(|| n["content"].as_str().map(str::to_string))
                        .filter(|c| !c.is_empty())
                        .context("内容无效")?;
                    self.store.knowledge_update(
                        id,
                        &t,
                        &c,
                        n["source"].as_str().unwrap_or(""),
                        n["enabled"].as_bool().unwrap_or(true),
                    )?;
                    if let Some(e) = notes.iter_mut().find(|n| n["id"] == id) {
                        e["title"] = json!(t);
                        e["content"] = json!(c);
                    }
                    Ok(id)
                }
                _ => Err(anyhow!("未知操作")),
            })();
            out.push(match result {
                Ok(id) => json!({"op":op,"id":id,"title":title,"status":"applied"}),
                Err(e) => {
                    json!({"op":op,"id":id,"title":title,"status":"skipped","note":e.to_string()})
                }
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(pair: &str, short: bool, profit: f64, reason: &str, hours: i64) -> Trade {
        let open = chrono::NaiveDate::from_ymd_opt(2026, 11, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let fmt = |t: chrono::NaiveDateTime| t.format("%Y-%m-%d %H:%M:%S").to_string();
        Trade {
            trade_id: 1,
            pair: pair.into(),
            market: "futures".into(),
            is_short: short,
            leverage: 5,
            amount: 1.0,
            open_rate: 100.0,
            stake_amount: 10.0,
            open_date: fmt(open),
            close_rate: Some(100.0),
            close_date: Some(fmt(open + chrono::Duration::hours(hours))),
            profit_abs: profit,
            profit_ratio: profit / 10.0,
            stop_loss_abs: 95.0,
            take_profit_abs: 0.0,
            peak: 100.0,
            enter_tag: "ai".into(),
            exit_reason: Some(reason.into()),
            entry_fee: 0.0,
            stop_dist: 0.05,
            entry_reason: String::new(),
            funding: 0.0,
        }
    }

    #[test]
    fn stats_summarize_results_by_exit_pair_and_holding_time() {
        let trades = vec![
            trade("NEAR/USDT:USDT", true, 3.0, "stoploss_on_exchange", 5),
            trade("NEAR/USDT:USDT", true, 1.0, "stoploss_on_exchange", 13),
            trade("XRP/USDT:USDT", false, -2.0, "force_exit", 1),
            trade("XRP/USDT:USDT", false, -1.0, "force_exit", 0),
        ];
        let s = trade_stats(&trades);
        assert_eq!(s["overall"]["trades"], 4);
        assert_eq!(s["overall"]["wins"], 2);
        assert_eq!(s["overall"]["win_rate_pct"], 50.0);
        assert_eq!(s["overall"]["total_profit_usdt"], 1.0);
        assert_eq!(s["overall"]["profit_factor"], 4.0 / 3.0);
        assert_eq!(s["by_exit_reason"]["force_exit"]["profit_usdt"], -3.0);
        assert_eq!(s["by_exit_reason"]["stoploss_on_exchange"]["wins"], 2);
        assert_eq!(s["by_pair_and_side"]["XRP/USDT:USDT long"]["trades"], 2);
        assert_eq!(s["by_holding_time"]["under 1h"]["trades"], 1);
        assert_eq!(s["by_holding_time"]["1h to 4h"]["trades"], 1);
        assert_eq!(s["by_holding_time"]["4h to 12h"]["trades"], 1);
        assert_eq!(s["by_holding_time"]["over 12h"]["trades"], 1);
        assert!(trade_stats(&[])["overall"]["win_rate_pct"].is_null());
    }

    #[test]
    fn reviews_wait_for_new_trades_and_a_minimum_gap() {
        let day = 86_400;
        assert!(review_due(5, 0, 0, 10_000));
        assert!(!review_due(2, 0, 0, 10_000));
        assert!(!review_due(4, 2, 0, 10_000));
        assert!(review_due(5, 2, 0, 10_000 + 3));
        // Never twice within 30 minutes, even with plenty of new trades.
        assert!(!review_due(20, 0, 10_000, 10_000 + 1799));
        // One new trade waits a day for company.
        assert!(!review_due(6, 5, 10_000, 10_000 + day - 1));
        assert!(review_due(6, 5, 10_000, 10_000 + day));
        assert!(!review_due(5, 5, 10_000, 10_000 + 5 * day));
    }
}
