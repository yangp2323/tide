//! MCP stdio transport, sharing the server's browser login and risk-controlled API.
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

fn tools() -> Value {
    json!([
        {"name":"get_market_snapshot","description":"完整行情快照、账户、持仓和风控。分析和下单前先读取。","inputSchema":{"type":"object","properties":{}}},
        {"name":"get_status","description":"机器人和账户概况。","inputSchema":{"type":"object","properties":{}}},
        {"name":"get_trades","description":"已平仓交易。","inputSchema":{"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":200}}}},
        {"name":"get_ai_decisions","description":"AI 决策历史。","inputSchema":{"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":500}}}},
        {"name":"open_position","description":"实盘市价开仓，需要用户明确同意。金额、杠杆和风控由服务器执行。","inputSchema":{"type":"object","properties":{"pair":{"type":"string"},"side":{"type":"string","enum":["long","short"]},"reason":{"type":"string"}},"required":["pair","side","reason"]}},
        {"name":"close_position","description":"实盘市价平仓，需要用户明确同意。","inputSchema":{"type":"object","properties":{"pair":{"type":"string"},"reason":{"type":"string"}},"required":["pair","reason"]}},
        {"name":"run_server_ai","description":"立即调用模型，execute=true 将执行实盘订单，需要用户明确同意。","inputSchema":{"type":"object","properties":{"execute":{"type":"boolean","default":false}}}}
    ])
}
async fn call(
    http: &reqwest::Client,
    base: &str,
    cookie: &str,
    name: &str,
    args: &Value,
) -> Result<Value> {
    let (method, path, body) = match name {
        "get_market_snapshot" => ("GET", "/api/ai/snapshot".into(), None),
        "get_status" => ("GET", "/api/overview".into(), None),
        "get_trades" => (
            "GET",
            format!(
                "/api/trades?limit={}",
                args["limit"].as_u64().unwrap_or(20).clamp(1, 200)
            ),
            None,
        ),
        "get_ai_decisions" => (
            "GET",
            format!(
                "/api/ai/log?limit={}",
                args["limit"].as_u64().unwrap_or(10).clamp(1, 500)
            ),
            None,
        ),
        "open_position" => {
            let side = args["side"].as_str().context("缺少 side")?;
            ensure!(["long", "short"].contains(&side), "side 无效");
            (
                "POST",
                "/api/ai/order".into(),
                Some(
                    json!({"action":format!("open_{}",side),"pair":args["pair"],"reason":args["reason"]}),
                ),
            )
        }
        "close_position" => (
            "POST",
            "/api/ai/order".into(),
            Some(json!({"action":"close","pair":args["pair"],"reason":args["reason"]})),
        ),
        "run_server_ai" => (
            "POST",
            format!(
                "/api/ai/run?execute={}",
                args["execute"].as_bool().unwrap_or(false)
            ),
            None,
        ),
        _ => anyhow::bail!("未知工具"),
    };
    let mut request = http
        .request(method.parse()?, format!("{}{}", base, path))
        .header("cookie", cookie)
        .header("origin", base);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let r = request.send().await?;
    let status = r.status();
    let v: Value = r.json().await?;
    ensure!(
        status.is_success(),
        "{}",
        v["detail"].as_str().unwrap_or("请求失败")
    );
    Ok(v)
}
async fn login(http: &reqwest::Client, base: &str) -> Result<String> {
    let r=http.post(format!("{}/api/auth/login",base)).header("origin",base).json(&json!({"username":std::env::var("PANEL_USER").unwrap_or_else(|_|"admin".into()),"password":std::env::var("PANEL_PASS").context("缺少 PANEL_PASS")?})).send().await?;
    ensure!(
        r.status().is_success(),
        "后台登录失败，检查账号、密码和服务地址"
    );
    Ok(r.headers()
        .get(reqwest::header::SET_COOKIE)
        .context("后台没有返回会话")?
        .to_str()?
        .split(';')
        .next()
        .unwrap()
        .into())
}
pub async fn run() -> Result<()> {
    let base = std::env::var("PANEL_URL")
        .unwrap_or_else(|_| "https://trade.juyouagent.com".into())
        .trim_end_matches('/')
        .to_string();
    let url = url::Url::parse(&base)?;
    ensure!(
        url.scheme() == "https" && url.origin().ascii_serialization() == base,
        "PANEL_URL 必须是 HTTPS origin"
    );
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(650))
        .build()?;
    let mut cookie = String::new();
    let mut logged_at = 0i64;
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.len() > 65536 {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = req.get("id") else {
            continue;
        };
        let response = match req["method"].as_str().unwrap_or("") {
            "initialize" => {
                json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"tide-trade","version":env!("CARGO_PKG_VERSION")}}})
            }
            "ping" => json!({"jsonrpc":"2.0","id":id,"result":{}}),
            "tools/list" => json!({"jsonrpc":"2.0","id":id,"result":{"tools":tools()}}),
            "tools/call" => {
                let result: Result<Value> = async {
                    let now = chrono::Utc::now().timestamp();
                    if cookie.is_empty() || now - logged_at > 43100 {
                        cookie = login(&http, &base).await?;
                        logged_at = now;
                    }
                    call(
                        &http,
                        &base,
                        &cookie,
                        req["params"]["name"].as_str().context("缺少工具名")?,
                        &req["params"]["arguments"],
                    )
                    .await
                }
                .await;
                match result {
                    Ok(v) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":v.to_string()}]}})
                    }
                    Err(e) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"isError":true,"content":[{"type":"text","text":e.to_string()}]}})
                    }
                }
            }
            _ => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}})
            }
        };
        stdout
            .write_all(format!("{}\n", response).as_bytes())
            .await?;
        stdout.flush().await?;
    }
    Ok(())
}
