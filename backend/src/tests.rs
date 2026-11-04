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
        max_open: 5,
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
    cfg.market = "spot".into();
    cfg.leverage = 25;
    assert!(api::validate(&cfg).is_err());
}

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
    // The exchange accepts 5 USDT; concurrent holdings, not past entries, limit trading.
    cfg.capital = Some(6.0);
    cfg.max_open = 1;
    // 5% of 6 USDT at a 2.5% stop wants 12 USDT; the 95% budget cap limits it to 5.7.
    cfg.risk_per_trade_pct = 5.0;
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
    });
    let actions = json!([{"action":"open_long","pair":"BTC/USDT","confidence":0.9},{"action":"open_long","pair":"ETH/USDT","confidence":0.9}]);
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
    // Repeatedly reuse the same slot on the same day, including close + reopen in one batch.
    for _ in 0..3 {
        let result = app
            .execute_actions(&[actions[0].clone()], false)
            .await
            .unwrap();
        assert_eq!(result[0]["status"], "executed", "{}", result[0]);
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
    assert_eq!(mock.lock().await.count, 14);
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
    assert_eq!(mock.lock().await.count, 15);
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
    api_server.abort();
    server.abort();
    std::env::remove_var("BYBIT_BASE_URL");
    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}
