use crate::{
    indicators::Candle,
    model::{num, Credentials},
};
use anyhow::{bail, ensure, Context, Result};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

#[derive(Clone)]
pub struct Bybit {
    pub http: reqwest::Client,
    pub base: String,
    pub credentials: Credentials,
}
#[derive(Clone, Debug)]
pub struct Fill {
    pub qty: f64,
    pub price: f64,
    pub fee: f64,
    pub base_fee: f64,
}
#[derive(Debug)]
pub struct Rejected(pub String, pub i64);
impl Rejected {
    pub fn definitive(&self) -> bool {
        !matches!(self.1, -1 | 10000 | 10014 | 10016 | 10019 | 110072)
    }
}
impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for Rejected {}
pub fn symbol(pair: &str) -> String {
    pair.split(':').next().unwrap_or(pair).replace('/', "")
}
pub fn category(market: &str) -> &'static str {
    if market == "futures" {
        "linear"
    } else {
        "spot"
    }
}
pub fn signature(secret: &str, payload: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(payload.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}
impl Bybit {
    pub fn new(http: reqwest::Client, c: Credentials) -> Self {
        Self {
            http,
            credentials: c,
            base: std::env::var("BYBIT_BASE_URL")
                .unwrap_or_else(|_| "https://api.bybit.com".into()),
        }
    }
    pub async fn get(&self, path: &str, params: Value, private: bool) -> Result<Value> {
        self.request("GET", path, params, private).await
    }
    pub async fn post(&self, path: &str, params: Value) -> Result<Value> {
        self.request("POST", path, params, true).await
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        params: Value,
        private: bool,
    ) -> Result<Value> {
        let payload = if method == "GET" {
            let mut s = url::form_urlencoded::Serializer::new(String::new());
            if let Some(obj) = params.as_object() {
                for (k, v) in obj {
                    s.append_pair(
                        k,
                        &v.as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| v.to_string()),
                    );
                }
            }
            s.finish()
        } else {
            params.to_string()
        };
        let uri = if method == "GET" && !payload.is_empty() {
            format!("{}{}?{}", self.base, path, payload)
        } else {
            format!("{}{}", self.base, path)
        };
        let mut r = self.http.request(method.parse()?, uri);
        if private {
            ensure!(
                !self.credentials.key.is_empty() && !self.credentials.secret.is_empty(),
                "还没保存 Bybit API Key"
            );
            let ts = chrono::Utc::now().timestamp_millis().to_string();
            let sig = signature(
                &self.credentials.secret,
                &format!("{}{}5000{}", ts, self.credentials.key, payload),
            );
            r = r
                .header("X-BAPI-API-KEY", &self.credentials.key)
                .header("X-BAPI-TIMESTAMP", ts)
                .header("X-BAPI-RECV-WINDOW", "5000")
                .header("X-BAPI-SIGN", sig);
        }
        if method == "POST" {
            r = r.header("content-type", "application/json").body(payload);
        }
        let resp = r
            .send()
            .await
            .context("连接 Bybit 失败；请先核对订单状态，不要重复提交")?;
        let status = resp.status();
        let v: Value = resp.json().await.context("Bybit 返回了无效响应")?;
        ensure!(status.is_success(), "Bybit HTTP {}", status);
        if v["retCode"].as_i64() != Some(0) {
            return Err(Rejected(
                format!(
                    "Bybit {}：{}",
                    v["retCode"],
                    v["retMsg"].as_str().unwrap_or("未知错误")
                ),
                v["retCode"].as_i64().unwrap_or(-1),
            )
            .into());
        }
        Ok(v["result"].clone())
    }
    pub async fn tickers(&self, market: &str) -> Result<Value> {
        self.get(
            "/v5/market/tickers",
            json!({"category":category(market)}),
            false,
        )
        .await
    }
    pub async fn price(&self, market: &str, pair: &str) -> Result<f64> {
        let v = self
            .get(
                "/v5/market/tickers",
                json!({"category":category(market),"symbol":symbol(pair)}),
                false,
            )
            .await?;
        let p = num(&v["list"][0]["lastPrice"]);
        ensure!(p > 0.0, "没有可用行情");
        Ok(p)
    }
    pub async fn instrument(&self, market: &str, pair: &str) -> Result<Value> {
        let v = self
            .get(
                "/v5/market/instruments-info",
                json!({"category":category(market),"symbol":symbol(pair)}),
                false,
            )
            .await?;
        let row = v["list"]
            .as_array()
            .and_then(|a| a.first())
            .context("交易对不存在")?;
        ensure!(row["status"] == "Trading", "交易对当前不可交易");
        Ok(row.clone())
    }
    pub async fn instruments(&self, market: &str) -> Result<Vec<Value>> {
        let mut out = vec![];
        let mut cursor = String::new();
        loop {
            let mut args = json!({"category":category(market),"limit":1000});
            if !cursor.is_empty() {
                args["cursor"] = json!(cursor);
            }
            let r = self.get("/v5/market/instruments-info", args, false).await?;
            out.extend(
                r["list"]
                    .as_array()
                    .context("缺少币种数据")?
                    .iter()
                    .cloned(),
            );
            let next = r["nextPageCursor"].as_str().unwrap_or("");
            if next.is_empty() || next == cursor {
                break;
            }
            cursor = next.into();
        }
        Ok(out)
    }
    pub async fn candles(
        &self,
        market: &str,
        pair: &str,
        interval: u32,
        limit: usize,
        end: Option<i64>,
    ) -> Result<Vec<Candle>> {
        self.klines(market, pair, &interval.to_string(), interval, limit, end)
            .await
    }
    /// Closed daily candles (Bybit names that interval "D").
    pub async fn daily_candles(
        &self,
        market: &str,
        pair: &str,
        limit: usize,
    ) -> Result<Vec<Candle>> {
        self.klines(market, pair, "D", 1440, limit, None).await
    }
    /// 24h change, turnover and, for perpetuals, funding rate and open interest.
    pub async fn stats(&self, market: &str, pair: &str) -> Result<Value> {
        let v = self
            .get(
                "/v5/market/tickers",
                json!({"category":category(market),"symbol":symbol(pair)}),
                false,
            )
            .await?;
        let t = &v["list"][0];
        let mut out = json!({"change_24h_pct":num(&t["price24hPcnt"])*100.0,"turnover_24h_usdt":num(&t["turnover24h"])});
        if market == "futures" {
            out["funding_rate_pct"] = json!(num(&t["fundingRate"]) * 100.0);
            out["open_interest"] = json!(num(&t["openInterest"]));
        }
        Ok(out)
    }
    async fn klines(
        &self,
        market: &str,
        pair: &str,
        code: &str,
        interval: u32,
        limit: usize,
        end: Option<i64>,
    ) -> Result<Vec<Candle>> {
        let mut args = json!({"category":category(market),"symbol":symbol(pair),"interval":code,"limit":limit.min(1000)});
        if let Some(end) = end {
            args["end"] = json!(end);
        }
        let v = self.get("/v5/market/kline", args, false).await?;
        let now = chrono::Utc::now().timestamp_millis();
        let mut rows = vec![];
        for r in v["list"].as_array().context("没有 K 线数据")? {
            let t = num(&r[0]) as i64;
            if t + interval as i64 * 60000 > now {
                continue;
            }
            rows.push(Candle {
                time: t,
                open: num(&r[1]),
                high: num(&r[2]),
                low: num(&r[3]),
                close: num(&r[4]),
                volume: num(&r[5]),
            });
        }
        rows.sort_by_key(|c| c.time);
        Ok(rows)
    }
    pub async fn wallet(&self) -> Result<Value> {
        let v = self
            .get(
                "/v5/account/wallet-balance",
                json!({"accountType":"UNIFIED"}),
                true,
            )
            .await?;
        Ok(v["list"][0].clone())
    }
    pub async fn positions(&self) -> Result<Vec<Value>> {
        let mut out = vec![];
        let mut cursor = String::new();
        loop {
            let mut args = json!({"category":"linear","settleCoin":"USDT","limit":200});
            if !cursor.is_empty() {
                args["cursor"] = json!(cursor);
            }
            let v = self.get("/v5/position/list", args, true).await?;
            out.extend(
                v["list"]
                    .as_array()
                    .context("持仓数据无效")?
                    .iter()
                    .cloned(),
            );
            let next = v["nextPageCursor"].as_str().unwrap_or("");
            if next.is_empty() || next == cursor {
                break;
            }
            cursor = next.into();
        }
        Ok(out)
    }
    pub async fn open_orders(&self, market: &str) -> Result<Vec<Value>> {
        let mut out = vec![];
        let mut cursor = String::new();
        loop {
            let mut args = json!({"category":category(market),"limit":50,"openOnly":0});
            if market == "futures" {
                args["settleCoin"] = json!("USDT");
            }
            if !cursor.is_empty() {
                args["cursor"] = json!(cursor);
            }
            let v = self.get("/v5/order/realtime", args, true).await?;
            out.extend(
                v["list"]
                    .as_array()
                    .context("挂单数据无效")?
                    .iter()
                    .cloned(),
            );
            let next = v["nextPageCursor"].as_str().unwrap_or("");
            if next.is_empty() || next == cursor {
                break;
            }
            cursor = next.into();
        }
        Ok(out)
    }
    pub async fn lookup(&self, market: &str, pair: &str, link: &str) -> Result<Option<Value>> {
        let args = json!({"category":category(market),"symbol":symbol(pair),"orderLinkId":link});
        for path in ["/v5/order/realtime", "/v5/order/history"] {
            let v = self.get(path, args.clone(), true).await?;
            if let Some(row) = v["list"].as_array().and_then(|a| a.first()) {
                return Ok(Some(row.clone()));
            }
        }
        Ok(None)
    }
    pub async fn fill(&self, market: &str, pair: &str, order: &Value) -> Result<Fill> {
        let qty = num(&order["cumExecQty"]);
        let price = num(&order["avgPrice"]);
        ensure!(qty > 0.0 && price > 0.0, "订单没有成交");
        let mut fee = 0.0;
        let mut base_fee = 0.0;
        let base = pair.split('/').next().unwrap_or("");
        let mut cursor = String::new();
        let mut seen = 0;
        loop {
            let mut args = json!({"category":category(market),"symbol":symbol(pair),"orderId":order["orderId"],"limit":100});
            if !cursor.is_empty() {
                args["cursor"] = json!(cursor);
            }
            let v = self.get("/v5/execution/list", args, true).await?;
            for e in v["list"].as_array().context("缺少成交明细")? {
                if e["execType"] != "Trade" {
                    continue;
                }
                seen += 1;
                let f = num(&e["execFee"]);
                let currency = e["feeCurrency"].as_str().unwrap_or(
                    if market == "spot" && order["side"] == "Buy" {
                        base
                    } else {
                        "USDT"
                    },
                );
                if currency == base {
                    base_fee += f;
                    fee += f * num(&e["execPrice"]);
                } else if currency == "USDT" {
                    fee += f;
                } else {
                    bail!("无法估值的手续费币种 {}", currency);
                }
            }
            let next = v["nextPageCursor"].as_str().unwrap_or("");
            if next.is_empty() || next == cursor {
                break;
            }
            cursor = next.into();
        }
        ensure!(seen > 0, "订单已成交，成交明细尚未可见，稍后自动核对");
        Ok(Fill {
            qty,
            price,
            fee,
            base_fee,
        })
    }
    /// Net USDT funding for a linear symbol since `start` (ms): negative is paid, positive received.
    /// The transaction log allows at most 7 days per query, so the range is walked in windows.
    pub async fn funding(&self, pair: &str, start: i64) -> Result<f64> {
        const WEEK: i64 = 7 * 86_400_000;
        let sym = symbol(pair);
        let now = chrono::Utc::now().timestamp_millis();
        let mut from = start.max(now - 2 * 365 * 86_400_000);
        let mut total = 0.0;
        while from < now {
            let to = (from + WEEK).min(now);
            let mut cursor = String::new();
            loop {
                let mut args = json!({"accountType":"UNIFIED","category":"linear","currency":"USDT","type":"SETTLEMENT","startTime":from,"endTime":to,"limit":50});
                if !cursor.is_empty() {
                    args["cursor"] = json!(cursor);
                }
                let v = self.get("/v5/account/transaction-log", args, true).await?;
                total += v["list"]
                    .as_array()
                    .context("资金费记录无效")?
                    .iter()
                    .filter(|e| e["symbol"] == sym)
                    .map(|e| num(&e["funding"]))
                    .sum::<f64>();
                let next = v["nextPageCursor"].as_str().unwrap_or("");
                if next.is_empty() || next == cursor {
                    break;
                }
                cursor = next.into();
            }
            from = to;
        }
        Ok(total)
    }
    pub async fn confirm(&self, market: &str, pair: &str, link: &str) -> Result<Option<Fill>> {
        let Some(o) = self.lookup(market, pair, link).await? else {
            return Ok(None);
        };
        let terminal = matches!(
            o["orderStatus"].as_str().unwrap_or(""),
            "Filled" | "Cancelled" | "Rejected" | "PartiallyFilledCanceled" | "Deactivated"
        );
        if !terminal {
            return Ok(None);
        }
        if num(&o["cumExecQty"]) == 0.0 {
            return Ok(Some(Fill {
                qty: 0.0,
                price: 0.0,
                fee: 0.0,
                base_fee: 0.0,
            }));
        }
        Ok(Some(self.fill(market, pair, &o).await?))
    }
}
pub fn quantize(value: f64, step: &str) -> Result<String> {
    let s: f64 = step.parse()?;
    ensure!(
        value.is_finite() && value > 0.0 && s > 0.0,
        "金额或精度无效"
    );
    let decimals = step
        .trim_end_matches('0')
        .split('.')
        .nth(1)
        .map(str::len)
        .unwrap_or(0)
        .min(12);
    let n = (value / s + 1e-9).floor() * s;
    ensure!(n > 0.0, "订单金额低于交易所最小精度");
    Ok(format!("{:.*}", decimals, n))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quantity_never_rounds_up() {
        assert_eq!(quantize(1.23459, "0.001").unwrap(), "1.234");
        assert_eq!(quantize(123.9, "1").unwrap(), "123");
        assert!(quantize(0.0001, "0.001").is_err());
    }
    #[test]
    fn symbols() {
        assert_eq!(symbol("BTC/USDT:USDT"), "BTCUSDT");
    }
}
