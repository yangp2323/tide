use crate::{
    auth,
    engine::usdt,
    exchange::{category, symbol},
    indicators,
    model::{mask, num, AiConfig, Channel, Config, Credentials},
    App,
};
use anyhow::{bail, ensure, Context, Result};
use axum::{
    body::to_bytes,
    extract::{Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};

pub async fn dispatch(State(app): State<Arc<App>>, req: Request) -> Response {
    let path = req.uri().path().trim_end_matches('/').to_string();
    let method = req.method().to_string();
    let q: HashMap<String, String> =
        url::form_urlencoded::parse(req.uri().query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    let bytes = match to_bytes(req.into_body(), 64 * 1024).await {
        Ok(b) => b,
        Err(_) => return error(StatusCode::PAYLOAD_TOO_LARGE, "请求过大"),
    };
    let body: Value = if bytes.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            // Alerts from other systems are often plain text; that text is the message.
            Err(_) if path.starts_with("/api/hook/") => {
                json!({"message":String::from_utf8_lossy(&bytes).to_string()})
            }
            Err(_) => return error(StatusCode::BAD_REQUEST, "JSON 格式无效"),
        }
    };
    if path == "/api/auth/login" && method == "POST" {
        let now = chrono::Utc::now().timestamp();
        {
            let mut attempts = app.login_attempts.lock().await;
            attempts.retain(|t| *t > now - 60);
            if attempts.len() >= 20 {
                return error(
                    StatusCode::TOO_MANY_REQUESTS,
                    "尝试次数过多，请一分钟后重试",
                );
            }
            attempts.push(now);
        }
        let Some(user) = body["username"].as_str().filter(|s| s.len() <= 256) else {
            return error(StatusCode::BAD_REQUEST, "账号格式无效");
        };
        let Some(pass) = body["password"].as_str().filter(|s| s.len() <= 1024) else {
            return error(StatusCode::BAD_REQUEST, "密码格式无效");
        };
        let (a, u, p) = (app.auth.clone(), user.to_string(), pass.to_string());
        match tokio::task::spawn_blocking(move || a.verify(&u, &p)).await {
            Ok(Ok(true)) => {
                let cookie = format!(
                    "{}={}; Path=/; Max-Age=43200; HttpOnly; Secure; SameSite=Strict",
                    auth::COOKIE,
                    auth::session(&app.secret)
                );
                return ([(header::SET_COOKIE, cookie)], Json(json!({"ok":true}))).into_response();
            }
            _ => return error(StatusCode::UNAUTHORIZED, "账号或密码错误"),
        }
    }
    if let Some(token) = path.strip_prefix("/api/hook/").filter(|_| method == "POST") {
        let (status, body) = app.receive_webhook(token, &body);
        return (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(body),
        )
            .into_response();
    }
    if path == "/api/auth/logout" && method == "POST" {
        return (
            [(
                header::SET_COOKIE,
                format!(
                    "{}=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Strict",
                    auth::COOKIE
                ),
            )],
            Json(json!({"ok":true})),
        )
            .into_response();
    }
    match handle(&app, &method, &path, &q, body).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}
fn error(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({"detail":msg}))).into_response()
}
fn limit(q: &HashMap<String, String>, default: usize, max: usize) -> usize {
    q.get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
        .clamp(1, max)
}
pub fn validate(c: &Config) -> Result<()> {
    ensure!(
        matches!(c.market.as_str(), "spot" | "futures"),
        "市场只能是现货或合约"
    );
    ensure!(
        !c.pairs.is_empty() && c.pairs.len() <= 12,
        "选择 1 到 12 个币"
    );
    ensure!((1..=5).contains(&c.max_open), "持仓数在 1 到 5 之间");
    ensure!((1..=25).contains(&c.leverage), "杠杆在 1 到 25 之间");
    ensure!(
        c.market == "futures" || c.leverage == 1 && !c.short,
        "现货不能加杠杆或做空"
    );
    ensure!(
        c.risk_per_trade_pct.is_finite() && (0.1..=5.0).contains(&c.risk_per_trade_pct),
        "每笔风险在 0.1% 到 5% 之间"
    );
    ensure!(c.max_same_direction <= 5, "同方向持仓上限在 0 到 5 之间");
    ensure!(
        c.capital.is_none_or(|v| v.is_finite() && v > 0.0),
        "资金上限必须是大于 0 的有效金额"
    );
    Ok(())
}
/// Title, content, source and enabled flag of a knowledge note; fields missing from the
/// request keep the values of `current`.
fn knowledge_fields(
    body: &Value,
    current: Option<&Value>,
) -> Result<(String, String, String, bool)> {
    let text = |key: &str| -> String {
        body[key]
            .as_str()
            .or_else(|| current.and_then(|c| c[key].as_str()))
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let (title, content, source) = (text("title"), text("content"), text("source"));
    ensure!(
        !title.is_empty() && title.chars().count() <= 80,
        "标题须为 1 到 80 个字符"
    );
    ensure!(
        !content.is_empty() && content.chars().count() <= 4000,
        "内容须为 1 到 4000 个字符"
    );
    ensure!(source.chars().count() <= 600, "来源不超过 600 个字符");
    let enabled = body["enabled"]
        .as_bool()
        .or_else(|| current.and_then(|c| c["enabled"].as_bool()))
        .unwrap_or(true);
    Ok((title, content, source, enabled))
}
fn ai_public(c: &AiConfig) -> Value {
    let mut v = json!(c);
    v["api_key"] = json!(mask(&c.api_key));
    for (i, ch) in c.channels.iter().enumerate() {
        v["channels"][i]["api_key"] = json!(mask(&ch.api_key));
    }
    v["hooks"]["webhook_token"] = json!(mask(&c.hooks.webhook_token));
    v["hooks"]["webhook_set"] = json!(!c.hooks.webhook_token.is_empty());
    v
}
/// Checks that apply to one channel together with the shared AI settings.
fn validate_channel(a: &AiConfig, ch: &Channel) -> Result<()> {
    ensure!(
        !ch.name.trim().is_empty() && ch.name.chars().count() <= 40,
        "渠道名称须为 1 到 40 个字符"
    );
    let probe = a.with_channel(ch);
    ensure!(
        matches!(probe.provider.as_str(), "anthropic" | "openai") && !probe.model.trim().is_empty(),
        "模型配置无效"
    );
    crate::ai::validate_model_options(&probe)?;
    if !probe.base_url.is_empty() {
        let url = url::Url::parse(&probe.base_url)?;
        ensure!(url.scheme() == "https", "模型接口必须使用 HTTPS");
    }
    Ok(())
}
async fn handle(
    app: &Arc<App>,
    method: &str,
    path: &str,
    q: &HashMap<String, String>,
    body: Value,
) -> Result<Value> {
    if method == "GET" {
        return match path {
            "/api/overview" => overview(app).await,
            "/api/holdings" => holdings(app).await,
            "/api/markets" => {
                markets(app, q.get("market").map(String::as_str).unwrap_or("spot")).await
            }
            "/api/trades" => {
                let c = app.store.config()?;
                let all: Vec<_> = app
                    .store
                    .trades()?
                    .into_iter()
                    .filter(|t| t.close_date.is_some() && t.market == c.market)
                    .collect();
                let offset = q
                    .get("offset")
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(0);
                Ok(
                    json!({"total_trades":all.len(),"trades":all.into_iter().skip(offset).take(limit(q,50,200)).collect::<Vec<_>>()}),
                )
            }
            "/api/logs" => app.store.logs(limit(q, 200, 500)),
            "/api/ai" => {
                let c = app.store.config()?;
                Ok(
                    json!({"config":ai_public(&c.ai),"state":app.ai_state.lock().await.clone(),"position_count":app.store.open_trades()?.len(),"max_open":c.max_open,"log":app.store.decisions(20)?}),
                )
            }
            "/api/ai/snapshot" => app.snapshot().await,
            "/api/ai/log" => Ok(json!(app.store.decisions(limit(q, 50, 500))?)),
            "/api/ai/usage" => app.store.usage_by_day(limit(q, 14, 90) as i64),
            "/api/knowledge" => Ok(json!(app.store.knowledge()?)),
            "/api/stops" => app.store.stop_history(limit(q, 50, 500)),
            "/api/orders/pending" => Ok(json!(app.store.unresolved()?)),
            _ => bail!("接口不存在"),
        };
    }
    if method == "POST" && path == "/api/ai/run" {
        return app
            .run_ai(q.get("execute").is_some_and(|s| s == "true"), "manual")
            .await;
    }
    if method == "POST" && path == "/api/ai/review" {
        return app.review("manual").await;
    }
    if method == "POST" && path == "/api/ai/order" {
        let actions = app
            .execute_actions(std::slice::from_ref(&body), true)
            .await?;
        let rec = json!({"time":crate::model::now(),"source":"mcp","model":"external","execute":true,"analysis":body["reason"],"actions":actions});
        app.store.decision(&rec)?;
        ensure!(
            actions[0]["status"] == "executed",
            "{}",
            actions[0]["note"].as_str().unwrap_or("执行失败")
        );
        return Ok(actions[0].clone());
    }
    ensure!(method == "POST" || method == "DELETE", "不支持的请求方法");
    let _guard = app.gate.lock().await;
    let mut c = app.store.config()?;
    match (method, path) {
        ("POST", "/api/bot/start") => {
            ensure!(
                app.store.unresolved()?.is_empty(),
                "有待核对订单，暂不能启动"
            );
            validate(&c)?;
            let ex = app.exchange(&c);
            ex.wallet().await?;
            // A fresh start re-baselines the circuit breakers on today's equity.
            let (equity, _, _) = app.account_risk(&ex, &app.store.open_trades()?).await?;
            app.store.set_state("equity_peak", &equity.to_string())?;
            app.store.set_state("breaker_since", &crate::model::now())?;
            c.running = true;
        }
        ("POST", "/api/bot/stop") => {
            c.running = false;
        }
        ("POST", "/api/key") => {
            ensure!(
                app.store.open_trades()?.is_empty() && app.store.unresolved()?.is_empty(),
                "有持仓或待核对订单时不能更换 Key"
            );
            let credentials: Credentials = serde_json::from_value(body)?;
            ensure!(
                !credentials.key.trim().is_empty() && !credentials.secret.trim().is_empty(),
                "Key 和 Secret 都要填"
            );
            c.exchange = Credentials {
                key: credentials.key.trim().into(),
                secret: credentials.secret.trim().into(),
            };
            let ex = app.exchange(&c);
            let info = ex.get("/v5/user/query-api", json!({}), true).await?;
            let perms = info["permissions"].to_string();
            ensure!(!perms.contains("Withdraw"), "Key 具有提现权限，不予保存");
            ensure!(num(&info["readOnly"]) == 0.0, "Key 是只读的");
            if c.market == "spot" {
                ensure!(perms.contains("SpotTrade"), "Key 缺少现货交易权限");
            } else {
                ensure!(
                    perms.contains("Order") && perms.contains("Position"),
                    "Key 缺少合约权限"
                );
            }
            let wallet = ex.wallet().await?;
            let balance = usdt(&wallet).0;
            app.store.save_config(&c)?;
            return Ok(
                json!({"withdraw":false,"read_only":false,"ip_bound":info["ips"].as_array().is_some_and(|a|!a.is_empty()&&!a.contains(&json!("*"))),"usdt":balance,"spot":perms.contains("SpotTrade"),"futures":perms.contains("Order")}),
            );
        }
        ("POST", "/api/capital") => {
            c.capital = if body["capital"].is_null() {
                None
            } else {
                Some(body["capital"].as_f64().context("金额无效")?)
            };
            validate(&c)?;
            if let Some(cap) = c.capital {
                let held: f64 = app
                    .store
                    .open_trades()?
                    .iter()
                    .map(|t| t.stake_amount)
                    .sum();
                ensure!(cap >= held, "资金上限低于现有持仓本金");
                let balance = usdt(&app.exchange(&c).wallet().await?).0;
                ensure!(cap <= balance, "资金上限超过账户 USDT 权益");
            }
        }
        ("POST", "/api/strategy") => {
            let mut new = c.clone();
            new.market = body["market"].as_str().context("缺少 market")?.into();
            new.pairs = body["pairs"]
                .as_array()
                .context("缺少 pairs")?
                .iter()
                .map(|s| s.as_str().unwrap_or("").trim().to_uppercase())
                .collect();
            new.pairs.sort();
            new.pairs.dedup();
            // Direction, position count and per-trade risk are the AI's decisions; the
            // system only keeps its hard limits.
            new.max_open = crate::model::MAX_OPEN;
            new.risk_per_trade_pct = crate::model::MAX_RISK_PER_TRADE_PCT;
            new.max_same_direction = 0;
            new.leverage = if new.market == "spot" {
                1
            } else {
                body["leverage"].as_u64().context("杠杆无效")? as u32
            };
            new.short = new.market == "futures";
            validate(&new)?;
            ensure!(
                app.store.unresolved()?.is_empty(),
                "有待核对订单，不能修改策略"
            );
            let held = app.store.open_trades()?;
            if new.market != c.market || new.leverage != c.leverage || new.short != c.short {
                ensure!(held.is_empty(), "先平掉现有持仓再修改市场、杠杆或做空");
                let ex = app.exchange(&c);
                ex.wallet().await?;
                ensure!(
                    ex.open_orders(&c.market).await?.is_empty(),
                    "Bybit 仍有挂单，不能修改"
                );
                if new.market == "futures" || c.market == "futures" {
                    ensure!(
                        !ex.positions().await?.iter().any(|p| num(&p["size"]) > 0.0),
                        "Bybit 仍有合约仓位，不能切换"
                    );
                }
            }
            ensure!(held.len() <= new.max_open, "新持仓上限小于现有持仓数");
            let known = markets(app, &new.market).await?;
            for pair in &new.pairs {
                let row = known
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|r| r["pair"] == *pair)
                    .context("所选交易对不可交易或是稳定币")?;
                ensure!(
                    new.market == "spot" || num(&row["max_lev"]) >= new.leverage as f64,
                    "所选币种杠杆上限不足"
                );
            }
            c = new;
        }
        ("POST", "/api/ai/config") => {
            // Model fields belong to a channel (the active one unless `channel` names another);
            // everything else is shared by all channels.
            const CHANNEL_KEYS: [&str; 12] = [
                "name",
                "provider",
                "api_protocol",
                "reasoning_effort",
                "claude_thinking",
                "claude_effort",
                "claude_thinking_budget",
                "stream",
                "model",
                "api_key",
                "base_url",
                "id",
            ];
            let obj = body.as_object().context("AI 配置无效")?;
            let target = obj
                .get("channel")
                .and_then(Value::as_str)
                .unwrap_or(&c.ai.active_channel)
                .to_string();
            let idx =
                c.ai.channels
                    .iter()
                    .position(|ch| ch.id == target)
                    .context("渠道不存在")?;
            let mut value = json!(c.ai);
            for (k, v) in obj {
                if matches!(
                    k.as_str(),
                    "enabled" | "channel" | "channels" | "active_channel" | "id"
                ) {
                    continue;
                }
                if k == "hooks" {
                    // Settings merge in; the webhook secret only changes through its own call.
                    for (hk, hv) in v.as_object().context("钩子配置无效")? {
                        if hk != "webhook_token" && hk != "webhook_set" {
                            value["hooks"][hk] = hv.clone();
                        }
                    }
                    continue;
                }
                if k == "api_key" && (v.is_null() || v == "") {
                    continue;
                }
                if CHANNEL_KEYS.contains(&k.as_str()) {
                    value["channels"][idx][k] = v.clone();
                } else {
                    value[k] = v.clone();
                }
            }
            let mut a: AiConfig = serde_json::from_value(value)?;
            validate_channel(&a, &a.channels[idx])?;
            a.normalize();
            crate::ai::validate_schedule(a.interval, a.delay_seconds)?;
            let h = &a.hooks;
            ensure!(
                (h.big_move_pct == 0.0 || (0.5..=20.0).contains(&h.big_move_pct))
                    && (1..=120).contains(&h.min_gap_minutes)
                    && (1..=60).contains(&h.max_per_hour),
                "钩子参数超出范围"
            );
            ensure!(
                (0.0..=1.0).contains(&a.min_confidence)
                    && (0.5..=50.0).contains(&a.max_daily_loss_pct)
                    && (a.max_drawdown_pct == 0.0 || (1.0..=90.0).contains(&a.max_drawdown_pct))
                    && a.max_consecutive_losses <= 50,
                "AI 风控参数超出范围"
            );
            c.ai = a;
            app.store.save_config(&c)?;
            return Ok(ai_public(&c.ai));
        }
        ("POST", "/api/knowledge") => {
            ensure!(app.store.knowledge()?.len() < 40, "最多保存 40 条笔记");
            let (title, content, source, enabled) = knowledge_fields(&body, None)?;
            let id = app
                .store
                .knowledge_add(&title, &content, &source, enabled, "user")?;
            return Ok(json!({"id":id}));
        }
        _ if method == "POST" && path.starts_with("/api/knowledge/") => {
            let id: i64 = path.trim_start_matches("/api/knowledge/").parse()?;
            let current = app
                .store
                .knowledge()?
                .into_iter()
                .find(|k| k["id"] == id)
                .context("笔记不存在")?;
            let (title, content, source, enabled) = knowledge_fields(&body, Some(&current))?;
            app.store
                .knowledge_update(id, &title, &content, &source, enabled)?;
            return Ok(json!({}));
        }
        _ if method == "DELETE" && path.starts_with("/api/knowledge/") => {
            let id: i64 = path.trim_start_matches("/api/knowledge/").parse()?;
            ensure!(app.store.knowledge_delete(id)?, "笔记不存在");
            return Ok(json!({}));
        }
        ("POST", "/api/ai/hooks/token") => {
            let bytes: [u8; 24] = rand::random();
            c.ai.hooks.webhook_token = hex::encode(bytes);
            let token = c.ai.hooks.webhook_token.clone();
            app.store.save_config(&c)?;
            // The only time the full secret is shown.
            return Ok(json!({"token":token}));
        }
        ("DELETE", "/api/ai/hooks/token") => {
            c.ai.hooks.webhook_token.clear();
            app.store.save_config(&c)?;
            return Ok(ai_public(&c.ai));
        }
        ("POST", "/api/ai/channels") => {
            ensure!(c.ai.channels.len() < 8, "最多保存 8 个渠道");
            let provider = body["provider"].as_str().unwrap_or("anthropic");
            ensure!(matches!(provider, "anthropic" | "openai"), "模型服务商无效");
            let mut ch = Channel {
                id: format!("c{}", &uuid::Uuid::new_v4().simple().to_string()[..8]),
                name: body["name"].as_str().unwrap_or("").trim().into(),
                provider: provider.into(),
                ..Channel::default()
            };
            if provider == "openai" {
                ch.model = "deepseek-chat".into();
            }
            validate_channel(&c.ai, &ch)?;
            let id = ch.id.clone();
            c.ai.channels.push(ch);
            app.store.save_config(&c)?;
            let mut out = ai_public(&c.ai);
            out["created"] = json!(id);
            return Ok(out);
        }
        ("POST", "/api/ai/channels/select") => {
            let id = body["id"].as_str().context("缺少渠道")?;
            let ch =
                c.ai.channels
                    .iter()
                    .find(|ch| ch.id == id)
                    .context("渠道不存在")?;
            ensure!(!ch.api_key.is_empty(), "这个渠道还没有保存 API Key");
            // Never swap the model underneath an analysis that is already running.
            let _idle = app
                .ai_gate
                .try_lock()
                .map_err(|_| anyhow::anyhow!("正在分析，等这一轮结束后再切换渠道"))?;
            c.ai.active_channel = id.into();
            app.store.save_config(&c)?;
            return Ok(ai_public(&app.store.config()?.ai));
        }
        ("DELETE", p) if p.starts_with("/api/ai/channels/") => {
            let id = p.trim_start_matches("/api/ai/channels/");
            ensure!(c.ai.channels.len() > 1, "至少保留一个渠道");
            ensure!(
                id != c.ai.active_channel,
                "不能删除正在使用的渠道，请先切换到其他渠道"
            );
            let before = c.ai.channels.len();
            c.ai.channels.retain(|ch| ch.id != id);
            ensure!(c.ai.channels.len() < before, "渠道不存在");
            app.store.save_config(&c)?;
            return Ok(ai_public(&c.ai));
        }
        ("POST", "/api/ai/enable") => {
            let enabled = body["enabled"].as_bool().context("enabled 无效")?;
            ensure!(!enabled || !c.ai.api_key.is_empty(), "先保存大模型 API Key");
            c.ai.enabled = enabled;
        }
        ("POST", "/api/telegram") => {
            let token = body["token"].as_str().context("缺少 Token")?.trim();
            ensure!(token.contains(':'), "Token 格式无效");
            let me = app.tg(token, "getMe", json!({})).await?;
            let chat = if c.telegram.token == token && !c.telegram.chat_id.is_empty() {
                c.telegram.chat_id.clone()
            } else {
                let updates = app.tg(token, "getUpdates", json!({})).await?;
                let mut chats = std::collections::BTreeSet::new();
                for u in updates.as_array().context("无法读取 Telegram 消息")? {
                    let chat = &u["message"]["chat"];
                    if chat["type"] == "private" {
                        chats.insert(chat["id"].to_string());
                    }
                }
                ensure!(
                    chats.len() == 1,
                    "请使用只有你发过消息的 Bot，并先发送 /start"
                );
                chats.into_iter().next().unwrap()
            };
            app.tg(
                token,
                "sendMessage",
                json!({"chat_id":chat,"text":"✅ Tide Rust 已连接"}),
            )
            .await?;
            c.telegram = crate::model::Telegram {
                enabled: true,
                token: token.into(),
                chat_id: chat,
            };
            app.store.save_config(&c)?;
            return Ok(json!({"bot":me["username"]}));
        }
        ("POST", "/api/telegram/test") => {
            ensure!(c.telegram.enabled, "尚未连接 Telegram");
            app.telegram(&c, "🔔 Tide 测试通知").await?;
            return Ok(json!({}));
        }
        ("DELETE", "/api/telegram") => {
            c.telegram = Default::default();
        }
        _ if method == "POST" && path.starts_with("/api/trades/") && path.ends_with("/exit") => {
            let id = path
                .trim_start_matches("/api/trades/")
                .trim_end_matches("/exit")
                .parse()?;
            app.close_position(id, "force_exit").await?;
            return Ok(json!({}));
        }
        _ => bail!("接口不存在"),
    }
    app.store.save_config(&c)?;
    Ok(json!({}))
}

pub async fn markets(app: &App, market: &str) -> Result<Value> {
    ensure!(matches!(market, "spot" | "futures"), "市场无效");
    let ex = app.exchange(&app.store.config()?);
    let ins = ex.instruments(market).await?;
    let ticks = ex.tickers(market).await?;
    let stable = [
        "USDC", "USDE", "USD1", "RLUSD", "FDUSD", "DAI", "PYUSD", "TUSD", "USDD", "BUSD", "USDP",
        "EURI", "USDR", "USDQ",
    ];
    let mut rows = vec![];
    for m in ins {
        if m["quoteCoin"] != "USDT"
            || m["status"] != "Trading"
            || stable.contains(&m["baseCoin"].as_str().unwrap_or(""))
        {
            continue;
        }
        if market == "futures"
            && (m["settleCoin"] != "USDT" || m["contractType"] != "LinearPerpetual")
        {
            continue;
        }
        let t = ticks["list"]
            .as_array()
            .and_then(|a| a.iter().find(|t| t["symbol"] == m["symbol"]));
        let empty = Value::Null;
        let t = t.unwrap_or(&empty);
        let base = m["baseCoin"].as_str().unwrap_or("");
        let pair = format!(
            "{}/USDT{}",
            base,
            if market == "futures" { ":USDT" } else { "" }
        );
        rows.push(json!({"pair":pair,"base":base,"price":num(&t["lastPrice"]),"change":num(&t["price24hPcnt"])*100.0,"volume":num(&t["turnover24h"]),"max_lev":num(&m["leverageFilter"]["maxLeverage"])}));
    }
    rows.sort_by(|a, b| num(&b["volume"]).total_cmp(&num(&a["volume"])));
    Ok(json!(rows))
}
async fn holdings(app: &App) -> Result<Value> {
    let c = app.store.config()?;
    let ex = app.exchange(&c);
    let w = ex.wallet().await?;
    let positions = ex.positions().await?;
    let mut orders = vec![];
    for market in ["spot", "futures"] {
        for o in ex.open_orders(market).await? {
            orders.push(json!({"symbol":o["symbol"],"category":category(market),"side":o["side"],"type":o["orderType"],"qty":num(&o["qty"]),"price":num(&o["price"]),"trigger":num(&o["triggerPrice"]),"status":o["orderStatus"],"stop_type":o["stopOrderType"]}));
        }
    }
    let coins:Vec<_>=w["coin"].as_array().context("账户数据无效")?.iter().filter(|c|num(&c["walletBalance"])>0.0).map(|c|json!({"coin":c["coin"],"amount":num(&c["walletBalance"]),"usd":num(&c["usdValue"]),"locked":num(&c["locked"])})).collect();
    let positions:Vec<_>=positions.iter().filter(|p|num(&p["size"])>0.0).map(|p|{let mark=num(&p["markPrice"]);let liq=num(&p["liqPrice"]);json!({"symbol":p["symbol"],"side":if p["side"]=="Buy"{"long"}else{"short"},"size":num(&p["size"]),"leverage":num(&p["leverage"]),"entry":num(&p["avgPrice"]),"mark":mark,"liq":liq,"liq_dist_pct":if mark>0.0&&liq>0.0{Some((mark-liq).abs()/mark*100.0)}else{None},"value":num(&p["positionValue"]),"margin":num(&p["positionIM"]),"pnl":num(&p["unrealisedPnl"]),"tp":num(&p["takeProfit"]),"sl":num(&p["stopLoss"])})}).collect();
    Ok(
        json!({"equity":num(&w["totalEquity"]),"available":num(&w["totalAvailableBalance"]),"coins":coins,"positions":positions,"orders":orders}),
    )
}
pub async fn overview(app: &App) -> Result<Value> {
    let c = app.store.config()?;
    let ex = app.exchange(&c);
    let trades = app.store.trades()?;
    let held = app.store.open_trades()?;
    let closed: Vec<_> = trades
        .iter()
        .filter(|t| t.close_date.is_some() && t.market == c.market)
        .collect();
    let profit: f64 = closed.iter().map(|t| t.profit_abs).sum();
    let mut daily = std::collections::BTreeMap::<String, f64>::new();
    for t in &closed {
        *daily
            .entry(t.close_date.as_ref().unwrap()[..10].into())
            .or_default() += t.profit_abs;
    }
    let days: Vec<_> = (0..30)
        .map(|i| {
            let day = (chrono::Utc::now() - chrono::Duration::days(i))
                .format("%Y-%m-%d")
                .to_string();
            json!({"date":day,"abs_profit":daily.get(&day).copied().unwrap_or(0.0)})
        })
        .collect();
    let mut out = json!({"engine":"rust","server_ip":std::env::var("SERVER_IP").unwrap_or_default(),"key":mask(&c.exchange.key),"telegram":c.telegram.enabled,"capital":c.capital,"market":c.market,"leverage":c.leverage,"short":c.short,"pairs":c.pairs,"max_open":c.max_open,"max_pairs":12,"max_open_limit":5,"max_leverage":25,"risk_per_trade_pct":c.risk_per_trade_pct,"max_same_direction":c.max_same_direction,"stop_rule":"AI 设置，2%–10%","ai":c.ai.enabled,"state":if c.running{"running"}else{"stopped"},"strategy":"AI","timeframe":"4h","open":[],"watch":[],"daily":days,"recent":closed.iter().take(5).collect::<Vec<_>>(),"profit":{"profit_closed_coin":profit,"profit_closed_ratio_mean":if closed.is_empty(){0.0}else{closed.iter().map(|t|t.profit_ratio).sum::<f64>()/closed.len() as f64},"closed_trade_count":closed.len(),"winning_trades":closed.iter().filter(|t|t.profit_abs>0.0).count(),"losing_trades":closed.iter().filter(|t|t.profit_abs<=0.0).count()}});
    let result:Result<()> =async {
        let ticks=ex.tickers(&c.market).await?;let mut open=vec![];for t in &held{let p=ticks["list"].as_array().and_then(|a|a.iter().find(|p|p["symbol"]==symbol(&t.pair))).context("持仓行情缺失")?;open.push(t.mark(num(&p["lastPrice"])));}
        out["open"]=json!(open);
        if !c.exchange.key.is_empty(){let w=ex.wallet().await?;
            let spot_value:f64=held.iter().filter(|t|t.market=="spot").filter_map(|t|open.iter().find(|o|o["trade_id"]==t.trade_id).map(|o|t.amount*num(&o["current_rate"]))).sum();
            out["balance"]=json!({"total":usdt(&w).0+spot_value,"symbol":"USDT"});}
        let mut watch=vec![];for pair in &c.pairs{let cs=ex.candles(&c.market,pair,240,300,None).await?;if cs.len()<220{watch.push(json!({"pair":pair,"state":"loading"}));continue;}
            let xs=indicators::calculate(&cs);let x=xs.last().unwrap();let price=cs.last().unwrap().close;let held=held.iter().find(|t|t.pair==*pair);
            let state=if held.is_some(){"holding"}else{"waiting_ai"};
            watch.push(json!({"pair":pair,"state":state,"price":price,"side":if price>x.ema200{"long"}else{"short"},"above200":price>x.ema200,"dist200":(price-x.ema200)/x.ema200,"golden":x.ema20>x.ema50,"gap":(x.ema20-x.ema50)/x.ema50,"adx":x.adx,"trending":x.adx>20.0,"holding_short":held.is_some_and(|t|t.is_short)}));
        }out["watch"]=json!(watch);Ok(())
    }.await;
    if let Err(e) = result {
        out["state"] = json!("offline");
        out["error"] = json!(e.to_string());
    }
    Ok(out)
}
