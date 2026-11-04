use crate::{
    engine::stop_distance,
    indicators,
    model::{now, num, AiConfig, Config},
    App,
};
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::sync::Arc;

const SYSTEM: &str = r#"You manage a Bybit crypto account. Your primary objective is to grow account equity through positive expected net profit over repeated trades, accounting for fees, slippage and funding costs where applicable. Actively seek evidence-backed long and short opportunities within the supplied risk limits. Evaluate entries, holding and exits by their expected future return; do not make breaking even on a position the objective. Do not force trades when the evidence does not support positive expected net profit, and do not invent unavailable cost estimates or imply guaranteed returns.
Use only the supplied closed candles and account snapshot. Return only a JSON object {"analysis":"简体中文分析","actions":[{"action":"open_long|open_short|close","pair":"exact pair from snapshot","confidence":0.0,"reason":"简体中文理由"}]}. Explain the opportunity or lack of opportunity using specific supplied trend, momentum, volatility and price signals, including the expected direction and what would invalidate the thesis. For each action, explain how it serves the net profit objective. When actions is empty, explain the missing market evidence or binding constraint. Passing a risk check, such as remaining below the daily loss limit, is eligibility to trade and is not evidence of a profitable opportunity.
Respect whitelist, concurrent position slots, daily loss and min confidence. Closed positions release slots and can be replaced by new longs or shorts; there is no daily entry count limit. Do not add to existing positions. Shorts only when short_allowed. System sizes each position so a stop-out loses about risk_per_trade_pct of equity, sets the stop at 2.5 × ATR(14) of the 4h candles (2% to 10% of price), lowers leverage per trade so the stop stays well ahead of liquidation, and trails the stop by half that distance once price has moved one stop distance in favor. The daily loss check counts realized PnL plus open floating losses. Confidence must be finite and between 0 and 1."#;

impl App {
    pub async fn snapshot(&self) -> Result<Value> {
        let c = self.store.config()?;
        let ex = self.exchange(&c);
        let held = self.store.open_trades()?;
        let (equity, _, pnl) = self.account_risk(&ex, &held).await?;
        let mut positions = vec![];
        let mut pairs = vec![];
        for t in &held {
            let price = ex.price(&t.market, &t.pair).await?;
            positions.push(json!({"pair":t.pair,"side":if t.is_short{"short"}else{"long"},"open_rate":t.open_rate,"current_rate":price,"profit_pct":num(&t.mark(price)["profit_ratio"])*100.0,"stop_loss_rate":t.stop_loss_abs,"opened_utc":t.open_date,"entry_tag":t.enter_tag}));
        }
        for p in &c.pairs {
            let cs = ex.candles(&c.market, p, 240, 300, None).await?;
            ensure!(cs.len() >= 220, "{} K 线预热数据不足", p);
            let xs = indicators::calculate(&cs);
            let rows: Vec<_> = cs
                .iter()
                .zip(&xs)
                .skip(cs.len().saturating_sub(60))
                .map(|(c, x)| {
                    json!([
                        chrono::DateTime::from_timestamp_millis(c.time)
                            .unwrap()
                            .format("%Y-%m-%d %H:%M")
                            .to_string(),
                        c.open,
                        c.high,
                        c.low,
                        c.close,
                        c.volume,
                        x.ema20,
                        x.ema50,
                        x.ema200,
                        x.adx,
                        x.rsi,
                        x.atr
                    ])
                })
                .collect();
            let last = (cs.last(), xs.last());
            let stop = match last {
                (Some(c), Some(x)) => stop_distance(x.atr, c.close),
                _ => None,
            };
            pairs.push(json!({"pair":p,"stop_loss_price_pct":stop.map(|d| d * 100.0),"columns":["date","open","high","low","close","volume","ema20","ema50","ema200","adx","rsi","atr"],"candles":rows}));
        }
        Ok(
            json!({"time_utc":now(),"exchange":"bybit","market":c.market,"timeframe":"4h","leverage":c.leverage,"short_allowed":c.market=="futures"&&c.short,"bot_state":if c.running{"running"}else{"stopped"},
            "account":{"equity_usdt":equity,"pnl_today_with_floating_loss_usdt":pnl,"pnl_today_with_floating_loss_pct":if equity-pnl>0.0{pnl/(equity-pnl)*100.0}else{0.0}},
            "risk_limits":{"max_open":c.max_open,"slots_left":c.max_open.saturating_sub(held.len()),"max_daily_loss_pct":c.ai.max_daily_loss_pct,"min_confidence":c.ai.min_confidence,"risk_per_trade_pct":c.risk_per_trade_pct,"max_leverage":c.leverage},"open_positions":positions,"pairs":pairs}),
        )
    }
    pub async fn execute_actions(&self, actions: &[Value], external: bool) -> Result<Vec<Value>> {
        let _lock = self.gate.lock().await;
        let mut acts = actions.to_vec();
        acts.sort_by_key(|a| a["action"] != "close");
        let mut out = vec![];
        for mut a in acts {
            let action = a["action"].as_str().context("缺少 action")?.to_string();
            let pair = a["pair"].as_str().context("缺少 pair")?.to_uppercase();
            let c = self.store.config()?;
            let held = self.store.open_trades()?;
            let canonical = c
                .pairs
                .iter()
                .chain(held.iter().map(|t| &t.pair))
                .find(|p| **p == pair || p.split(':').next() == Some(pair.as_str()))
                .cloned()
                .unwrap_or(pair);
            a["pair"] = json!(canonical);
            let result = if !external && !c.ai.enabled {
                Err(anyhow::anyhow!("AI 交易已关闭"))
            } else if action == "close" {
                match held.iter().find(|t| t.pair == canonical) {
                    Some(t) => self.close_position(t.trade_id, "force_exit").await,
                    None => Err(anyhow::anyhow!("没有该交易对持仓")),
                }
            } else if action == "open_long" || action == "open_short" {
                let confidence = if external {
                    None
                } else {
                    Some(a["confidence"].as_f64().unwrap_or(f64::NAN))
                };
                self.open_position(&canonical, action == "open_short", "ai", confidence)
                    .await
            } else {
                Err(anyhow::anyhow!("未知 action"))
            };
            match result {
                Ok(()) => a["status"] = json!("executed"),
                Err(e) => {
                    a["status"] = json!("failed");
                    a["note"] = json!(e.to_string());
                }
            }
            out.push(a);
        }
        Ok(out)
    }
    pub async fn run_ai(&self, execute: bool, source: &str) -> Result<Value> {
        let _lock = self
            .ai_gate
            .try_lock()
            .map_err(|_| anyhow::anyhow!("上一次分析还没结束"))?;
        let c = self.store.config()?;
        ensure!(!c.ai.api_key.is_empty(), "先保存大模型 API Key");
        ensure!(!execute || c.ai.enabled, "先开启 AI 交易");
        self.ai_state.lock().await["running"] = json!(true);
        let started = std::time::Instant::now();
        let mut rec = json!({"time":now(),"source":source,"model":c.ai.model,"execute":execute});
        let result: Result<()> = async {
            let snap = self.snapshot().await?;
            let (text, usage) = call_model(&c, &snap).await?;
            rec["usage"] = usage;
            let text = text
                .trim()
                .trim_start_matches("```json")
                .trim_start_matches("```")
                .trim_end_matches("```")
                .trim();
            let response: Value = serde_json::from_str(text)?;
            ensure!(response["analysis"].is_string(), "模型缺少 analysis");
            let acts = response["actions"].as_array().context("模型缺少 actions")?;
            ensure!(acts.len() <= 24, "模型返回了过多动作");
            for a in acts {
                ensure!(
                    matches!(
                        a["action"].as_str(),
                        Some("open_long" | "open_short" | "close")
                    ) && a["pair"].is_string()
                        && a["confidence"]
                            .as_f64()
                            .is_some_and(|n| n.is_finite() && (0.0..=1.0).contains(&n)),
                    "模型动作格式无效"
                );
            }
            rec["analysis"] = response["analysis"].clone();
            rec["actions"] = if execute {
                json!(self.execute_actions(acts, false).await?)
            } else {
                json!(acts)
            };
            Ok(())
        }
        .await;
        if let Err(e) = result {
            rec["error"] = json!(e.to_string());
        }
        rec["seconds"] = json!(started.elapsed().as_secs_f64());
        {
            let mut state = self.ai_state.lock().await;
            state["running"] = json!(false);
            state["last_run"] = rec["time"].clone();
            state["last_error"] = rec["error"].clone();
        }
        self.store.decision(&rec)?;
        if execute {
            let _ = self.telegram(&c, &format!("AI 决策：{}", rec)).await;
        }
        Ok(rec)
    }
}
async fn call_model(c: &Config, snap: &Value) -> Result<(String, Value)> {
    let http = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .timeout(std::time::Duration::from_secs(600))
        .build()?;
    let ai = &c.ai;
    let (url, body) = model_request(ai, snap)?;
    let request = http
        .post(&url)
        .header("accept", "application/json")
        .json(&body);
    let request = if ai.provider == "anthropic" {
        request
            .header("x-api-key", &ai.api_key)
            .header("anthropic-version", "2023-06-01")
    } else {
        request.bearer_auth(&ai.api_key)
    };
    let response = request.send().await?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();
    let text = response.text().await?;
    let v = decode_model_body(status, &content_type, &text, &ai.api_key, &url)?;
    parse_model_response(&v)
}

fn decode_model_body(
    status: reqwest::StatusCode,
    content_type: &str,
    text: &str,
    key: &str,
    endpoint: &str,
) -> Result<Value> {
    let mut endpoint = url::Url::parse(endpoint)?;
    endpoint.set_query(None);
    endpoint.set_fragment(None);
    let _ = endpoint.set_username("");
    let _ = endpoint.set_password(None);
    let is_html = content_type.to_ascii_lowercase().contains("text/html")
        || text.trim_start().starts_with('<');
    ensure!(!is_html, "模型接口返回了 HTML 网页而非 JSON（HTTP {}，地址 {}）。请检查接口路径，通常应为 /v1/responses、/v1/chat/completions 或 /v1/messages", status, endpoint);
    let safe = if key.is_empty() {
        text.to_string()
    } else {
        text.replace(key, "[redacted]")
    };
    if !status.is_success() {
        bail!(
            "模型接口 HTTP {}：{}",
            status,
            safe.chars().take(1000).collect::<String>()
        );
    }
    ensure!(
        !content_type.contains("text/event-stream")
            && !text.trim_start().starts_with("event:")
            && !text.trim_start().starts_with("data:"),
        "模型接口返回了流式数据，但项目请求为非流式；请检查中转服务的强制流式设置（地址 {}）",
        endpoint
    );
    let v: Value = serde_json::from_str(text).with_context(|| {
        format!(
            "模型接口未返回有效 JSON（HTTP {}，Content-Type: {}，地址 {}）",
            status, content_type, endpoint
        )
    })?;
    if !v["error"].is_null() {
        let message = v["error"]["message"]
            .as_str()
            .or_else(|| v["error"].as_str())
            .unwrap_or("未知接口错误");
        let safe = if key.is_empty() {
            message.into()
        } else {
            message.replace(key, "[redacted]")
        };
        bail!(
            "模型接口错误：{}",
            safe.chars().take(1000).collect::<String>()
        );
    }
    Ok(v)
}

pub fn validate_model_options(ai: &AiConfig) -> Result<()> {
    ensure!(
        matches!(
            ai.claude_thinking.as_str(),
            "default" | "adaptive" | "enabled" | "disabled"
        ),
        "Claude 思考模式无效"
    );
    ensure!(
        matches!(
            ai.claude_effort.as_str(),
            "default" | "low" | "medium" | "high" | "xhigh" | "max"
        ),
        "Claude 思考等级无效"
    );
    if ai.claude_thinking == "enabled" {
        ensure!(
            (1024..16000).contains(&ai.claude_thinking_budget),
            "Claude 手动思考预算须为 1024–15999 tokens"
        );
    }
    ensure!(
        ai.provider != "anthropic"
            || ai.claude_thinking != "disabled"
            || !matches!(ai.claude_effort.as_str(), "xhigh" | "max"),
        "Claude 在 xhigh/max 等级下不能关闭思考，请选择自适应模式或降低等级"
    );
    ensure!(
        matches!(ai.api_protocol.as_str(), "chat_completions" | "responses"),
        "模型接口类型无效"
    );
    ensure!(
        matches!(
            ai.reasoning_effort.as_str(),
            "default" | "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
        ),
        "思考等级无效"
    );
    Ok(())
}

fn model_request(ai: &AiConfig, snap: &Value) -> Result<(String, Value)> {
    validate_model_options(ai)?;
    // Some Responses gateways check the input independently of system instructions
    // before accepting JSON mode, so the requirement belongs in both places.
    let user = format!(
        "Return only a valid JSON object with analysis and actions.\nSnapshot:\n{}",
        snap
    );
    if ai.provider == "anthropic" {
        let base = if ai.base_url.is_empty() {
            "https://api.anthropic.com"
        } else {
            ai.base_url.trim_end_matches('/')
        };
        let url = if base.ends_with("/v1/messages") {
            base.into()
        } else if base.ends_with("/v1") {
            format!("{}/messages", base)
        } else {
            format!("{}/v1/messages", base)
        };
        let mut body = json!({"model":ai.model,"stream":false,"max_tokens":32000,"system":SYSTEM,"messages":[{"role":"user","content":user}]});
        match ai.claude_thinking.as_str() {
            "adaptive" => body["thinking"] = json!({"type":"adaptive"}),
            "enabled" => {
                body["thinking"] =
                    json!({"type":"enabled","budget_tokens":ai.claude_thinking_budget})
            }
            "disabled" => body["thinking"] = json!({"type":"disabled"}),
            _ => {}
        }
        if ai.claude_effort != "default" {
            body["output_config"] = json!({"effort":ai.claude_effort});
        }
        return Ok((url, body));
    }
    ensure!(ai.provider == "openai", "模型服务商无效");
    let base = if ai.base_url.is_empty() {
        "https://api.openai.com/v1"
    } else {
        ai.base_url.trim_end_matches('/')
    };
    // A root host is an OpenAI API host, not an API prefix. Full endpoints and
    // custom prefixes are explicit and must retain their existing path.
    let root = url::Url::parse(base)?;
    let base = if root.path().is_empty() || root.path() == "/" {
        format!("{}/v1", base)
    } else {
        base.to_string()
    };
    let base = base
        .strip_suffix("/chat/completions")
        .or_else(|| base.strip_suffix("/responses"))
        .unwrap_or(&base);
    let (path, mut body) = if ai.api_protocol == "responses" {
        (
            "responses",
            json!({"model":ai.model,"stream":false,"store":false,"instructions":SYSTEM,"input":user,"text":{"format":{"type":"json_object"}}}),
        )
    } else {
        (
            "chat/completions",
            json!({"model":ai.model,"stream":false,"response_format":{"type":"json_object"},"messages":[{"role":"system","content":SYSTEM},{"role":"user","content":user}]}),
        )
    };
    if ai.reasoning_effort != "default" {
        if ai.api_protocol == "responses" {
            body["reasoning"] = json!({"effort":ai.reasoning_effort});
        } else {
            body["reasoning_effort"] = json!(ai.reasoning_effort);
        }
    }
    Ok((format!("{}/{}", base, path), body))
}

fn parse_model_response(v: &Value) -> Result<(String, Value)> {
    ensure!(v["error"].is_null(), "模型接口返回错误");
    ensure!(
        !matches!(
            v["status"].as_str(),
            Some("incomplete" | "failed" | "cancelled" | "queued" | "in_progress")
        ) && v["choices"][0]["finish_reason"] != "length"
            && v["stop_reason"] != "max_tokens",
        "模型输出未完成，未执行交易（{}，已输出 {} tokens）。思考等级过高时思考会用完输出上限，可调低思考等级",
        [&v["stop_reason"], &v["choices"][0]["finish_reason"], &v["incomplete_details"]["reason"], &v["status"]]
            .into_iter()
            .find_map(|x| x.as_str())
            .unwrap_or("unknown"),
        [&v["usage"]["output_tokens"], &v["usage"]["completion_tokens"]]
            .into_iter()
            .find_map(|x| x.as_u64())
            .map_or("?".into(), |n| n.to_string())
    );
    ensure!(
        v["choices"][0]["message"]["refusal"].as_str().is_none(),
        "模型拒绝生成交易决策"
    );
    if let Some(text) = v["choices"][0]["message"]["content"].as_str() {
        if !text.trim().is_empty() {
            return Ok((text.into(), v["usage"].clone()));
        }
    }
    if let Some(xs) = v["output"].as_array() {
        let mut parts = vec![];
        for message in xs
            .iter()
            .filter(|m| m["type"] == "message" && m["role"] == "assistant")
        {
            ensure!(
                message["status"].as_str().is_none_or(|s| s == "completed"),
                "模型输出未完成，未执行交易"
            );
            if let Some(content) = message["content"].as_array() {
                for part in content {
                    ensure!(part["type"] != "refusal", "模型拒绝生成交易决策");
                    if part["type"] == "output_text" {
                        if let Some(text) = part["text"].as_str() {
                            parts.push(text);
                        }
                    }
                }
            }
        }
        let text = parts.join("");
        if !text.trim().is_empty() {
            return Ok((text, v["usage"].clone()));
        }
    }
    if let Some(xs) = v["content"].as_array() {
        let text = xs
            .iter()
            .filter(|x| x["type"] == "text")
            .filter_map(|x| x["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if !text.trim().is_empty() {
            return Ok((text, v["usage"].clone()));
        }
    }
    bail!("模型没有返回可解析的文本")
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct ScheduleState {
    interval: u32,
    delay_seconds: u32,
    last_slot: i64,
}

pub fn validate_schedule(interval: u32, delay_seconds: u32) -> Result<()> {
    ensure!(
        (1..=1440).contains(&interval),
        "分析间隔须为 1 到 1440 分钟"
    );
    ensure!(delay_seconds < interval * 60, "延迟须小于分析间隔（秒）");
    Ok(())
}

// Persist the handled slot before invoking the model. A new or changed schedule
// starts at its next future slot, rather than immediately replaying a past one.
fn schedule_tick(
    now: i64,
    interval: u32,
    delay_seconds: u32,
    previous: Option<&ScheduleState>,
) -> Result<(ScheduleState, i64, bool)> {
    validate_schedule(interval, delay_seconds)?;
    let step = i64::from(interval) * 60;
    let slot = (now - i64::from(delay_seconds)).div_euclid(step) * step + i64::from(delay_seconds);
    let same = previous.filter(|p| p.interval == interval && p.delay_seconds == delay_seconds);
    let due = same.is_some_and(|p| p.last_slot < slot && now - slot < step.min(300));
    Ok((
        ScheduleState {
            interval,
            delay_seconds,
            last_slot: slot,
        },
        slot + step,
        due,
    ))
}

pub async fn scheduler(app: Arc<App>) {
    loop {
        let result: Result<()> = async {
            let c = app.store.config()?;
            if !c.ai.enabled || !c.running || c.ai.api_key.is_empty() {
                app.ai_state.lock().await["next_run"] = Value::Null;
                return Ok(());
            }
            let now = chrono::Utc::now().timestamp();
            let key = "ai_schedule_v2";
            let previous = app
                .store
                .state(key)?
                .map(|s| serde_json::from_str::<ScheduleState>(&s))
                .transpose()?;
            let (schedule, next, due) =
                schedule_tick(now, c.ai.interval, c.ai.delay_seconds, previous.as_ref())?;
            app.ai_state.lock().await["next_run"] =
                json!(chrono::DateTime::from_timestamp(next, 0)
                    .unwrap()
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string());
            app.store
                .set_state(key, &serde_json::to_string(&schedule)?)?;
            if due {
                app.run_ai(true, "auto").await?;
            }
            Ok(())
        }
        .await;
        if let Err(e) = result {
            app.ai_state.lock().await["last_error"] = json!(e.to_string());
            app.store.log("ERROR", &format!("AI：{}", e));
        }
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
    }
}

#[cfg(test)]
mod model_tests {
    use super::*;
    use std::future::IntoFuture;

    #[test]
    fn legacy_config_preserves_chat_defaults() {
        let old: AiConfig =
            serde_json::from_value(json!({"provider":"openai","model":"old-model"})).unwrap();
        let (url, body) = model_request(&old, &json!({})).unwrap();
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("reasoning").is_none());
        let old: AiConfig = serde_json::from_value(json!({"provider":"anthropic"})).unwrap();
        let (_, body) = model_request(&old, &json!({})).unwrap();
        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
        assert_eq!(old.claude_thinking_budget, 4096);
    }

    #[test]
    fn complete_endpoints_do_not_duplicate_paths() {
        let mut ai = AiConfig {
            provider: "openai".into(),
            api_protocol: "responses".into(),
            ..AiConfig::default()
        };
        for base in [
            "https://example.com",
            "https://example.com/",
            "https://example.com/v1/",
            "https://example.com/v1/responses",
            "https://example.com/v1/chat/completions/",
        ] {
            ai.base_url = base.into();
            assert_eq!(
                model_request(&ai, &json!({})).unwrap().0,
                "https://example.com/v1/responses"
            );
        }
        for (base, expected) in [
            (
                "https://api.onerouter.com",
                "https://api.onerouter.com/v1/responses",
            ),
            (
                "https://example.com/custom/responses",
                "https://example.com/custom/responses",
            ),
            (
                "https://example.com/responses",
                "https://example.com/responses",
            ),
            (
                "https://example.com/proxy",
                "https://example.com/proxy/responses",
            ),
        ] {
            ai.base_url = base.into();
            assert_eq!(model_request(&ai, &json!({})).unwrap().0, expected);
        }
        ai.api_protocol = "chat_completions".into();
        ai.base_url = "https://api.onerouter.com".into();
        assert_eq!(
            model_request(&ai, &json!({})).unwrap().0,
            "https://api.onerouter.com/v1/chat/completions"
        );
        ai.provider = "anthropic".into();
        for base in [
            "https://example.com",
            "https://example.com/v1",
            "https://example.com/v1/messages/",
        ] {
            ai.base_url = base.into();
            assert_eq!(
                model_request(&ai, &json!({})).unwrap().0,
                "https://example.com/v1/messages"
            );
        }
    }

    #[test]
    fn model_response_errors_identify_html_streams_and_bad_json() {
        let url = "https://example.com/responses?api_key=secret";
        let error = decode_model_body(
            reqwest::StatusCode::OK,
            "text/html",
            "<!doctype html><html>secret</html>",
            "secret",
            url,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("HTML") && error.contains("/v1/responses"));
        assert!(!error.contains("secret"));
        let error = decode_model_body(
            reqwest::StatusCode::OK,
            "text/event-stream",
            "event: response.completed\ndata: {}",
            "",
            url,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("流式"));
        let error = decode_model_body(
            reqwest::StatusCode::OK,
            "application/json",
            "not-json",
            "",
            url,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("Content-Type: application/json"));
        let error = decode_model_body(
            reqwest::StatusCode::OK,
            "application/json",
            r#"{"error":{"message":"unsupported parameter secret"}}"#,
            "secret",
            url,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unsupported parameter") && !error.contains("secret"));
        assert_eq!(
            decode_model_body(reqwest::StatusCode::OK, "text/plain", "{}", "", url).unwrap(),
            json!({})
        );
    }

    #[test]
    fn claude_budget_and_effort_validation() {
        let mut ai = AiConfig {
            claude_thinking: "enabled".into(),
            ..AiConfig::default()
        };
        for budget in [1024, 4096, 15999] {
            ai.claude_thinking_budget = budget;
            validate_model_options(&ai).unwrap();
        }
        for budget in [0, 1023, 16000] {
            ai.claude_thinking_budget = budget;
            assert!(validate_model_options(&ai).is_err());
        }
        ai.claude_thinking = "disabled".into();
        ai.claude_effort = "max".into();
        assert!(validate_model_options(&ai).is_err());
        ai.claude_thinking = "adaptive".into();
        validate_model_options(&ai).unwrap();
        ai.provider = "openai".into();
        let (_, body) = model_request(&ai, &json!({})).unwrap();
        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn partial_outputs_refusals_and_reasoning_are_never_decisions() {
        for v in [
            json!({"status":"incomplete","output":[]}),
            json!({"choices":[{"finish_reason":"length","message":{"content":"{}"}}]}),
            json!({"stop_reason":"max_tokens","content":[{"type":"text","text":"{}"}]}),
            json!({"output":[{"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"no"}]}]}),
            json!({"output":[{"type":"reasoning","content":[{"type":"output_text","text":"{}"}]}]}),
            json!({"content":[{"type":"thinking","text":"{}"}]}),
        ] {
            assert!(parse_model_response(&v).is_err(), "{}", v);
        }
    }

    async fn mock_model(req: axum::extract::Request) -> axum::response::Response {
        use axum::response::IntoResponse;
        let path = req.uri().path().to_string();
        let headers = req.headers().clone();
        let bytes = axum::body::to_bytes(req.into_body(), 65536).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(headers["accept"], "application/json");
        assert_eq!(body["stream"], false);
        if body["model"] == "unsupported" {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                axum::Json(
                    json!({"error":{"message":"unsupported reasoning_effort; key=test-model-key"}}),
                ),
            )
                .into_response();
        }
        let decision = "{\"analysis\":\"test\",\"actions\":[]}";
        let response = match path.as_str() {
            "/v1/responses" => {
                assert_eq!(headers["authorization"], "Bearer test-model-key");
                assert_eq!(body["reasoning"]["effort"], "high");
                assert!(body.get("reasoning_effort").is_none());
                assert_eq!(body["store"], false);
                assert_eq!(body["text"]["format"]["type"], "json_object");
                assert!(body["input"].as_str().unwrap().contains("snapshot-test"));
                assert!(body["input"]
                    .as_str()
                    .unwrap()
                    .to_ascii_lowercase()
                    .contains("json"));
                json!({"status":"completed","output":[{"type":"reasoning","summary":[]},{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":decision}]}],"usage":{"output_tokens":8}})
            }
            "/v1/chat/completions" => {
                assert_eq!(headers["authorization"], "Bearer test-model-key");
                assert_eq!(body["reasoning_effort"], "high");
                assert!(body.get("reasoning").is_none());
                assert_eq!(body["response_format"]["type"], "json_object");
                json!({"choices":[{"finish_reason":"stop","message":{"content":decision}}],"usage":{"completion_tokens":8}})
            }
            "/v1/messages" => {
                assert_eq!(headers["x-api-key"], "test-model-key");
                assert!(body.get("reasoning_effort").is_none());
                assert!(body.get("reasoning").is_none());
                match body["model"].as_str().unwrap() {
                    "test-adaptive" => {
                        assert_eq!(body["thinking"], json!({"type":"adaptive"}));
                        assert_eq!(body["output_config"], json!({"effort":"high"}));
                    }
                    "test-manual" => {
                        assert_eq!(
                            body["thinking"],
                            json!({"type":"enabled","budget_tokens":4096})
                        );
                        assert_eq!(body["max_tokens"], 32000);
                        assert!(body.get("output_config").is_none());
                    }
                    "test-disabled" => {
                        assert_eq!(body["thinking"], json!({"type":"disabled"}));
                        assert!(body.get("output_config").is_none());
                    }
                    "test-effort-only" => {
                        assert!(body.get("thinking").is_none());
                        assert_eq!(body["output_config"], json!({"effort":"medium"}));
                    }
                    _ => {
                        assert!(body.get("thinking").is_none());
                        assert!(body.get("output_config").is_none());
                    }
                }
                json!({"content":[{"type":"thinking","thinking":"private"},{"type":"text","text":decision}],"usage":{"output_tokens":8}})
            }
            _ => panic!("unexpected model endpoint {}", path),
        };
        axum::Json(response).into_response()
    }

    #[tokio::test]
    async fn sends_each_protocol_and_parses_only_final_answers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(
            axum::serve(
                listener,
                axum::Router::new().route("/{*path}", axum::routing::post(mock_model)),
            )
            .into_future(),
        );
        let mut c = Config::default();
        c.ai.base_url = format!("http://{}/v1", address);
        c.ai.api_key = "test-model-key".into();
        c.ai.reasoning_effort = "high".into();
        for (provider, protocol) in [
            ("openai", "chat_completions"),
            ("openai", "responses"),
            ("anthropic", "chat_completions"),
        ] {
            c.ai.provider = provider.into();
            c.ai.api_protocol = protocol.into();
            let (text, usage) = call_model(&c, &json!({"source":"snapshot-test"}))
                .await
                .unwrap();
            let result: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(result["analysis"], "test");
            assert_eq!(result["actions"], json!([]));
            assert!(usage.is_object());
        }
        for (model, thinking, effort) in [
            ("test-adaptive", "adaptive", "high"),
            ("test-manual", "enabled", "default"),
            ("test-disabled", "disabled", "default"),
            ("test-effort-only", "default", "medium"),
        ] {
            c.ai.model = model.into();
            c.ai.claude_thinking = thinking.into();
            c.ai.claude_effort = effort.into();
            let (text, _) = call_model(&c, &json!({})).await.unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&text).unwrap()["actions"],
                json!([])
            );
        }
        c.ai.model = "unsupported".into();
        let error = call_model(&c, &json!({})).await.unwrap_err().to_string();
        assert!(error.contains("400") && error.contains("unsupported reasoning_effort"));
        assert!(!error.contains("test-model-key"));
        server.abort();
    }
}

#[cfg(test)]
mod schedule_tests {
    use super::*;
    #[test]
    fn validates_custom_intervals_and_delays() {
        assert!(validate_schedule(1, 0).is_ok());
        assert!(validate_schedule(1440, 86399).is_ok());
        assert!(validate_schedule(0, 0).is_err());
        assert!(validate_schedule(1441, 0).is_err());
        assert!(validate_schedule(5, 300).is_err());
    }
    #[test]
    fn custom_schedule_runs_once_per_slot_and_survives_restart() {
        let (state, next, due) = schedule_tick(1000, 7, 30, None).unwrap();
        assert_eq!(next, 1290);
        assert!(!due);
        let (state, _, due) = schedule_tick(1290, 7, 30, Some(&state)).unwrap();
        assert!(due);
        let persisted: ScheduleState =
            serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert!(!schedule_tick(1300, 7, 30, Some(&persisted)).unwrap().2);
        assert!(schedule_tick(1710, 7, 30, Some(&persisted)).unwrap().2);
    }
    #[test]
    fn schedule_changes_and_stale_slots_do_not_trigger_catch_up() {
        let (state, _, _) = schedule_tick(1000, 60, 180, None).unwrap();
        assert!(!schedule_tick(1291, 7, 30, Some(&state)).unwrap().2);
        assert!(!schedule_tick(1000, 60, 0, Some(&state)).unwrap().2);
        let (old, _, _) = schedule_tick(0, 15, 0, None).unwrap();
        assert!(!schedule_tick(1301, 15, 0, Some(&old)).unwrap().2);
    }
    #[test]
    fn old_configuration_keeps_its_previous_timing() {
        let c: crate::model::AiConfig = serde_json::from_str(r#"{"interval":60}"#).unwrap();
        assert_eq!(c.delay_seconds, 180);
        let (state, next, due) = schedule_tick(3600, 60, 180, None).unwrap();
        assert_eq!(next, 3780);
        assert!(!due);
        assert!(schedule_tick(3780, 60, 180, Some(&state)).unwrap().2);
    }
}
