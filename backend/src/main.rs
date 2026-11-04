mod ai;
mod api;
mod auth;
mod engine;
mod exchange;
mod indicators;
mod mcp;
mod migrate;
mod model;
mod store;
#[cfg(test)]
mod tests;

use anyhow::{ensure, Result};
use axum::{
    extract::DefaultBodyLimit,
    middleware,
    routing::{any, get},
    Router,
};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

pub struct App {
    pub store: store::Store,
    pub http: reqwest::Client,
    pub auth: auth::Auth,
    pub secret: String,
    pub origin: String,
    pub gate: Mutex<()>,
    pub ai_gate: Mutex<()>,
    pub ai_state: Mutex<Value>,
    pub login_attempts: Mutex<Vec<i64>>,
}
impl App {
    pub fn exchange(&self, c: &model::Config) -> exchange::Bybit {
        exchange::Bybit::new(self.http.clone(), c.exchange.clone())
    }
    pub async fn telegram(&self, c: &model::Config, text: &str) -> Result<()> {
        if c.telegram.enabled && !c.telegram.chat_id.is_empty() {
            self.tg(
                &c.telegram.token,
                "sendMessage",
                json!({"chat_id":c.telegram.chat_id,"text":text}),
            )
            .await?;
        }
        Ok(())
    }
    pub async fn tg(&self, token: &str, method: &str, data: Value) -> Result<Value> {
        let v: Value = self
            .http
            .post(format!("https://api.telegram.org/bot{}/{}", token, method))
            .json(&data)
            .send()
            .await?
            .json()
            .await?;
        ensure!(v["ok"] == true, "Telegram 请求失败");
        Ok(v["result"].clone())
    }
}
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tide=info,tower_http=info".into()),
        )
        .init();
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("mcp") {
        return mcp::run().await;
    }
    let data = PathBuf::from(std::env::var("TIDE_DATA_DIR").unwrap_or_else(|_| "data".into()));
    let store = store::Store::open(&data)?;
    if args.get(1).map(String::as_str) == Some("init") {
        ensure!(!data.join("auth.json").exists(), "已有登录配置，不覆盖");
        let password = std::env::var("TIDE_ADMIN_PASSWORD")?;
        let a = auth::Auth::create(
            std::env::var("TIDE_ADMIN_USER").unwrap_or_else(|_| "admin".into()),
            &password,
        )?;
        store::atomic_json(&data.join("auth.json"), &a)?;
        store.save_config(&model::Config::default())?;
        println!("初始化完成；交易默认暂停");
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("migrate") {
        return migrate::run(
            &store,
            &PathBuf::from(
                args.get(2)
                    .ok_or_else(|| anyhow::anyhow!("需要旧 user_data 目录"))?,
            ),
        );
    }
    let secret = std::env::var("PANEL_SESSION_SECRET")
        .map_err(|_| anyhow::anyhow!("缺少 PANEL_SESSION_SECRET"))?;
    ensure!(secret.len() >= 32, "会话密钥至少 32 字符");
    let origin = std::env::var("TIDE_PUBLIC_ORIGIN")?;
    let url = url::Url::parse(&origin)?;
    ensure!(
        url.scheme() == "https" && url.origin().ascii_serialization() == origin,
        "TIDE_PUBLIC_ORIGIN 必须是 HTTPS origin，不带路径"
    );
    let app = Arc::new(App {
        auth: auth::Auth::load(&data.join("auth.json"))?,
        store,
        http: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()?,
        secret,
        origin,
        gate: Mutex::new(()),
        ai_gate: Mutex::new(()),
        ai_state: Mutex::new(
            json!({"running":false,"last_run":null,"next_run":null,"last_error":null}),
        ),
        login_attempts: Mutex::new(vec![]),
    });
    let web = std::env::var("TIDE_WEB_DIR").unwrap_or_else(|_| "web/out".into());
    ensure!(
        PathBuf::from(&web).join("index.html").exists(),
        "缺少 Next.js 静态构建，请先构建前端"
    );
    let router = Router::new()
        .route(
            "/healthz",
            get(|| async {
                axum::Json(json!({"ok":true,"version":env!("CARGO_PKG_VERSION"),"engine":"rust"}))
            }),
        )
        .route("/api/{*path}", any(api::dispatch))
        .fallback_service(
            tower_http::services::ServeDir::new(web).append_index_html_on_directories(true),
        )
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), auth::protect))
        .layer(tower_http::compression::CompressionLayer::new())
        .with_state(app.clone());
    app.store.log("INFO", "Tide Rust 交易服务启动");
    tokio::spawn(engine::scheduler(app.clone()));
    tokio::spawn(ai::scheduler(app.clone()));
    let address = std::env::var("TIDE_LISTEN").unwrap_or_else(|_| "127.0.0.1:8000".into());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    tracing::info!(%address,"listening");
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            #[cfg(unix)]
            {
                let mut sig =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .unwrap();
                tokio::select! {_=sig.recv()=>{},_=tokio::signal::ctrl_c()=>{}}
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
        })
        .await?;
    Ok(())
}
