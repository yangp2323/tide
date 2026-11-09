use super::*;
use axum::{
    body::to_bytes,
    extract::{Request, State},
    response::IntoResponse,
};
use std::collections::HashMap;
use std::future::IntoFuture;

#[test]
fn capital_limit_does_not_require_funding_all_slots() {
    let mut cfg = model::Config {
        max_open: model::MAX_OPEN,
        ..model::Config::default()
    };
    for capital in [0.5, 6.0, 20.0] {
        cfg.capital = Some(capital);
        api::validate(&cfg).unwrap();
    }
    for capital in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        cfg.capital = Some(capital);
        assert!(api::validate(&cfg).is_err());
    }
}

#[test]
fn leverage_cap_is_25_for_futures_only() {
    let mut cfg = model::Config {
        market: "futures".into(),
        ..model::Config::default()
    };
    for leverage in [1, 10, 25] {
        cfg.leverage = leverage;
        api::validate(&cfg).unwrap();
    }
    for leverage in [0, 26] {
        cfg.leverage = leverage;
        assert!(api::validate(&cfg).is_err());
    }
    // The minimum sits between 1x and the maximum.
    cfg.leverage = 10;
    for min in [1, 5, 10] {
        cfg.min_leverage = min;
        api::validate(&cfg).unwrap();
    }
    for min in [0, 11] {
        cfg.min_leverage = min;
        assert!(api::validate(&cfg).is_err());
    }
    cfg.min_leverage = 1;
    cfg.market = "spot".into();
    cfg.leverage = 25;
    assert!(api::validate(&cfg).is_err());
}

/// BYBIT_BASE_URL is process-wide, so tests that point it at a mock take turns.
static EXCHANGE: Mutex<()> = Mutex::const_new(());

#[derive(Default)]
struct Mock {
    orders: HashMap<String, Value>,
    count: usize,
}
async fn bybit(State(mock): State<Arc<Mutex<Mock>>>, req: Request) -> axum::response::Response {
    let path = req.uri().path().to_string();
    let q: HashMap<String, String> =
        url::form_urlencoded::parse(req.uri().query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    let bytes = to_bytes(req.into_body(), 65536).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(json!({}));
    let mut mock = mock.lock().await;
    if path == "/v5/position/list" {
        return axum::Json(json!({"retCode":10001,"retMsg":"simulated position lookup failure"}))
            .into_response();
    }
    let result = match path.as_str() {
        "/v5/account/wallet-balance" => {
            json!({"list":[{"coin":[{"coin":"USDT","equity":"100","walletBalance":"100","locked":"0","totalPositionIM":"0","totalOrderIM":"0"}]}]})
        }
        "/v5/market/instruments-info" => {
            json!({"list":[{"status":"Trading","lotSizeFilter":{"basePrecision":"0.01","minOrderAmt":"5","minOrderQty":"0.01","maxMarketOrderQty":"1000"}}]})
        }
        "/v5/market/tickers" => json!({"list":[{"lastPrice":"10"}]}),
        "/v5/market/kline" => {
            // Flat 4h candles with a 0.1 range: ATR 0.1, so the stop is 2.5% at price 10.
            let end = chrono::Utc::now().timestamp_millis() / 14_400_000 * 14_400_000 - 14_400_000;
            json!({"list":(0..300).map(|i| json!([(end - i * 14_400_000).to_string(),"10","10.05","9.95","10","1"])).collect::<Vec<_>>()})
        }
        "/v5/order/create" => {
            mock.count += 1;
            let id = format!("order{}", mock.count);
            let mut o = body.clone();
            o["orderId"] = json!(id);
            o["orderStatus"] = json!("Filled");
            o["cumExecQty"] = body["qty"].clone();
            o["avgPrice"] = json!("10");
            mock.orders
                .insert(body["orderLinkId"].as_str().unwrap().into(), o);
            json!({"orderId":id})
        }
        "/v5/order/realtime" | "/v5/order/history" => {
            json!({"list":q.get("orderLinkId").and_then(|id|mock.orders.get(id)).cloned().into_iter().collect::<Vec<_>>()})
        }
        "/v5/execution/list" => {
            let o = mock
                .orders
                .values()
                .find(|o| o["orderId"].as_str() == q.get("orderId").map(String::as_str))
                .unwrap();
            let buy = o["side"] == "Buy";
            let qty = model::num(&o["qty"]);
            json!({"list":[{"execType":"Trade","execPrice":"10","execFee":(qty*if buy{0.001}else{0.01}).to_string(),"feeCurrency":if buy{"BTC"}else{"USDT"}}]})
        }
        _ => panic!("Unexpected exchange request {}", path),
    };
    axum::Json(json!({"retCode":0,"retMsg":"OK","result":result})).into_response()
}
#[tokio::test]
async fn execution_is_durable_and_closing_releases_position_slots() {
    let _env = EXCHANGE.lock().await;
    let mock = Arc::new(Mutex::new(Mock::default()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new().fallback(bybit).with_state(mock.clone()),
        )
        .into_future(),
    );
    std::env::set_var("BYBIT_BASE_URL", format!("http://{}", addr));
    let dir = std::env::temp_dir().join(format!("tide-test-{}", uuid::Uuid::new_v4()));
    let store = store::Store::open(&dir).unwrap();
    let mut cfg = model::Config {
        running: true,
        ..model::Config::default()
    };
    cfg.ai.enabled = true;
    // The loop below closes the same slot at a loss many times on purpose.
    cfg.ai.max_consecutive_losses = 0;
    // The exchange accepts 5 USDT; concurrent holdings, not past entries, limit trading.
    cfg.capital = Some(6.0);
    cfg.max_open = 1;
    // This test is about durable orders, not sizing: each entry asks for more margin than the
    // 6 USDT capital allows and is lowered to 95% of it, 5.7. (Its repeated losing closes would
    // otherwise calibrate the 60% forecast down and shrink the suggested size.)
    api::validate(&cfg).unwrap();
    cfg.exchange = model::Credentials {
        key: "test".into(),
        secret: "secret".into(),
    };
    store.save_config(&cfg).unwrap();
    for i in 0..20 {
        let id = format!("old{}", i);
        store.reserve(&id, "entry", "SOL/USDT", &json!({})).unwrap();
        store.order_state(&id, "filled").unwrap();
    }
    let app = Arc::new(App {
        store,
        http: reqwest::Client::new(),
        auth: auth::Auth::create("admin".into(), "password").unwrap(),
        secret: "01234567890123456789012345678901".into(),
        origin: "https://trade.example.com".into(),
        gate: Mutex::new(()),
        ai_gate: Mutex::new(()),
        ai_state: Mutex::new(json!({})),
        login_attempts: Mutex::new(vec![]),
        hooks: Default::default(),
    });
    let actions = json!([{"action":"open_long","pair":"BTC/USDT","confidence":0.9,"win_prob":0.6,"margin_usdt":100},{"action":"open_long","pair":"ETH/USDT","confidence":0.9,"win_prob":0.6,"margin_usdt":100}]);
    let result = app
        .execute_actions(actions.as_array().unwrap(), false)
        .await
        .unwrap();
    assert_eq!(result[0]["status"], "executed", "{}", result[0]);
    assert_eq!(result[1]["status"], "failed");
    assert_eq!(result[1]["note"], "已达到持仓上限");
    assert_eq!(mock.lock().await.count, 1);
    let held = app.store.open_trades().unwrap();
    assert_eq!(held.len(), 1);
    assert!((held[0].stake_amount - 5.7).abs() < 1e-8);
    assert!((held[0].stop_dist - 0.025).abs() < 1e-9);
    assert!((held[0].stop_loss_abs - 9.75).abs() < 1e-9);
    let response = api::dispatch(
        State(app.clone()),
        Request::builder()
            .uri("/api/ai")
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await;
    let status: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(status["position_count"], 1);
    assert_eq!(status["max_open"], 1);
    assert!(status.get("entries_today").is_none());
    assert!(status["config"].get("max_entries_per_day").is_none());
    assert!(held[0].amount < model::num(&mock.lock().await.orders.values().next().unwrap()["qty"]));
    {
        let _lock = app.gate.lock().await;
        app.close_position(held[0].trade_id, "force_exit")
            .await
            .unwrap();
    }
    assert!(app.store.open_trades().unwrap().is_empty());
    assert!(app.store.unresolved().unwrap().is_empty());
    assert_eq!(mock.lock().await.count, 2);
    // The AI cannot abandon an entry within the minimum holding time; the operator can.
    let result = app
        .execute_actions(&[actions[0].clone()], false)
        .await
        .unwrap();
    assert_eq!(result[0]["status"], "executed", "{}", result[0]);
    let close = json!({"action":"close","pair":"BTC/USDT","confidence":0.9});
    let result = app
        .execute_actions(std::slice::from_ref(&close), false)
        .await
        .unwrap();
    assert_eq!(result[0]["status"], "failed", "{}", result[0]);
    assert!(result[0]["note"].as_str().unwrap().contains("set_stops"));
    assert_eq!(app.store.open_trades().unwrap().len(), 1);
    let result = app
        .execute_actions(std::slice::from_ref(&close), true)
        .await
        .unwrap();
    assert_eq!(result[0]["status"], "executed", "{}", result[0]);
    assert!(app.store.open_trades().unwrap().is_empty());
    // Repeatedly reuse the same slot on the same day, including close + reopen in one batch.
    for _ in 0..3 {
        let result = app
            .execute_actions(&[actions[0].clone()], false)
            .await
            .unwrap();
        assert_eq!(result[0]["status"], "executed", "{}", result[0]);
        age_open_trades(&app, model::MIN_HOLD_BEFORE_AI_CLOSE_MINUTES);
        // Close is processed before open even if the model lists open first.
        let result = app
            .execute_actions(
                &[
                    actions[0].clone(),
                    json!({"action":"close","pair":"BTC/USDT"}),
                ],
                false,
            )
            .await
            .unwrap();
        assert!(
            result.iter().all(|a| a["status"] == "executed"),
            "{:?}",
            result
        );
        age_open_trades(&app, model::MIN_HOLD_BEFORE_AI_CLOSE_MINUTES);
        let result = app
            .execute_actions(&[json!({"action":"close","pair":"BTC/USDT"})], false)
            .await
            .unwrap();
        assert_eq!(result[0]["status"], "executed");
        assert!(app.store.open_trades().unwrap().is_empty());
    }
    let response = api::dispatch(
        State(app.clone()),
        Request::builder()
            .uri("/api/ai")
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await;
    let status: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(status["position_count"], 0);
    // 2 for the first round trip, 2 for the held-back close, 4 for each of the 3 loops.
    assert_eq!(mock.lock().await.count, 16);
    app.store
        .reserve("uncertain", "entry", "SOL/USDT", &json!({"trade":held[0]}))
        .unwrap();
    // Reopening the database retains reservations; a process restart cannot reset risk limits.
    let reopened = store::Store::open(&dir).unwrap();
    assert_eq!(reopened.unresolved().unwrap().len(), 1);
    drop(reopened);
    // One broken futures position must not prevent a different spot stop-loss.
    // Pausing entries also must not disable risk exits.
    let mut healthy = held[0].clone();
    healthy.trade_id = 0;
    healthy.amount = 1.0;
    healthy.open_rate = 100.0;
    healthy.stop_loss_abs = 92.0;
    healthy.peak = 100.0;
    app.store.insert_trade(healthy.clone(), "").unwrap();
    let mut broken = healthy;
    broken.pair = "ETH/USDT:USDT".into();
    broken.market = "futures".into();
    app.store.insert_trade(broken, "").unwrap();
    let mut cfg = app.store.config().unwrap();
    cfg.running = false;
    app.store.save_config(&cfg).unwrap();
    {
        let _lock = app.gate.lock().await;
        assert!(app.tick().await.is_err());
    }
    let remaining = app.store.open_trades().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].market, "futures");
    assert_eq!(mock.lock().await.count, 17);
    assert!(!app.store.state("risk_error").unwrap().unwrap().is_empty());
    // Test the actual HTTP authentication / origin middleware without any live exchange traffic.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = Router::new()
        .route("/api/{*path}", any(api::dispatch))
        .layer(middleware::from_fn_with_state(app.clone(), auth::protect))
        .with_state(app.clone());
    let api_server = tokio::spawn(axum::serve(listener, router).into_future());
    let http = reqwest::Client::new();
    let url = format!("http://{}/api", addr);
    assert_eq!(
        http.get(format!("{}/logs", url))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        http.post(format!("{}/auth/login", url))
            .header("origin", "https://evil.example")
            .json(&json!({"username":"admin","password":"password"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let r = http
        .post(format!("{}/auth/login", url))
        .header("origin", &app.origin)
        .json(&json!({"username":"admin","password":"password"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let cookie = r.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    assert_eq!(
        http.get(format!("{}/logs", url))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let response = http
        .post(format!("{}/ai/config", url))
        .header("cookie", &cookie)
        .header("origin", &app.origin)
        .json(&json!({"interval":7,"delay_seconds":20,"api_protocol":"responses","reasoning_effort":"high","claude_thinking":"adaptive","claude_effort":"high","claude_thinking_budget":4096,"stream":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let config: Value = response.json().await.unwrap();
    assert_eq!(config["interval"], 7);
    assert_eq!(config["delay_seconds"], 20);
    assert_eq!(config["api_protocol"], "responses");
    assert_eq!(config["reasoning_effort"], "high");
    assert_eq!(config["claude_thinking"], "adaptive");
    assert_eq!(config["claude_effort"], "high");
    assert_eq!(config["claude_thinking_budget"], 4096);
    assert_eq!(config["stream"], true);
    for invalid in [
        json!({"interval":0}),
        json!({"interval":1441}),
        json!({"delay_seconds":420}),
        json!({"delay_seconds":-1}),
        json!({"api_protocol":"unknown"}),
        json!({"reasoning_effort":"unknown"}),
        json!({"claude_thinking":"unknown"}),
        json!({"claude_effort":"unknown"}),
        json!({"claude_thinking":"enabled","claude_thinking_budget":1023}),
        json!({"claude_thinking":"enabled","claude_thinking_budget":16000}),
        json!({"claude_thinking":"disabled","claude_effort":"max"}),
    ] {
        let r = http
            .post(format!("{}/ai/config", url))
            .header("cookie", &cookie)
            .header("origin", &app.origin)
            .json(&invalid)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400);
        assert_eq!(app.store.config().unwrap().ai.interval, 7);
        assert_eq!(app.store.config().unwrap().ai.delay_seconds, 20);
        assert_eq!(app.store.config().unwrap().ai.api_protocol, "responses");
        assert_eq!(app.store.config().unwrap().ai.reasoning_effort, "high");
        assert_eq!(app.store.config().unwrap().ai.claude_thinking, "adaptive");
        assert_eq!(app.store.config().unwrap().ai.claude_effort, "high");
    }
    // Several API channels can be saved; one is in use and the rest are kept.
    let send = |path: &str, method: &str, body: Value| {
        let rb = if method == "DELETE" {
            http.delete(format!("{}{}", url, path))
        } else {
            http.post(format!("{}{}", url, path))
        };
        rb.header("cookie", &cookie)
            .header("origin", &app.origin)
            .json(&body)
            .send()
    };
    // The knowledge base starts with the built-in notes and can be edited.
    let r = http
        .get(format!("{}/knowledge", url))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let notes: Value = r.json().await.unwrap();
    assert!(notes.as_array().unwrap().len() >= 6);
    let r = send(
        "/knowledge",
        "POST",
        json!({"title":"我的想法","content":"只在 ADX 大于 25 时做突破"}),
    )
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    let id = r.json::<Value>().await.unwrap()["id"].as_i64().unwrap();
    assert_eq!(
        send("/knowledge", "POST", json!({"title":"","content":"x"}))
            .await
            .unwrap()
            .status(),
        400
    );
    let r = send(
        &format!("/knowledge/{}", id),
        "POST",
        json!({"enabled":false}),
    )
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    let mine = app
        .store
        .knowledge()
        .unwrap()
        .into_iter()
        .find(|k| k["id"] == id)
        .unwrap();
    assert_eq!(mine["enabled"], false);
    assert_eq!(mine["title"], "我的想法");
    assert_eq!(
        send(&format!("/knowledge/{}", id), "DELETE", json!({}))
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        send(&format!("/knowledge/{}", id), "DELETE", json!({}))
            .await
            .unwrap()
            .status(),
        400
    );
    // The webhook is reached with no login, only its secret; nothing works until one exists.
    let hook = |token: &str, body: &'static str| {
        http.post(format!("{}/hook/{}", url, token))
            .body(body)
            .send()
    };
    // An earlier step paused the bot; the webhook only queues while trading is on.
    let mut running = app.store.config().unwrap();
    running.running = true;
    running.ai.enabled = true;
    app.store.save_config(&running).unwrap();
    app.hooks.lock().unwrap().pending.clear();
    assert_eq!(hook("anything", "x").await.unwrap().status(), 404);
    let r = send("/ai/hooks/token", "POST", json!({})).await.unwrap();
    assert_eq!(r.status(), 200);
    let token = r.json::<Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(token.len(), 48);
    assert_eq!(hook("wrong", "x").await.unwrap().status(), 404);
    let r = hook(&token, "BTC 跌破 76000，看看要不要处理")
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["queued"], true);
    {
        let h = app.hooks.lock().unwrap();
        assert_eq!(h.pending.len(), 1);
        assert_eq!(h.pending[0]["type"], "webhook");
        assert_eq!(h.pending[0]["message"], "BTC 跌破 76000，看看要不要处理");
    }
    // Alerts that are JSON without a message field, or a bare number, still reach the model.
    for body in [r#"{"ticker":"BTCUSDT","action":"buy"}"#, "123"] {
        assert_eq!(hook(&token, body).await.unwrap().status(), 200);
    }
    {
        let mut h = app.hooks.lock().unwrap();
        assert!(h.pending[1]["message"]
            .as_str()
            .unwrap()
            .contains("BTCUSDT"));
        assert_eq!(h.pending[2]["message"], "123");
        h.pending.truncate(1);
    }
    // The secret is masked when the settings are read, and saving settings keeps it.
    let r = http
        .get(format!("{}/ai", url))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    let ai: Value = r.json().await.unwrap();
    assert_eq!(ai["config"]["hooks"]["webhook_set"], true);
    assert_ne!(ai["config"]["hooks"]["webhook_token"], token);
    let r = send(
        "/ai/config",
        "POST",
        json!({"hooks":{"big_move_pct":5.0,"webhook_token":"stolen"}}),
    )
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    let saved = app.store.config().unwrap().ai.hooks;
    assert_eq!(saved.big_move_pct, 5.0);
    assert_eq!(saved.webhook_token, token);
    for bad in [
        json!({"min_gap_minutes":0}),
        json!({"max_per_hour":0}),
        json!({"big_move_pct":0.1}),
    ] {
        let r = send("/ai/config", "POST", json!({"hooks":bad}))
            .await
            .unwrap();
        assert_eq!(r.status(), 400);
    }
    // Guessing is throttled.
    let mut last = 0;
    for _ in 0..12 {
        last = hook("guess", "x").await.unwrap().status().as_u16();
    }
    assert_eq!(last, 429);
    let r = http
        .delete(format!("{}/ai/hooks/token", url))
        .header("cookie", &cookie)
        .header("origin", &app.origin)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(app
        .store
        .config()
        .unwrap()
        .ai
        .hooks
        .webhook_token
        .is_empty());
    let r = send(
        "/ai/channels",
        "POST",
        json!({"name":"备用","provider":"openai"}),
    )
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    let config: Value = r.json().await.unwrap();
    let spare = config["created"].as_str().unwrap().to_string();
    assert_eq!(config["channels"].as_array().unwrap().len(), 2);
    assert_eq!(config["active_channel"], "default");
    assert_eq!(config["channels"][1]["model"], "deepseek-chat");
    assert!(config["channels"][1]["api_key"].is_null());
    // A channel without a key cannot be put in use.
    let r = send("/ai/channels/select", "POST", json!({"id":spare}))
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let r = send(
        "/ai/config",
        "POST",
        json!({"channel":spare,"api_key":"sk-test-key-12345","base_url":"https://relay.example.com","model":"gpt-x"}),
    )
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    let config: Value = r.json().await.unwrap();
    // Editing another channel leaves the one in use alone, and keys are never returned in full.
    assert_eq!(config["active_channel"], "default");
    assert_eq!(config["api_protocol"], "responses");
    assert_eq!(config["channels"][1]["model"], "gpt-x");
    assert_eq!(config["channels"][1]["api_key"], "sk-t…2345");
    let r = send("/ai/channels/select", "POST", json!({"id":spare}))
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let config: Value = r.json().await.unwrap();
    assert_eq!(config["active_channel"], spare);
    assert_eq!(config["model"], "gpt-x");
    assert_eq!(config["provider"], "openai");
    assert_eq!(config["interval"], 7);
    assert_eq!(
        app.store.config().unwrap().ai.base_url,
        "https://relay.example.com"
    );
    // The channel in use cannot be deleted; the last remaining one cannot either.
    assert_eq!(
        send(&format!("/ai/channels/{}", spare), "DELETE", json!({}))
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        send("/ai/channels/default", "DELETE", json!({}))
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(app.store.config().unwrap().ai.channels.len(), 1);
    assert_eq!(
        send(&format!("/ai/channels/{}", spare), "DELETE", json!({}))
            .await
            .unwrap()
            .status(),
        400
    );
    api_server.abort();
    server.abort();
    std::env::remove_var("BYBIT_BASE_URL");
    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}

// A small stateful Bybit: orders, linear positions with their stop / take profit, and
// conditional spot orders that a test can "fire".
#[derive(Default)]
struct Sim {
    futures: bool,
    price: f64,
    count: usize,
    orders: HashMap<String, Value>,
    positions: Vec<Value>,
    external: Vec<Value>,
    manual_orders: Vec<Value>,
    calls: Vec<(String, Value)>,
    /// USDT in the wallet; 1000 unless a test moves funds in or out.
    wallet: Option<f64>,
}
async fn sim_exchange(
    State(sim): State<Arc<Mutex<Sim>>>,
    req: Request,
) -> axum::response::Response {
    let path = req.uri().path().to_string();
    let q: HashMap<String, String> =
        url::form_urlencoded::parse(req.uri().query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    let bytes = to_bytes(req.into_body(), 65536).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(json!({}));
    let mut s = sim.lock().await;
    s.calls.push((path.clone(), body.clone()));
    let result = match path.as_str() {
        "/v5/account/wallet-balance" => {
            let usdt = s.wallet.unwrap_or(1000.0).to_string();
            json!({"list":[{"coin":[{"coin":"USDT","equity":usdt,"walletBalance":usdt,"locked":"0","totalPositionIM":"0","totalOrderIM":"0"}]}]})
        }
        "/v5/account/info" => json!({"marginMode":"ISOLATED_MARGIN"}),
        "/v5/account/transaction-log" => json!({"list":[]}),
        "/v5/market/instruments-info" => {
            json!({"list":[{"status":"Trading","lotSizeFilter":{"qtyStep":"0.1","basePrecision":"0.01","minNotionalValue":"5","minOrderAmt":"5","minOrderQty":"0.1","maxMktOrderQty":"10000","maxMarketOrderQty":"10000"},"priceFilter":{"tickSize":"0.01"},"leverageFilter":{"maxLeverage":"50"}}]})
        }
        "/v5/market/tickers" => {
            json!({"list":[{"lastPrice":s.price.to_string(),"fundingRate":"0.0001","openInterest":"12345","price24hPcnt":"0.01","turnover24h":"1000000"}]})
        }
        "/v5/market/kline" => {
            let step: i64 = if q.get("interval").is_some_and(|i| i == "D") {
                86_400_000
            } else {
                q.get("interval")
                    .and_then(|i| i.parse::<i64>().ok())
                    .unwrap_or(240)
                    * 60_000
            };
            let end = chrono::Utc::now().timestamp_millis() / step * step - step;
            if q.get("limit").is_some_and(|l| l == "2") {
                // The newest row is the candle that is still forming.
                return axum::Json(json!({"retCode":0,"result":{"list":[[(end + step).to_string(),"10","10.4","9.9","10.2","3"],[end.to_string(),"10","10.05","9.95","10","1"]]}})).into_response();
            }
            json!({"list":(0..300).map(|i| json!([(end - i * step).to_string(),"10","10.05","9.95","10","1"])).collect::<Vec<_>>()})
        }
        "/v5/position/list" => json!({"list":s.positions.clone()}),
        "/v5/position/set-leverage" => json!({}),
        "/v5/position/trading-stop" => {
            let sym = body["symbol"].clone();
            for p in s.positions.iter_mut().filter(|p| p["symbol"] == sym) {
                p["stopLoss"] = body["stopLoss"].clone();
                p["takeProfit"] = body["takeProfit"].clone();
            }
            json!({})
        }
        "/v5/order/create" => {
            s.count += 1;
            let id = format!("order{}", s.count);
            let mut o = body.clone();
            o["orderId"] = json!(id);
            if body.get("triggerPrice").is_some() {
                o["orderStatus"] = json!("Untriggered");
                o["cumExecQty"] = json!("0");
            } else {
                o["orderStatus"] = json!("Filled");
                o["cumExecQty"] = body["qty"].clone();
                o["avgPrice"] = json!(s.price.to_string());
                if s.futures {
                    let sym = body["symbol"].clone();
                    if body["reduceOnly"] == true {
                        s.positions.retain(|p| p["symbol"] != sym);
                    } else {
                        let side = body["side"].clone();
                        // One-way mode: more in the same direction grows the one position and
                        // keeps its stop and take profit.
                        if let Some(p) = s
                            .positions
                            .iter_mut()
                            .find(|p| p["symbol"] == sym && p["side"] == side)
                        {
                            let size = model::num(&p["size"]) + model::num(&body["qty"]);
                            p["size"] = json!(size.to_string());
                        } else {
                            s.positions.push(json!({"symbol":sym,"side":side,"size":body["qty"],"positionIdx":"0","stopLoss":body["stopLoss"],"takeProfit":body.get("takeProfit").cloned().unwrap_or(json!("0"))}));
                        }
                    }
                }
            }
            s.orders
                .insert(body["orderLinkId"].as_str().unwrap().into(), o);
            json!({"orderId":id})
        }
        "/v5/order/amend" => {
            let o = s
                .orders
                .get_mut(body["orderLinkId"].as_str().unwrap())
                .unwrap();
            o["triggerPrice"] = body["triggerPrice"].clone();
            json!({})
        }
        "/v5/order/cancel" => {
            let o = s
                .orders
                .get_mut(body["orderLinkId"].as_str().unwrap())
                .unwrap();
            o["orderStatus"] = json!("Cancelled");
            json!({})
        }
        "/v5/order/realtime" | "/v5/order/history" => match q.get("orderLinkId") {
            Some(id) => json!({"list":s.orders.get(id).cloned().into_iter().collect::<Vec<_>>()}),
            None if q.get("orderFilter").is_some_and(|f| f == "StopOrder") => {
                json!({"list":s.orders.iter().filter(|(_, o)| o["orderStatus"] == "Untriggered").map(|(k, o)| { let mut o = o.clone(); o["orderLinkId"] = json!(k); o }).collect::<Vec<_>>()})
            }
            None => json!({"list":s.manual_orders.clone()}),
        },
        "/v5/execution/list" => match q.get("orderId") {
            Some(id) => {
                let o = s
                    .orders
                    .values()
                    .find(|o| o["orderId"].as_str() == Some(id))
                    .unwrap();
                json!({"list":[{"execType":"Trade","execPrice":o["avgPrice"],"execFee":"0.01","feeCurrency":"USDT"}]})
            }
            None => json!({"list":s.external.clone()}),
        },
        _ => panic!("Unexpected exchange request {}", path),
    };
    axum::Json(json!({"retCode":0,"retMsg":"OK","result":result})).into_response()
}
async fn sim_app(
    sim: &Arc<Mutex<Sim>>,
    cfg: model::Config,
) -> (
    Arc<App>,
    std::path::PathBuf,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new().fallback(sim_exchange).with_state(sim.clone()),
        )
        .into_future(),
    );
    std::env::set_var("BYBIT_BASE_URL", format!("http://{}", addr));
    let dir = std::env::temp_dir().join(format!("tide-sim-{}", uuid::Uuid::new_v4()));
    let store = store::Store::open(&dir).unwrap();
    let mut cfg = cfg;
    cfg.running = true;
    cfg.ai.enabled = true;
    cfg.exchange = model::Credentials {
        key: "test".into(),
        secret: "secret".into(),
    };
    store.save_config(&cfg).unwrap();
    let app = Arc::new(App {
        store,
        http: reqwest::Client::new(),
        auth: auth::Auth::create("admin".into(), "password").unwrap(),
        secret: "01234567890123456789012345678901".into(),
        origin: "https://trade.example.com".into(),
        gate: Mutex::new(()),
        ai_gate: Mutex::new(()),
        ai_state: Mutex::new(json!({})),
        login_attempts: Mutex::new(vec![]),
        hooks: Default::default(),
    });
    (app, dir, server)
}
/// Moves every open trade's opening time back, as if it had been held for `minutes`.
fn age_open_trades(app: &App, minutes: i64) {
    let opened = (chrono::Utc::now() - chrono::Duration::minutes(minutes))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    for mut t in app.store.open_trades().unwrap() {
        t.open_date = opened.clone();
        app.store.update_trade(&t).unwrap();
    }
}

fn open_long(pair: &str) -> Value {
    json!({"action":"open_long","pair":pair,"confidence":0.9,"win_prob":0.6,"reason":"突破后回踩确认","stop_loss_pct":4.0,"take_profit_pct":8.0})
}

#[tokio::test]
async fn futures_stops_follow_ai_trail_and_external_take_profit() {
    let _env = EXCHANGE.lock().await;
    let sim = Arc::new(Mutex::new(Sim {
        futures: true,
        price: 10.0,
        ..Sim::default()
    }));
    let cfg = model::Config {
        market: "futures".into(),
        pairs: vec!["BTC/USDT:USDT".into()],
        // 1000 over 20 slots: a 50 USDT entry sits exactly at the per-entry minimum.
        max_open: 20,
        leverage: 5,
        short: true,
        ..model::Config::default()
    };
    let (app, dir, server) = sim_app(&sim, cfg).await;
    // 50 USDT of margin at 5x is a 250 USDT position: 25 contracts at 10.
    let mut action = open_long("BTC/USDT:USDT");
    action["margin_usdt"] = json!(50);
    let result = app.execute_actions(&[action], false).await.unwrap();
    assert_eq!(result[0]["status"], "executed", "{}", result[0]);
    let t = app.store.open_trades().unwrap().remove(0);
    assert!((t.stop_loss_abs - 9.6).abs() < 1e-9 && (t.take_profit_abs - 10.8).abs() < 1e-9);
    assert_eq!(t.leverage, 5);
    {
        let s = sim.lock().await;
        let create = &s
            .calls
            .iter()
            .find(|c| c.0 == "/v5/order/create")
            .unwrap()
            .1;
        assert_eq!(create["stopLoss"], "9.60");
        assert_eq!(create["takeProfit"], "10.80");
        assert_eq!(s.positions[0]["size"], "25.0");
    }
    assert_eq!(t.entry_reason, "突破后回踩确认");
    // The next run is shown its own notes and why each position was opened.
    app.store
        .decision(&json!({"time":"2026-01-01 00:00:00","source":"auto","execute":true,"analysis":"上一轮分析","memory":"BTC 多单：跌破 9.6 失效","actions":[]}))
        .unwrap();
    let snap = app.snapshot().await.unwrap();
    assert_eq!(snap["open_positions"][0]["entry_reason"], "突破后回踩确认");
    // Just opened: the model sees how long it has held the position.
    assert!(snap["open_positions"][0]["held_minutes"].as_i64().unwrap() < 2);
    assert_eq!(snap["exit_performance"]["sample_trades"], 0);
    assert!(snap["exit_performance"]["by_exit_reason"].is_object());
    assert_eq!(snap["open_positions"][0]["leverage"], 5);
    assert_eq!(snap["pairs"][0]["market_stats"]["last_price"], 10.0);
    assert_eq!(snap["pairs"][0]["forming_4h_candle"]["high_so_far"], 10.4);
    assert_eq!(snap["pairs"][0]["forming_4h_candle"]["last"], 10.2);
    // Short-term candles sit next to the 4h chart, each at its own interval.
    let short = &snap["pairs"][0]["short_term"];
    for (key, rows, minutes) in [("1h", 24, 60), ("15m", 32, 15)] {
        let list = short[key].as_array().unwrap();
        assert_eq!(list.len(), rows);
        let time = |row: &serde_json::Value| {
            chrono::NaiveDateTime::parse_from_str(row[0].as_str().unwrap(), "%Y-%m-%d %H:%M")
                .unwrap()
        };
        assert_eq!((time(&list[1]) - time(&list[0])).num_minutes(), minutes);
        assert_eq!(
            list[0].as_array().unwrap().len(),
            short["columns"].as_array().unwrap().len()
        );
    }
    assert_eq!(snap["account"]["available_usdt"], 1000.0);
    // Account-level loss limits belong to the system; the model is not shown them.
    let text = snap.to_string();
    for hidden in [
        "pnl_today",
        "max_daily_loss_pct",
        "max_drawdown_pct",
        "max_consecutive_losses",
        "drawdown_from_peak_pct",
        "risk_limits",
    ] {
        assert!(!text.contains(hidden), "{hidden}");
    }
    // The model allocates capital itself: it sees what it can commit, and no per-trade cap.
    assert!(!text.contains("max_risk_per_trade_pct"));
    // One open position holds 50 USDT of margin; the simulated wallet still reports 1000 free.
    assert!((snap["trade_limits"]["allocatable_usdt"].as_f64().unwrap() - 950.0).abs() < 1e-9);
    // A 5 USDT minimum at the safe leverage (capped at the configured 5x) needs 1 USDT of margin.
    assert_eq!(snap["pairs"][0]["sizing"]["min_order_usdt"], 5.0);
    assert_eq!(
        snap["pairs"][0]["sizing"]["min_margin_usdt_at_safe_leverage"],
        1.0
    );
    assert!(snap["daily_profit_target"].is_null());
    let mut config = app.store.config().unwrap();
    config.ai.daily_profit_target_pct = 2.0;
    app.store.save_config(&config).unwrap();
    let target = app.snapshot().await.unwrap()["daily_profit_target"].clone();
    assert_eq!(target["target_pct"], 2.0);
    assert!(target["target_usdt"].as_f64().unwrap() > 0.0);
    assert!(target["earned_today_usdt"].as_f64().unwrap() >= 0.0);
    config.ai.daily_profit_target_pct = 0.0;
    app.store.save_config(&config).unwrap();
    // The exchange's own view is sent live: the bot's position is flagged as its own and a
    // position and an order the operator placed by hand are shown as theirs.
    {
        let mut s = sim.lock().await;
        s.positions.push(json!({"symbol":"ETHUSDT","side":"Sell","size":"2","leverage":"3","avgPrice":"2000","markPrice":"1990","liqPrice":"2600","positionValue":"4000","positionIM":"1333","unrealisedPnl":"20","stopLoss":"0","takeProfit":"0","positionIdx":"0"}));
        s.manual_orders.push(json!({"symbol":"ETHUSDT","side":"Buy","orderType":"Limit","qty":"1","price":"1800","triggerPrice":"0","orderStatus":"New","orderLinkId":"my-own-order"}));
    }
    let live = app.snapshot().await.unwrap()["exchange_live"].clone();
    assert_eq!(live["available"], true, "{live}");
    let own = live["positions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["pair"] == "BTC/USDT:USDT")
        .unwrap()
        .clone();
    assert_eq!(own["managed_by_tide"], true);
    let manual = live["positions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["pair"] == "ETHUSDT")
        .unwrap()
        .clone();
    assert_eq!(manual["managed_by_tide"], false);
    assert_eq!(manual["side"], "short");
    assert!((manual["liquidation_distance_pct"].as_f64().unwrap() - 30.65).abs() < 0.1);
    assert_eq!(live["open_orders"][0]["placed_by_tide"], false);
    {
        let mut s = sim.lock().await;
        s.positions.retain(|p| p["symbol"] != "ETHUSDT");
        s.manual_orders.clear();
    }
    assert_eq!(
        snap["previous_runs"][0]["notes_to_self"],
        "BTC 多单：跌破 9.6 失效"
    );
    assert_eq!(snap["previous_runs"].as_array().unwrap().len(), 1);
    // The AI may tighten its stop but never loosen it.
    app.set_stops(t.trade_id, Some(9.8), None).await.unwrap();
    assert!(app.set_stops(t.trade_id, Some(9.0), None).await.is_err());
    assert_eq!(sim.lock().await.positions[0]["stopLoss"], "9.80");
    assert_eq!(sim.lock().await.positions[0]["takeProfit"], "10.80");
    // A take profit removed on the exchange is restored from the record.
    sim.lock().await.positions[0]["takeProfit"] = json!("0");
    {
        let _lock = app.gate.lock().await;
        app.tick().await.unwrap();
    }
    assert_eq!(sim.lock().await.positions[0]["takeProfit"], "10.80");
    // Past one stop distance in favor, the stop trails the best price by half of it.
    sim.lock().await.price = 10.5;
    {
        let _lock = app.gate.lock().await;
        app.tick().await.unwrap();
    }
    assert_eq!(sim.lock().await.positions[0]["stopLoss"], "10.29");
    // Bybit fills the take profit while the bot is not looking.
    {
        let mut s = sim.lock().await;
        s.price = 10.8;
        s.positions.clear();
        s.external = vec![
            json!({"side":"Sell","closedSize":"25.0","execType":"Trade","execPrice":"10.8","execFee":"0.1","orderLinkId":""}),
        ];
    }
    {
        let _lock = app.gate.lock().await;
        app.tick().await.unwrap();
    }
    assert!(app.store.open_trades().unwrap().is_empty());
    let closed = app.store.trades().unwrap().remove(0);
    assert_eq!(
        closed.exit_reason.as_deref(),
        Some("take_profit_on_exchange")
    );
    assert!(closed.profit_abs > 19.0);
    let history = app.store.stop_history(50).unwrap();
    let sources: Vec<_> = history
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["source"].as_str().unwrap())
        .collect();
    for s in ["open", "ai", "trail"] {
        assert!(sources.contains(&s), "{:?}", sources);
    }
    server.abort();
    std::env::remove_var("BYBIT_BASE_URL");
    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn ai_allocates_margin_and_leverage_within_the_limits() {
    let _env = EXCHANGE.lock().await;
    let sim = Arc::new(Mutex::new(Sim {
        futures: true,
        price: 10.0,
        ..Sim::default()
    }));
    let cfg = model::Config {
        market: "futures".into(),
        pairs: vec!["BTC/USDT:USDT".into()],
        // Enough slots that the per-entry minimum (1000 / slots) stays out of the way until
        // the end of the test.
        max_open: 10_000,
        leverage: 5,
        short: true,
        ..model::Config::default()
    };
    let (app, dir, server) = sim_app(&sim, cfg).await;
    // 75 USDT of margin at 2x is 150 notional: 15 contracts at 10.
    let mut action = open_long("BTC/USDT:USDT");
    action["margin_usdt"] = json!(75);
    action["leverage"] = json!(2);
    let result = app.execute_actions(&[action], false).await.unwrap();
    assert_eq!(result[0]["status"], "executed", "{}", result[0]);
    let t = app.store.open_trades().unwrap().remove(0);
    assert_eq!(t.leverage, 2);
    assert!((t.amount - 15.0).abs() < 1e-9, "{}", t.amount);
    {
        let _lock = app.gate.lock().await;
        app.close_position(t.trade_id, "force_exit").await.unwrap();
    }
    // Asking for more than the limits allows is lowered to them, not refused: margin to 95%
    // of the 1000 free, leverage to the configured 5x. 950 × 5 at 10 is 475 contracts.
    let mut action = open_long("BTC/USDT:USDT");
    action["margin_usdt"] = json!(5000);
    action["leverage"] = json!(50);
    let result = app.execute_actions(&[action], false).await.unwrap();
    assert_eq!(result[0]["status"], "executed", "{}", result[0]);
    let t = app.store.open_trades().unwrap().remove(0);
    assert_eq!(t.leverage, 5);
    assert!((t.amount - 475.0).abs() < 1e-9, "{}", t.amount);
    {
        let _lock = app.gate.lock().await;
        app.close_position(t.trade_id, "force_exit").await.unwrap();
    }
    // Without a margin the system sizes by its forecast: the two earlier 60% forecasts both
    // lost, so 60% calibrates to (0 + 10 × 0.6) / 12 = 50%; at 2R that is still worth +0.47R,
    // half Kelly is about 12% of equity and the 5% cap applies: 50 lost at a 4% stop is a
    // 1250 position, 125 contracts at 10.
    let result = app
        .execute_actions(&[open_long("BTC/USDT:USDT")], false)
        .await
        .unwrap();
    assert_eq!(result[0]["status"], "executed", "{}", result[0]);
    let sizing = &result[0]["sizing"];
    assert_eq!(sizing["margin_source"], "kelly", "{sizing}");
    assert!((sizing["edge"]["calibrated_win_prob"].as_f64().unwrap() - 0.5).abs() < 1e-9);
    assert!((sizing["kelly_risk_pct"].as_f64().unwrap() - 5.0).abs() < 1e-9);
    let t = app.store.open_trades().unwrap().remove(0);
    assert!((t.amount - 125.0).abs() < 1e-9, "{}", t.amount);
    assert_eq!(t.win_prob, Some(0.6));
    assert!((t.planned_rr.unwrap() - 2.0).abs() < 1e-9);
    {
        let _lock = app.gate.lock().await;
        app.close_position(t.trade_id, "force_exit").await.unwrap();
    }
    // The operator's minimum leverage raises a lower request: 2x becomes 3x.
    let mut config = app.store.config().unwrap();
    config.min_leverage = 3;
    app.store.save_config(&config).unwrap();
    let mut action = open_long("BTC/USDT:USDT");
    action["margin_usdt"] = json!(10);
    action["leverage"] = json!(2);
    let result = app.execute_actions(&[action], false).await.unwrap();
    assert_eq!(result[0]["status"], "executed", "{}", result[0]);
    let t = app.store.open_trades().unwrap().remove(0);
    assert_eq!(t.leverage, 3);
    assert!((t.amount - 3.0).abs() < 1e-9, "{}", t.amount);
    assert_eq!(
        app.snapshot().await.unwrap()["trade_limits"]["min_leverage"],
        3
    );
    {
        let _lock = app.gate.lock().await;
        app.close_position(t.trade_id, "force_exit").await.unwrap();
    }
    config.min_leverage = 1;
    app.store.save_config(&config).unwrap();
    // With 4 slots every AI entry commits at least 1000 / 4 = 250: a 10 USDT request is raised
    // to it, 250 at 5x is 125 contracts at 10.
    config.max_open = 4;
    app.store.save_config(&config).unwrap();
    assert_eq!(
        app.snapshot().await.unwrap()["trade_limits"]["min_margin_per_entry_usdt"],
        250.0
    );
    let mut action = open_long("BTC/USDT:USDT");
    action["margin_usdt"] = json!(10);
    action["leverage"] = json!(5);
    let result = app.execute_actions(&[action], false).await.unwrap();
    assert_eq!(result[0]["status"], "executed", "{}", result[0]);
    let sizing = &result[0]["sizing"];
    assert_eq!(sizing["margin_source"], "floor", "{sizing}");
    assert_eq!(sizing["requested_margin_usdt"], 10.0);
    assert_eq!(sizing["min_margin_usdt"], 250.0);
    let t = app.store.open_trades().unwrap().remove(0);
    assert!((t.amount - 125.0).abs() < 1e-9, "{}", t.amount);
    {
        let _lock = app.gate.lock().await;
        app.close_position(t.trade_id, "force_exit").await.unwrap();
    }
    config.max_open = 10_000;
    app.store.save_config(&config).unwrap();
    // Below the exchange minimum (5 USDT of position) is refused with the margin it needs.
    let mut action = open_long("BTC/USDT:USDT");
    action["margin_usdt"] = json!(0.5);
    action["leverage"] = json!(5);
    let result = app.execute_actions(&[action], false).await.unwrap();
    assert_eq!(result[0]["status"], "failed", "{}", result[0]);
    assert!(
        result[0]["note"].as_str().unwrap().contains("最小下单额"),
        "{}",
        result[0]
    );
    // Nonsense values are rejected before any order is sent.
    let orders = sim.lock().await.count;
    for bad in [
        json!({"leverage":2.5}),
        json!({"leverage":0}),
        json!({"margin_usdt":-1}),
        json!({"margin_usdt":0}),
        json!({"margin_usdt":"1"}),
    ] {
        let mut action = open_long("BTC/USDT:USDT");
        for (k, v) in bad.as_object().unwrap() {
            action[k] = v.clone();
        }
        let result = app.execute_actions(&[action], false).await.unwrap();
        assert_eq!(result[0]["status"], "failed", "{}", result[0]);
    }
    assert_eq!(sim.lock().await.count, orders);
    server.abort();
    std::env::remove_var("BYBIT_BASE_URL");
    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn spot_stop_rests_on_the_exchange_and_is_cancelled_before_a_sale() {
    let _env = EXCHANGE.lock().await;
    let sim = Arc::new(Mutex::new(Sim {
        price: 10.0,
        ..Sim::default()
    }));
    let mut cfg = model::Config {
        pairs: vec!["BTC/USDT".into()],
        ..model::Config::default()
    };
    // The simulated wallet never changes, so holdings would look like a drawdown once sold.
    cfg.ai.max_drawdown_pct = 0.0;
    let (app, dir, server) = sim_app(&sim, cfg).await;
    let result = app
        .execute_actions(&[open_long("BTC/USDT")], false)
        .await
        .unwrap();
    assert_eq!(result[0]["status"], "executed", "{}", result[0]);
    let tick = || async {
        let _lock = app.gate.lock().await;
        app.tick().await.unwrap();
    };
    let t = app.store.open_trades().unwrap().remove(0);
    tick().await;
    let stop_order = |s: &Sim| {
        s.orders
            .iter()
            .find(|(k, _)| k.starts_with('s'))
            .map(|(k, o)| (k.clone(), o.clone()))
            .unwrap()
    };
    {
        let s = sim.lock().await;
        let (_, o) = stop_order(&s);
        assert_eq!(o["triggerPrice"], "9.60");
        assert_eq!(o["orderFilter"], "StopOrder");
        assert_eq!(o["side"], "Sell");
        assert_eq!(o["orderStatus"], "Untriggered");
    }
    // A tighter stop is amended in place instead of creating a second order.
    app.set_stops(t.trade_id, Some(9.8), None).await.unwrap();
    tick().await;
    {
        let s = sim.lock().await;
        assert_eq!(stop_order(&s).1["triggerPrice"], "9.80");
        assert_eq!(s.orders.keys().filter(|k| k.starts_with('s')).count(), 1);
    }
    // If the saved link is lost, the resting order is adopted rather than duplicated.
    app.store
        .set_state(&format!("spot_stop:{}", t.trade_id), "")
        .unwrap();
    tick().await;
    assert_eq!(
        sim.lock()
            .await
            .orders
            .keys()
            .filter(|k| k.starts_with('s'))
            .count(),
        1
    );
    // The exchange fires the stop: the trade is settled without a second sale.
    {
        let mut s = sim.lock().await;
        s.price = 9.7;
        let (link, _) = stop_order(&s);
        let o = s.orders.get_mut(&link).unwrap();
        o["orderStatus"] = json!("Filled");
        o["cumExecQty"] = o["qty"].clone();
        o["avgPrice"] = json!("9.8");
    }
    tick().await;
    assert!(app.store.open_trades().unwrap().is_empty());
    let closed = app.store.trades().unwrap().remove(0);
    assert_eq!(closed.exit_reason.as_deref(), Some("stop_loss_on_exchange"));
    assert!(closed.profit_abs < 0.0);
    assert_eq!(sim.lock().await.count, 2);
    // Selling by hand cancels the resting stop first.
    sim.lock().await.price = 10.0;
    let result = app
        .execute_actions(&[open_long("BTC/USDT")], false)
        .await
        .unwrap();
    assert_eq!(result[0]["status"], "executed", "{}", result[0]);
    tick().await;
    let id = app.store.open_trades().unwrap()[0].trade_id;
    {
        let _lock = app.gate.lock().await;
        app.close_position(id, "force_exit").await.unwrap();
    }
    assert!(app.store.open_trades().unwrap().is_empty());
    let s = sim.lock().await;
    let paths: Vec<_> = s.calls.iter().map(|c| c.0.as_str()).collect();
    let cancel = paths
        .iter()
        .rposition(|p| *p == "/v5/order/cancel")
        .unwrap();
    let sell = paths
        .iter()
        .rposition(|p| *p == "/v5/order/create")
        .unwrap();
    assert!(cancel < sell);
    drop(s);
    server.abort();
    std::env::remove_var("BYBIT_BASE_URL");
    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn old_decisions_and_events_are_pruned_but_trades_and_recent_records_stay() {
    let dir = std::env::temp_dir().join(format!("tide-prune-{}", uuid::Uuid::new_v4()));
    let store = store::Store::open(&dir).unwrap();
    store
        .decision(&json!({"time":"2000-01-01 00:00:00","source":"auto","actions":[]}))
        .unwrap();
    store
        .decision(&json!({"time":model::now(),"source":"auto","actions":[]}))
        .unwrap();
    store.log("INFO", "recent");
    drop(store);
    let db = rusqlite::Connection::open(dir.join("tide.sqlite")).unwrap();
    db.execute(
        "INSERT INTO events(time,level,message) VALUES ('2000-01-01 00:00:00','INFO','old')",
        [],
    )
    .unwrap();
    drop(db);
    // Records past their retention go when the store opens.
    let store = store::Store::open(&dir).unwrap();
    let decisions = store.decisions(10).unwrap();
    assert_eq!(decisions.len(), 1);
    assert_ne!(decisions[0]["time"], "2000-01-01 00:00:00");
    let logs = store.logs(10).unwrap()["logs"].to_string();
    assert!(logs.contains("recent") && !logs.contains("old"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn pairs_are_fetched_together_and_keep_their_order() {
    let _env = EXCHANGE.lock().await;
    let sim = Arc::new(Mutex::new(Sim {
        futures: true,
        price: 10.0,
        ..Sim::default()
    }));
    let cfg = model::Config {
        market: "futures".into(),
        pairs: vec![
            "ETH/USDT:USDT".into(),
            "BTC/USDT:USDT".into(),
            "SOL/USDT:USDT".into(),
        ],
        leverage: 5,
        ..model::Config::default()
    };
    let (app, dir, server) = sim_app(&sim, cfg).await;
    let snap = app.snapshot().await.unwrap();
    let names: Vec<_> = snap["pairs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["pair"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["ETH/USDT:USDT", "BTC/USDT:USDT", "SOL/USDT:USDT"]);
    assert!(snap["pairs"]
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["market_stats"]["last_price"] == 10.0));
    server.abort();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn model_use_is_summed_per_day_across_provider_shapes() {
    let dir = std::env::temp_dir().join(format!("tide-usage-{}", uuid::Uuid::new_v4()));
    let store = store::Store::open(&dir).unwrap();
    let today = model::now();
    store.decision(&json!({"time":today,"source":"auto","usage":{"input_tokens":100,"cache_read_input_tokens":900,"cache_creation_input_tokens":50,"output_tokens":30}})).unwrap();
    store.decision(&json!({"time":today,"source":"hook","usage":{"prompt_tokens":400,"prompt_tokens_details":{"cached_tokens":300},"completion_tokens":20}})).unwrap();
    store
        .decision(&json!({"time":today,"source":"auto","error":"boom"}))
        .unwrap();
    let days = store.usage_by_day(7).unwrap();
    let d = &days[0];
    assert_eq!(d["date"], &today[..10]);
    assert_eq!(
        (d["calls"].as_u64(), d["failed"].as_u64()),
        (Some(3), Some(1))
    );
    assert_eq!(d["input"], 1050 + 400);
    assert_eq!(d["cache_read"], 900 + 300);
    assert_eq!(
        (d["cache_write"].as_u64(), d["output"].as_u64()),
        (Some(50), Some(50))
    );
    assert_eq!(d["by_source"]["auto"], 2);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_lower_position_cap_carries_over_to_the_saved_config() {
    let dir = std::env::temp_dir().join(format!("tide-cap-{}", uuid::Uuid::new_v4()));
    let store = store::Store::open(&dir).unwrap();
    // A configuration saved under the old cap of 5.
    let mut c = store.config().unwrap();
    c.max_open = 5;
    store.save_config(&c).unwrap();
    store.set_state("max_open_cap", "5").unwrap();
    drop(store);
    let store = store::Store::open(&dir).unwrap();
    assert_eq!(store.config().unwrap().max_open, model::MAX_OPEN);
    assert_eq!(model::MAX_OPEN, 4);
    // A lower saved limit is kept, and the step runs once per cap.
    let mut c = store.config().unwrap();
    c.max_open = 2;
    store.save_config(&c).unwrap();
    drop(store);
    let store = store::Store::open(&dir).unwrap();
    assert_eq!(store.config().unwrap().max_open, 2);
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn schema_is_versioned_and_open_trades_are_filtered_in_sql() {
    let dir = std::env::temp_dir().join(format!("tide-schema-{}", uuid::Uuid::new_v4()));
    let store = store::Store::open(&dir).unwrap();
    drop(store);
    // Reopening applies no step twice.
    let store = store::Store::open(&dir).unwrap();
    store.log_stop(1, "BTC/USDT", 9.0, 11.0, "ai");
    assert_eq!(store.stop_history(10).unwrap().as_array().unwrap().len(), 1);
    assert!(store.open_trades().unwrap().is_empty());
    assert_eq!(store.realized_on("2000-01-01").unwrap(), 0.0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn legacy_ai_config_becomes_one_channel() {
    let mut a: model::AiConfig = serde_json::from_value(
        json!({"provider":"openai","model":"m","api_key":"k","base_url":"https://x.example"}),
    )
    .unwrap();
    assert!(a.channels.is_empty());
    a.normalize();
    assert_eq!(a.channels.len(), 1);
    assert_eq!(a.active_channel, "default");
    let ch = a.active().unwrap();
    assert_eq!(
        (ch.provider.as_str(), ch.model.as_str(), ch.api_key.as_str()),
        ("openai", "m", "k")
    );
    // Normalizing again changes nothing, and a stale active id falls back to the first channel.
    let before = serde_json::to_string(&a).unwrap();
    a.normalize();
    assert_eq!(before, serde_json::to_string(&a).unwrap());
    a.active_channel = "gone".into();
    a.normalize();
    assert_eq!(a.active_channel, "default");
}

#[test]
fn existing_config_moves_to_ai_managed_limits_once() {
    let dir = std::env::temp_dir().join(format!("tide-limits-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.json"),
        json!({"market":"futures","max_open":3,"risk_per_trade_pct":1.0,"max_same_direction":2,"short":false,"leverage":10}).to_string(),
    )
    .unwrap();
    let store = store::Store::open(&dir).unwrap();
    let c = store.config().unwrap();
    // The retired per-trade risk setting in old files is ignored, not an error.
    assert_eq!(c.max_open, model::MAX_OPEN);
    assert_eq!(c.max_same_direction, 0);
    assert!(c.short);
    assert_eq!(c.leverage, 10);
    // The limits are applied once; later changes made in code or by hand are left alone.
    let mut c = c;
    c.max_open = 2;
    store.save_config(&c).unwrap();
    drop(store);
    let store = store::Store::open(&dir).unwrap();
    assert_eq!(store.config().unwrap().max_open, 2);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn the_reviewer_can_only_change_its_own_notes() {
    let dir = std::env::temp_dir().join(format!("tide-review-{}", uuid::Uuid::new_v4()));
    let store = store::Store::open(&dir).unwrap();
    let app = App {
        store,
        http: reqwest::Client::new(),
        auth: auth::Auth::create("admin".into(), "password").unwrap(),
        secret: "01234567890123456789012345678901".into(),
        origin: "https://trade.example.com".into(),
        gate: Mutex::new(()),
        ai_gate: Mutex::new(()),
        ai_state: Mutex::new(json!({})),
        login_attempts: Mutex::new(vec![]),
        hooks: Default::default(),
    };
    let seeded = app.store.knowledge().unwrap();
    let seed_id = seeded[0]["id"].as_i64().unwrap();
    assert!(seeded.iter().all(|n| n["origin"] == "seed"));
    let out = app
        .apply_review_changes(&[json!({"op":"add","title":"AI 平仓多数亏损","content":"14 笔主动平仓合计亏损，样本 14。"})])
        .unwrap();
    assert_eq!(out[0]["status"], "applied", "{}", out[0]);
    let mine = out[0]["id"].as_i64().unwrap();
    let note = |id: i64| {
        app.store
            .knowledge()
            .unwrap()
            .into_iter()
            .find(|n| n["id"] == id)
    };
    assert_eq!(note(mine).unwrap()["origin"], "ai");
    // The user's switch survives an update, and fields left out keep their values.
    app.store
        .knowledge_update(
            mine,
            "AI 平仓多数亏损",
            "14 笔主动平仓合计亏损，样本 14。",
            "AI 复盘",
            false,
        )
        .unwrap();
    let out = app
        .apply_review_changes(&[json!({"op":"update","id":mine,"content":"更新后的内容"})])
        .unwrap();
    assert_eq!(out[0]["status"], "applied", "{}", out[0]);
    let n = note(mine).unwrap();
    assert_eq!(
        (
            n["title"].as_str(),
            n["content"].as_str(),
            n["enabled"].as_bool()
        ),
        (Some("AI 平仓多数亏损"), Some("更新后的内容"), Some(false))
    );
    // Built-in and hand-written notes are off limits; so are bad ids and unknown operations.
    for bad in [
        json!({"op":"update","id":seed_id,"content":"x"}),
        json!({"op":"delete","id":seed_id}),
        json!({"op":"delete","id":99999}),
        json!({"op":"update","content":"x"}),
        json!({"op":"add","title":"","content":"x"}),
        json!({"op":"rewrite","id":mine}),
    ] {
        let out = app.apply_review_changes(&[bad]).unwrap();
        assert_eq!(out[0]["status"], "skipped", "{}", out[0]);
    }
    assert!(note(seed_id).is_some());
    // At most 12 of its own notes; the 13th is skipped.
    for i in 0..11 {
        let out = app
            .apply_review_changes(&[json!({"op":"add","title":format!("n{}", i),"content":"c"})])
            .unwrap();
        assert_eq!(out[0]["status"], "applied", "{} {}", i, out[0]);
    }
    let out = app
        .apply_review_changes(&[json!({"op":"add","title":"one too many","content":"c"})])
        .unwrap();
    assert_eq!(out[0]["status"], "skipped");
    let out = app
        .apply_review_changes(&[json!({"op":"delete","id":mine})])
        .unwrap();
    assert_eq!(out[0]["status"], "applied");
    assert!(note(mine).is_none());
    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_review_reads_closed_trades_and_writes_notes() {
    let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
    async fn model(
        State(seen): State<Arc<Mutex<Vec<Value>>>>,
        body: axum::Json<Value>,
    ) -> axum::Json<Value> {
        seen.lock().await.push(body.0);
        let reply = json!({"summary":"3 笔样本，主动平仓亏损","changes":[
            {"op":"add","title":"主动平仓亏损","content":"样本 3：AI 主动平仓全部亏损，优先让止损单管理出场。"},
            {"op":"delete","id":1}
        ]});
        axum::Json(
            json!({"choices":[{"finish_reason":"stop","message":{"content":reply.to_string()}}],"usage":{"completion_tokens":9}}),
        )
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/v1/chat/completions", axum::routing::post(model))
                .with_state(seen.clone()),
        )
        .into_future(),
    );
    let dir = std::env::temp_dir().join(format!("tide-review-e2e-{}", uuid::Uuid::new_v4()));
    let store = store::Store::open(&dir).unwrap();
    let mut cfg = model::Config::default();
    cfg.ai.provider = "openai".into();
    cfg.ai.model = "review-model".into();
    cfg.ai.api_key = "k".into();
    cfg.ai.base_url = format!("http://{}/v1", addr);
    store.save_config(&cfg).unwrap();
    for (i, profit) in [-1.0, -2.0, -0.5].into_iter().enumerate() {
        let t = model::Trade {
            trade_id: 0,
            pair: "XRP/USDT:USDT".into(),
            market: "futures".into(),
            is_short: false,
            leverage: 5,
            amount: 1.0,
            open_rate: 1.0,
            stake_amount: 1.0,
            open_date: format!("2026-11-01 0{}:00:00", i),
            close_rate: Some(1.0),
            close_date: Some(format!("2026-11-01 0{}:30:00", i)),
            profit_abs: profit,
            profit_ratio: profit,
            stop_loss_abs: 0.95,
            take_profit_abs: 0.0,
            peak: 1.0,
            enter_tag: "ai".into(),
            exit_reason: Some("force_exit".into()),
            entry_fee: 0.0,
            stop_dist: 0.05,
            entry_reason: "突破".into(),
            funding: 0.0,
            win_prob: None,
            planned_rr: None,
        };
        store.insert_trade(t, "").unwrap();
    }
    let app = App {
        store,
        http: reqwest::Client::new(),
        auth: auth::Auth::create("admin".into(), "password").unwrap(),
        secret: "01234567890123456789012345678901".into(),
        origin: "https://trade.example.com".into(),
        gate: Mutex::new(()),
        ai_gate: Mutex::new(()),
        ai_state: Mutex::new(json!({})),
        login_attempts: Mutex::new(vec![]),
        hooks: Default::default(),
    };
    let rec = app.review("manual").await.unwrap();
    assert!(rec["error"].is_null(), "{}", rec);
    assert_eq!(rec["changes"][0]["status"], "applied");
    // id 1 is a built-in note, which the reviewer may not delete.
    assert_eq!(rec["changes"][1]["status"], "skipped");
    let notes = app.store.knowledge().unwrap();
    assert!(notes
        .iter()
        .any(|n| n["origin"] == "ai" && n["title"] == "主动平仓亏损"));
    assert!(notes.iter().any(|n| n["id"] == 1));
    let request = seen.lock().await[0].clone();
    assert_eq!(request["model"], "review-model");
    assert!(request["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("post-trade reviewer"));
    let user = request["messages"][1]["content"].as_str().unwrap();
    assert!(user.contains("profit_factor") && user.contains("force_exit") && user.contains("突破"));
    // The review is recorded next to the trading decisions, and a trading run never sees it.
    let log = app.store.decisions(5).unwrap();
    assert_eq!(log[0]["source"], "review");
    assert_eq!(log[0]["analysis"], "3 笔样本，主动平仓亏损");
    server.abort();
    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn open_trades_raise_hook_events_as_they_move() {
    let _env = EXCHANGE.lock().await;
    let sim = Arc::new(Mutex::new(Sim {
        futures: true,
        price: 10.0,
        ..Sim::default()
    }));
    let mut cfg = model::Config {
        market: "futures".into(),
        pairs: vec!["BTC/USDT:USDT".into()],
        // 1000 over 20 slots: a 50 USDT entry sits exactly at the per-entry minimum.
        max_open: 20,
        leverage: 5,
        short: true,
        ..model::Config::default()
    };
    cfg.ai.hooks.big_move_pct = 0.0;
    let (app, dir, server) = sim_app(&sim, cfg).await;
    // 50 USDT at 5x: the 25 contracts the simulated exchange fill below closes.
    let mut action = open_long("BTC/USDT:USDT");
    action["margin_usdt"] = json!(50);
    let result = app.execute_actions(&[action], false).await.unwrap();
    assert_eq!(result[0]["status"], "executed", "{}", result[0]);
    let tick = || async {
        let _lock = app.gate.lock().await;
        app.tick().await.unwrap();
    };
    let types = || -> Vec<String> {
        app.hooks
            .lock()
            .unwrap()
            .pending
            .iter()
            .map(|e| e["type"].as_str().unwrap().to_string())
            .collect()
    };
    tick().await;
    assert!(types().is_empty());
    // A 4% stop: a 3% loss is past 60% of the way, and it is reported once.
    sim.lock().await.price = 9.7;
    tick().await;
    tick().await;
    assert_eq!(types(), ["adverse_move"]);
    // Up one stop distance: the trailing level.
    sim.lock().await.price = 10.5;
    tick().await;
    assert_eq!(types(), ["adverse_move", "profit_milestone"]);
    // The exchange fills the take profit: that wakes the AI too.
    {
        let mut s = sim.lock().await;
        s.price = 10.8;
        s.positions.clear();
        s.external = vec![
            json!({"side":"Sell","closedSize":"25.0","execType":"Trade","execPrice":"10.8","execFee":"0.1","orderLinkId":""}),
        ];
    }
    tick().await;
    assert_eq!(
        types(),
        ["adverse_move", "profit_milestone", "position_closed"]
    );
    let closed = app.hooks.lock().unwrap().pending[2].clone();
    assert_eq!(closed["reason"], "take_profit_on_exchange");
    // Hooks switched off queue nothing.
    let mut c = app.store.config().unwrap();
    c.ai.hooks.enabled = false;
    app.store.save_config(&c).unwrap();
    app.hooks.lock().unwrap().pending.clear();
    app.push_hook(json!({"type":"big_move","pair":"BTC/USDT"}));
    assert!(types().is_empty());
    server.abort();
    std::env::remove_var("BYBIT_BASE_URL");
    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn queued_events_start_one_run_within_the_limits() {
    let _env = EXCHANGE.lock().await;
    let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
    async fn model(
        State(seen): State<Arc<Mutex<Vec<Value>>>>,
        body: axum::Json<Value>,
    ) -> axum::Json<Value> {
        seen.lock().await.push(body.0);
        let reply = json!({"analysis":"事件已处理，观望","memory":"","actions":[]});
        axum::Json(
            json!({"choices":[{"finish_reason":"stop","message":{"content":reply.to_string()}}],"usage":{"completion_tokens":9}}),
        )
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let model_server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/v1/chat/completions", axum::routing::post(model))
                .with_state(seen.clone()),
        )
        .into_future(),
    );
    let sim = Arc::new(Mutex::new(Sim {
        price: 10.0,
        ..Sim::default()
    }));
    let mut cfg = model::Config {
        pairs: vec!["BTC/USDT".into()],
        ..model::Config::default()
    };
    cfg.ai.provider = "openai".into();
    cfg.ai.model = "m".into();
    cfg.ai.api_key = "k".into();
    cfg.ai.base_url = format!("http://{}/v1", addr);
    let (app, dir, server) = sim_app(&sim, cfg).await;
    // Nothing queued: nothing runs.
    assert!(app.dispatch_hooks().await.unwrap().is_none());
    app.push_hook(json!({"type":"big_move","pair":"BTC/USDT","move_pct":-3.4,"over_minutes":55}));
    let rec = app.dispatch_hooks().await.unwrap().unwrap();
    assert!(rec["error"].is_null(), "{}", rec);
    assert_eq!(rec["source"], "hook");
    assert_eq!(rec["trigger"]["events"][0]["type"], "big_move");
    assert!(app.hooks.lock().unwrap().pending.is_empty());
    // The model is told why it was started.
    let request = seen.lock().await[0].clone();
    let user = request["messages"][1]["content"].as_str().unwrap();
    assert!(
        user.contains("\"trigger\":{") && user.contains("big_move"),
        "{}",
        user
    );
    // Too soon after the last run: the event waits instead of starting another run.
    app.push_hook(json!({"type":"profit_milestone","pair":"BTC/USDT"}));
    assert!(app.dispatch_hooks().await.unwrap().is_none());
    assert_eq!(app.hooks.lock().unwrap().pending.len(), 1);
    // A scheduled run covers whatever was queued, because its snapshot is fresh.
    let rec = app.run_ai(true, "auto").await.unwrap();
    assert_eq!(rec["source"], "auto");
    assert!(app.hooks.lock().unwrap().pending.is_empty());
    let last = seen.lock().await.last().unwrap().clone();
    assert!(last["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("\"type\":\"scheduled\""));
    model_server.abort();
    server.abort();
    std::env::remove_var("BYBIT_BASE_URL");
    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn withdrawals_do_not_trip_the_drawdown_breaker_but_trading_losses_do() {
    let _env = EXCHANGE.lock().await;
    let sim = Arc::new(Mutex::new(Sim {
        futures: true,
        price: 10.0,
        ..Sim::default()
    }));
    let cfg = model::Config {
        market: "futures".into(),
        pairs: vec!["BTC/USDT:USDT".into()],
        leverage: 5,
        short: true,
        ..model::Config::default()
    };
    let (app, dir, server) = sim_app(&sim, cfg).await;
    // 50 USDT at 5x: 25 contracts long at 10, stop at 9.6.
    let mut action = open_long("BTC/USDT:USDT");
    action["margin_usdt"] = json!(50);
    let result = app.execute_actions(&[action], false).await.unwrap();
    assert_eq!(result[0]["status"], "executed", "{}", result[0]);
    // The watchdog checks once a minute; clear its timer so each tick checks.
    let watch = || async {
        app.store.set_state("watchdog_at", "0").unwrap();
        let _lock = app.gate.lock().await;
        app.tick().await.unwrap();
    };
    watch().await;
    assert!(app.store.config().unwrap().running);
    // The operator moves 980 of the 1000 USDT out. The wallet falls 98%, Tide's trading PnL
    // does not, so the bot keeps running (the old equity-based check paused it here).
    sim.lock().await.wallet = Some(20.0);
    watch().await;
    assert!(app.store.config().unwrap().running);
    // A real loss counts against what is left: 25 × 0.3 = 7.5 lost of about 27.5 is past 20%.
    sim.lock().await.price = 9.7;
    watch().await;
    assert!(!app.store.config().unwrap().running);
    let logs = app.store.logs(5).unwrap().to_string();
    assert!(
        logs.contains("交易回撤") && logs.contains("不含出入金"),
        "{logs}"
    );
    server.abort();
    std::env::remove_var("BYBIT_BASE_URL");
    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn ai_adds_to_an_open_position_at_its_leverage_and_stop() {
    let _env = EXCHANGE.lock().await;
    let sim = Arc::new(Mutex::new(Sim {
        futures: true,
        price: 10.0,
        ..Sim::default()
    }));
    let cfg = model::Config {
        market: "futures".into(),
        pairs: vec!["BTC/USDT:USDT".into()],
        // 1000 over 20 slots: a 50 USDT entry sits exactly at the per-entry minimum.
        max_open: 20,
        leverage: 5,
        short: true,
        ..model::Config::default()
    };
    let (app, dir, server) = sim_app(&sim, cfg).await;
    // 50 USDT at 5x: 25 contracts long at 10, stop 9.6, take profit 10.8.
    let mut action = open_long("BTC/USDT:USDT");
    action["margin_usdt"] = json!(50);
    let result = app.execute_actions(&[action], false).await.unwrap();
    assert_eq!(result[0]["status"], "executed", "{}", result[0]);
    // Price runs to 10.5 and the AI adds 50 USDT more: 250 / 10.5 rounds down to 23.8.
    sim.lock().await.price = 10.5;
    let add = json!({"action":"add","pair":"BTC/USDT:USDT","confidence":0.9,"margin_usdt":50,"reason":"突破后放量延续"});
    let result = app
        .execute_actions(std::slice::from_ref(&add), false)
        .await
        .unwrap();
    assert_eq!(result[0]["status"], "executed", "{}", result[0]);
    let held = app.store.open_trades().unwrap();
    assert_eq!(
        held.len(),
        1,
        "an add grows the position, it does not open another"
    );
    let t = &held[0];
    assert!((t.amount - 48.8).abs() < 1e-9, "{}", t.amount);
    assert!((t.open_rate - (250.0 + 23.8 * 10.5) / 48.8).abs() < 1e-9);
    assert!((t.stake_amount - (50.0 + 23.8 * 10.5 / 5.0)).abs() < 1e-9);
    assert_eq!(t.leverage, 5);
    assert!((t.stop_loss_abs - 9.6).abs() < 1e-9 && (t.take_profit_abs - 10.8).abs() < 1e-9);
    assert!(t.entry_reason.contains("突破后回踩确认") && t.entry_reason.contains("突破后放量延续"));
    assert_eq!(sim.lock().await.positions.len(), 1);
    // The exchange and the record agree, so the next check finds nothing to repair.
    {
        let _lock = app.gate.lock().await;
        app.tick().await.unwrap();
    }
    assert_eq!(app.store.open_trades().unwrap()[0].amount, t.amount);
    // At 13, adding 500 more would put the average near 12.4: the 9.6 stop would then be
    // past 60% of the way to liquidation at 5x, so the add is refused before any order.
    sim.lock().await.price = 13.0;
    let orders = sim.lock().await.count;
    let mut big = add.clone();
    big["margin_usdt"] = json!(500);
    let result = app.execute_actions(&[big], false).await.unwrap();
    assert_eq!(result[0]["status"], "failed", "{}", result[0]);
    assert!(result[0]["note"].as_str().unwrap().contains("离强平太近"));
    // An add needs a margin and an existing position; neither reaches the exchange.
    let mut no_margin = add.clone();
    no_margin.as_object_mut().unwrap().remove("margin_usdt");
    let mut no_position = add.clone();
    no_position["pair"] = json!("ETH/USDT:USDT");
    for bad in [no_margin, no_position] {
        let result = app.execute_actions(&[bad], false).await.unwrap();
        assert_eq!(result[0]["status"], "failed", "{}", result[0]);
    }
    assert_eq!(sim.lock().await.count, orders);
    // The process stops after an add reached the exchange but before it was confirmed: the
    // next reconcile merges the fill into the position instead of opening or losing it.
    sim.lock().await.price = 10.5;
    let before = app.store.open_trades().unwrap().remove(0);
    let link = "tadd-recovered";
    let body = json!({"category":"linear","symbol":"BTCUSDT","side":"Buy","orderType":"Market","qty":"2.0","orderLinkId":link,"positionIdx":0});
    app.store
        .reserve(
            link,
            "add",
            &before.pair,
            &json!({"trade":before,"request":body,"reason":"恢复"}),
        )
        .unwrap();
    let config = app.store.config().unwrap();
    app.exchange(&config)
        .post("/v5/order/create", body)
        .await
        .unwrap();
    assert_eq!(app.store.unresolved().unwrap().len(), 1);
    app.reconcile(&config).await.unwrap();
    assert!(app.store.unresolved().unwrap().is_empty());
    let after = app.store.open_trades().unwrap();
    assert_eq!(after.len(), 1);
    assert!(
        (after[0].amount - (before.amount + 2.0)).abs() < 1e-9,
        "{}",
        after[0].amount
    );
    {
        let _lock = app.gate.lock().await;
        app.tick().await.unwrap();
    }
    server.abort();
    std::env::remove_var("BYBIT_BASE_URL");
    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn ai_entries_need_a_forecast_with_a_positive_calibrated_edge() {
    let _env = EXCHANGE.lock().await;
    let sim = Arc::new(Mutex::new(Sim {
        futures: true,
        price: 10.0,
        ..Sim::default()
    }));
    let cfg = model::Config {
        market: "futures".into(),
        pairs: vec!["BTC/USDT:USDT".into()],
        leverage: 5,
        short: true,
        ..model::Config::default()
    };
    let (app, dir, server) = sim_app(&sim, cfg).await;
    // A 30% forecast at 2R is worth 0.3 × 2 − 0.7 − fees: negative, so nothing is sent.
    let mut weak = open_long("BTC/USDT:USDT");
    weak["win_prob"] = json!(0.3);
    let mut silent = open_long("BTC/USDT:USDT");
    silent.as_object_mut().unwrap().remove("win_prob");
    let mut nonsense = open_long("BTC/USDT:USDT");
    nonsense["win_prob"] = json!(1.5);
    for (bad, why) in [
        (weak, "不为正"),
        (silent, "win_prob"),
        (nonsense, "win_prob"),
    ] {
        let result = app.execute_actions(&[bad], false).await.unwrap();
        assert_eq!(result[0]["status"], "failed", "{}", result[0]);
        assert!(
            result[0]["note"].as_str().unwrap().contains(why),
            "{}",
            result[0]
        );
    }
    assert_eq!(sim.lock().await.count, 0);
    // The operator's own order through the API carries no forecast and is not gated by one.
    let manual =
        json!({"action":"open_long","pair":"BTC/USDT:USDT","stop_loss_pct":4.0,"margin_usdt":20});
    let result = app.execute_actions(&[manual], true).await.unwrap();
    assert_eq!(result[0]["status"], "executed", "{}", result[0]);
    assert_eq!(result[0]["sizing"]["margin_source"], "ai");
    assert!(result[0]["sizing"]["edge"].is_null());
    assert_eq!(app.store.open_trades().unwrap()[0].win_prob, None);
    // The model is shown its calibration so far and how entries are sized from it.
    let snap = app.snapshot().await.unwrap();
    assert_eq!(snap["calibration"]["trades_with_forecast"], 0);
    assert_eq!(
        snap["calibration"]["by_forecast"].as_array().unwrap().len(),
        5
    );
    assert_eq!(snap["sizing_policy"]["kelly_fraction"], 0.5);
    assert_eq!(snap["sizing_policy"]["kelly_cap_pct"], 5.0);
    server.abort();
    std::env::remove_var("BYBIT_BASE_URL");
    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}
