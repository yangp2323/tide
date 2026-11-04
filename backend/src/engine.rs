use crate::{
    exchange::{category, quantize, symbol, Bybit, Fill},
    model::{day, now, num, Config, Trade},
    App,
};
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::sync::Arc;

// Keep the independently audited inputs explicit in this pure risk function.
#[allow(clippy::too_many_arguments)]
pub fn entry_guard(
    c: &Config,
    held: &[Trade],
    pnl: f64,
    equity: f64,
    pair: &str,
    short: bool,
    confidence: Option<f64>,
    ai: bool,
) -> Result<()> {
    ensure!(c.running, "机器人已暂停");
    ensure!(c.pairs.iter().any(|p| p == pair), "交易对不在已选币种中");
    ensure!(
        !short || (c.market == "futures" && c.short),
        "当前不允许做空"
    );
    ensure!(held.len() < c.max_open, "已达到持仓上限");
    ensure!(!held.iter().any(|t| t.pair == pair), "这个交易对已有持仓");
    if ai {
        ensure!(c.ai.enabled, "AI 交易未开启");
        let start = equity - pnl;
        ensure!(start > 0.0, "无法确定当日起始权益");
        ensure!(
            pnl / start * 100.0 > -c.ai.max_daily_loss_pct,
            "达到当日亏损上限"
        );
        if let Some(conf) = confidence {
            ensure!(
                conf.is_finite() && (0.0..=1.0).contains(&conf) && conf >= c.ai.min_confidence,
                "低于最低置信度或置信度无效"
            );
        }
    }
    Ok(())
}
/// Position slots are an upper bound; reserve 5% and divide only among affordable slots.
pub fn entry_stake(budget: f64, slots: usize, minimum: f64) -> Option<f64> {
    if !budget.is_finite() || budget <= 0.0 || slots == 0 || !minimum.is_finite() || minimum < 0.0 {
        return None;
    }
    let usable = budget * 0.95;
    let affordable = if minimum > 0.0 {
        ((usable / minimum).floor() as usize).min(slots)
    } else {
        slots
    };
    if affordable == 0 {
        return None;
    }
    Some(usable / affordable as f64)
}

pub fn stop_price(t: &Trade, price: f64) -> (f64, f64) {
    let peak = if t.is_short {
        t.peak.min(price)
    } else {
        t.peak.max(price)
    };
    let favorable = if t.is_short {
        (t.open_rate - peak) / t.open_rate
    } else {
        (peak - t.open_rate) / t.open_rate
    };
    let stop = if favorable >= 0.06 {
        if t.is_short {
            t.stop_loss_abs.min(peak * 1.03)
        } else {
            t.stop_loss_abs.max(peak * 0.97)
        }
    } else {
        t.stop_loss_abs
    };
    (peak, stop)
}
pub fn realized(trades: &[Trade], today_only: bool) -> f64 {
    trades
        .iter()
        .filter(|t| {
            t.close_date
                .as_ref()
                .is_some_and(|s| !today_only || s.starts_with(&day()))
        })
        .map(|t| t.profit_abs)
        .sum()
}
pub fn usdt(wallet: &Value) -> (f64, f64) {
    let coin = wallet["coin"]
        .as_array()
        .and_then(|a| a.iter().find(|c| c["coin"] == "USDT"));
    let equity = coin.map(|c| num(&c["equity"])).unwrap_or(0.0);
    // WalletBalance excludes unrealized PnL. Reserve all locked funds and derivatives IM.
    let free = coin
        .map(|c| {
            num(&c["walletBalance"])
                - num(&c["locked"])
                - num(&c["totalPositionIM"])
                - num(&c["totalOrderIM"])
        })
        .unwrap_or(0.0)
        .max(0.0);
    (equity, free)
}
impl App {
    pub async fn settle(&self, order: &Value, fill: Fill) -> Result<()> {
        let link = order["link"].as_str().context("缺少订单标识")?;
        if fill.qty == 0.0 {
            self.store.order_state(link, "rejected")?;
            self.store.log("WARNING", &format!("订单 {} 未成交", link));
            return Ok(());
        }
        let mut t: Trade = serde_json::from_value(order["data"]["trade"].clone())?;
        if order["action"] == "entry" {
            t.open_rate = fill.price;
            t.amount = fill.qty
                - if t.market == "spot" {
                    fill.base_fee
                } else {
                    0.0
                };
            t.peak = fill.price;
            t.stake_amount = fill.qty * fill.price / t.leverage as f64;
            t.entry_fee = fill.fee;
            t.stop_loss_abs = fill.price * if t.is_short { 1.08 } else { 0.92 };
            self.store.insert_trade(t.clone(), link)?;
            self.store.log(
                "INFO",
                &format!("开仓 {} {} @ {}", t.pair, t.amount, t.open_rate),
            );
        } else {
            // A close can partially fill. Only the filled fraction becomes a closed trade.
            let current = self
                .store
                .open_trades()?
                .into_iter()
                .find(|x| x.trade_id == t.trade_id)
                .context("平仓订单对应的持仓不存在")?;
            t = current;
            ensure!(
                fill.qty <= t.amount * (1.0 + 1e-8),
                "平仓成交数量大于已记录持仓，停止核对"
            );
            let requested = num(&order["data"]["request"]["qty"]);
            let step = num(&order["data"]["qty_step"]);
            let dust = t.market == "spot"
                && requested > 0.0
                && (fill.qty - requested).abs() < 1e-10
                && t.amount - fill.qty < step;
            let fraction = if dust {
                1.0
            } else {
                (fill.qty / t.amount).min(1.0)
            };
            let mut remaining = t.clone();
            remaining.amount -= fill.qty;
            remaining.stake_amount *= 1.0 - fraction;
            remaining.entry_fee *= 1.0 - fraction;
            let dust_value = if dust {
                (t.amount - fill.qty) * t.open_rate
            } else {
                0.0
            };
            if dust {
                remaining.amount = 0.0;
                self.store.set_state(
                    &format!("dust:{}", t.trade_id),
                    &(t.amount - fill.qty).to_string(),
                )?;
            }
            t.amount = fill.qty;
            t.stake_amount *= fraction;
            t.entry_fee *= fraction;
            t.close_rate = Some(fill.price);
            t.close_date = Some(now());
            t.exit_reason = Some(
                order["data"]["reason"]
                    .as_str()
                    .unwrap_or("force_exit")
                    .into(),
            );
            t.profit_abs =
                (fill.price - t.open_rate) * fill.qty * if t.is_short { -1.0 } else { 1.0 }
                    - t.entry_fee
                    - fill.fee
                    - dust_value;
            t.profit_ratio = t.profit_abs / t.stake_amount;
            self.store.settle_exit(
                &t,
                if remaining.amount > 1e-10 {
                    Some(&remaining)
                } else {
                    None
                },
                link,
            )?;
            self.store.log(
                "INFO",
                &format!(
                    "平仓 {} {}，盈亏 {:.4} USDT",
                    t.pair, t.amount, t.profit_abs
                ),
            );
        }
        Ok(())
    }
    pub async fn reconcile(&self, c: &Config) -> Result<()> {
        let ex = self.exchange(c);
        for o in self.store.unresolved()? {
            let result: Result<()> = async {
                let t: Trade = serde_json::from_value(o["data"]["trade"].clone())?;
                if let Some(fill) = ex
                    .confirm(&t.market, &t.pair, o["link"].as_str().unwrap())
                    .await?
                {
                    self.settle(&o, fill).await?;
                }
                Ok(())
            }
            .await;
            if let Err(e) = result {
                self.store
                    .log("ERROR", &format!("订单 {} 核对失败：{}", o["link"], e));
            }
        }
        Ok(())
    }
    pub async fn await_order(&self, c: &Config, link: &str) -> Result<()> {
        let ex = self.exchange(c);
        let order = self
            .store
            .unresolved()?
            .into_iter()
            .find(|o| o["link"] == link)
            .context("订单不存在")?;
        let t: Trade = serde_json::from_value(order["data"]["trade"].clone())?;
        for _ in 0..8 {
            if let Some(fill) = ex.confirm(&t.market, &t.pair, link).await? {
                let executed = fill.qty > 0.0;
                self.settle(&order, fill).await?;
                ensure!(executed, "订单未成交");
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        }
        bail!("订单结果待确认，系统将自动核对；请勿重复提交")
    }
    pub async fn open_position(
        &self,
        pair: &str,
        short: bool,
        tag: &str,
        confidence: Option<f64>,
    ) -> Result<()> {
        // Caller must hold gate from fresh risk checks through durable reservation and submission.
        let c = self.store.config()?;
        self.reconcile(&c).await?;
        ensure!(
            self.store.unresolved()?.is_empty(),
            "有待核对订单，禁止继续开仓"
        );
        let ex = self.exchange(&c);
        let wallet = ex.wallet().await?;
        let (mut equity, free) = usdt(&wallet);
        let trades = self.store.trades()?;
        let held = self.store.open_trades()?;
        for t in &held {
            if t.market == "spot" {
                equity += t.amount * ex.price(&t.market, &t.pair).await?;
            }
        }
        ensure!(
            self.store
                .state("risk_error")?
                .unwrap_or_default()
                .is_empty(),
            "持仓核对异常，暂停新开仓；请查看运行日志"
        );
        let ai = tag != "trend";
        entry_guard(
            &c,
            &held,
            realized(&trades, true),
            equity,
            pair,
            short,
            confidence,
            ai,
        )?;
        let ins = ex.instrument(&c.market, pair).await?;
        let price = ex.price(&c.market, pair).await?;
        let used: f64 = held.iter().map(|t| t.stake_amount).sum();
        let budget = if let Some(cap) = c.capital {
            (cap - used).max(0.0).min(free)
        } else {
            free
        };
        let futures = c.market == "futures";
        let leverage = if futures { c.leverage } else { 1 };
        if futures {
            let account = ex.get("/v5/account/info", json!({}), true).await?;
            ensure!(
                account["marginMode"] == "ISOLATED_MARGIN",
                "请先在 Bybit 将统一账户设为逐仓保证金模式"
            );
            let positions = ex.positions().await?;
            let sym = symbol(pair);
            ensure!(
                !positions
                    .iter()
                    .any(|p| p["symbol"] == sym && num(&p["size"]) > 0.0),
                "Bybit 已有该合约仓位，不合并手动仓位"
            );
            ensure!(
                !positions
                    .iter()
                    .any(|p| p["symbol"] == sym && num(&p["positionIdx"]) != 0.0),
                "该合约不是单向持仓模式，请先在 Bybit 修改"
            );
            ensure!(
                leverage as f64 <= num(&ins["leverageFilter"]["maxLeverage"]),
                "交易对杠杆上限不足"
            );
            if let Err(e)=ex.post("/v5/position/set-leverage",json!({"category":"linear","symbol":sym,"buyLeverage":leverage.to_string(),"sellLeverage":leverage.to_string()})).await {
                // 110043 means leverage is already set to the requested value.
                ensure!(e.to_string().contains("110043"),"{}",e);
            }
        }
        ensure!(
            !ex.open_orders(&c.market)
                .await?
                .iter()
                .any(|o| o["symbol"] == symbol(pair)),
            "该交易对存在挂单，先处理现有订单"
        );
        let step = if futures {
            ins["lotSizeFilter"]["qtyStep"].as_str()
        } else {
            ins["lotSizeFilter"]["basePrecision"].as_str()
        }
        .context("缺少下单精度")?;
        let min_notional = num(&ins["lotSizeFilter"][if futures {
            "minNotionalValue"
        } else {
            "minOrderAmt"
        }]);
        let qty_step: f64 = step.parse()?;
        ensure!(
            qty_step.is_finite() && qty_step > 0.0 && price.is_finite() && price > 0.0,
            "交易所价格或下单精度无效"
        );
        // Round the exchange minimum up to a tradable quantity before allocating slots.
        let min_qty = (min_notional / price)
            .max(num(&ins["lotSizeFilter"]["minOrderQty"]))
            .max(qty_step);
        let min_stake = (min_qty / qty_step).ceil() * qty_step * price / leverage as f64;
        let stake = entry_stake(budget, c.max_open - held.len(), min_stake)
            .context("可用预算预留 5% 后不足该交易对的最小下单要求")?;
        let qty = quantize(stake * leverage as f64 / price, step)?;
        let amount: f64 = qty.parse()?;
        ensure!(
            amount * price >= min_notional && amount >= num(&ins["lotSizeFilter"]["minOrderQty"]),
            "订单低于交易所最小金额或数量"
        );
        let max_qty = num(&ins["lotSizeFilter"][if futures {
            "maxMktOrderQty"
        } else {
            "maxMarketOrderQty"
        }]);
        ensure!(
            max_qty == 0.0 || amount <= max_qty,
            "订单超过市价单数量上限"
        );
        let link = format!("t{}", uuid::Uuid::new_v4().simple());
        let t = Trade {
            trade_id: 0,
            pair: pair.into(),
            market: c.market.clone(),
            is_short: short,
            leverage,
            amount,
            open_rate: price,
            stake_amount: amount * price / leverage as f64,
            open_date: now(),
            close_rate: None,
            close_date: None,
            profit_abs: 0.0,
            profit_ratio: 0.0,
            stop_loss_abs: price * if short { 1.08 } else { 0.92 },
            peak: price,
            enter_tag: tag.into(),
            exit_reason: None,
            entry_fee: 0.0,
        };
        let mut body = json!({"category":category(&c.market),"symbol":symbol(pair),"side":if short{"Sell"}else{"Buy"},"orderType":"Market","qty":qty,"orderLinkId":link});
        if futures {
            let tick = ins["priceFilter"]["tickSize"]
                .as_str()
                .context("缺少价格精度")?;
            body["positionIdx"] = json!(0);
            body["stopLoss"] = json!(quantize(t.stop_loss_abs, tick)?);
            body["slTriggerBy"] = json!("LastPrice");
            body["tpslMode"] = json!("Full");
            body["slOrderType"] = json!("Market");
        } else {
            body["marketUnit"] = json!("baseCoin");
            body["isLeverage"] = json!(0);
        }
        self.store
            .reserve(&link, "entry", pair, &json!({"trade":t,"request":body}))?;
        if let Err(e) = ex.post("/v5/order/create", body).await {
            if e.downcast_ref::<crate::exchange::Rejected>()
                .is_some_and(|e| e.definitive())
            {
                self.store.order_state(&link, "rejected")?;
            }
            self.store
                .log("ERROR", &format!("订单 {} 提交失败：{}", link, e));
            return Err(e);
        }
        self.await_order(&c, &link).await?;
        let _ = self
            .telegram(
                &c,
                &format!("开仓 {} {}", pair, if short { "空" } else { "多" }),
            )
            .await;
        Ok(())
    }
    pub async fn close_position(&self, id: i64, reason: &str) -> Result<()> {
        let c = self.store.config()?;
        self.reconcile(&c).await?;
        let t = self
            .store
            .open_trades()?
            .into_iter()
            .find(|t| t.trade_id == id)
            .context("持仓不存在或已平仓")?;
        ensure!(
            !self.store.unresolved()?.iter().any(|o| o["pair"] == t.pair),
            "该交易对有待确认订单"
        );
        let ex = self.exchange(&c);
        let ins = ex.instrument(&t.market, &t.pair).await?;
        let step = if t.market == "futures" {
            ins["lotSizeFilter"]["qtyStep"].as_str()
        } else {
            ins["lotSizeFilter"]["basePrecision"].as_str()
        }
        .context("缺少下单精度")?;
        let qty = quantize(t.amount, step)?;
        let link = format!("t{}", uuid::Uuid::new_v4().simple());
        let mut body = json!({"category":category(&t.market),"symbol":symbol(&t.pair),"side":if t.is_short{"Buy"}else{"Sell"},"orderType":"Market","qty":qty,"orderLinkId":link});
        if t.market == "futures" {
            body["reduceOnly"] = json!(true);
            body["positionIdx"] = json!(0);
        } else {
            body["marketUnit"] = json!("baseCoin");
        }
        self.store.reserve(
            &link,
            "exit",
            &t.pair,
            &json!({"trade":t,"reason":reason,"request":body,"qty_step":step}),
        )?;
        if let Err(e) = ex.post("/v5/order/create", body).await {
            if e.downcast_ref::<crate::exchange::Rejected>()
                .is_some_and(|e| e.definitive())
            {
                self.store.order_state(&link, "rejected")?;
            }
            return Err(e);
        }
        self.await_order(&c, &link).await?;
        let _ = self
            .telegram(&c, &format!("平仓 {} ({})", t.pair, reason))
            .await;
        Ok(())
    }
    pub async fn sync_external_exit(&self, c: &Config, t: &Trade, ex: &Bybit) -> Result<bool> {
        let positions = ex.positions().await?;
        let amount: f64 = positions
            .iter()
            .filter(|p| {
                p["symbol"] == symbol(&t.pair)
                    && p["side"] == if t.is_short { "Sell" } else { "Buy" }
            })
            .map(|p| num(&p["size"]))
            .sum();
        if (amount - t.amount).abs() <= t.amount * 1e-8 {
            // Restore the exchange-hosted stop if it was removed or differs from
            // the fill-based stop. Never rely only on the submission-time quote.
            let p = positions
                .iter()
                .find(|p| {
                    p["symbol"] == symbol(&t.pair)
                        && p["side"] == if t.is_short { "Sell" } else { "Buy" }
                })
                .context("缺少合约仓位")?;
            let ins = ex.instrument(&t.market, &t.pair).await?;
            let tick = ins["priceFilter"]["tickSize"]
                .as_str()
                .context("缺少价格精度")?;
            let desired = quantize(t.stop_loss_abs, tick)?;
            if (num(&p["stopLoss"]) - desired.parse::<f64>()?).abs()
                > num(&ins["priceFilter"]["tickSize"]) * 0.5
            {
                ex.post("/v5/position/trading-stop",json!({"category":"linear","symbol":symbol(&t.pair),"tpslMode":"Full","positionIdx":0,"stopLoss":desired,"slTriggerBy":"LastPrice"})).await?;
            }
            return Ok(false);
        }
        ensure!(
            amount == 0.0,
            "{} 交易所仓位数量与记录不一致，需人工核对",
            t.pair
        );
        let opened = chrono::NaiveDateTime::parse_from_str(&t.open_date, "%Y-%m-%d %H:%M:%S")?
            .and_utc()
            .timestamp_millis();
        let start = opened.max(chrono::Utc::now().timestamp_millis() - 6 * 86400000);
        let mut cursor = String::new();
        let mut fills = vec![];
        loop {
            let mut args =
                json!({"category":"linear","symbol":symbol(&t.pair),"startTime":start,"limit":100});
            if !cursor.is_empty() {
                args["cursor"] = json!(cursor);
            }
            let v = ex.get("/v5/execution/list", args, true).await?;
            fills.extend(
                v["list"]
                    .as_array()
                    .context("无法读取平仓成交")?
                    .iter()
                    .filter(|e| {
                        e["side"] == if t.is_short { "Buy" } else { "Sell" }
                            && num(&e["closedSize"]) > 0.0
                            && e["execType"] == "Trade"
                    })
                    .cloned(),
            );
            let next = v["nextPageCursor"].as_str().unwrap_or("");
            if next.is_empty() || next == cursor {
                break;
            }
            cursor = next.into();
        }
        let qty: f64 = fills.iter().map(|e| num(&e["closedSize"])).sum();
        ensure!(
            (qty - t.amount).abs() < t.amount * 1e-6,
            "{} 已无仓位，但成交记录无法匹配；需人工核对",
            t.pair
        );
        let value: f64 = fills
            .iter()
            .map(|e| num(&e["closedSize"]) * num(&e["execPrice"]))
            .sum();
        let fee: f64 = fills.iter().map(|e| num(&e["execFee"])).sum();
        let link = format!("external-{}", t.trade_id);
        let order =
            json!({"link":link,"action":"exit","data":{"trade":t,"reason":"stoploss_on_exchange"}});
        // This is an already confirmed external fill, not an outgoing order.
        // settle_exit records it atomically; no synthetic pending order can be
        // left behind if the process stops before committing the trade.
        self.settle(
            &order,
            Fill {
                qty,
                price: value / qty,
                fee,
                base_fee: 0.0,
            },
        )
        .await?;
        let _ = self
            .telegram(c, &format!("已同步交易所平仓 {}", t.pair))
            .await;
        Ok(true)
    }
    pub async fn tick(&self) -> Result<()> {
        let c = self.store.config()?;
        if c.exchange.key.is_empty() {
            return Ok(());
        }
        self.reconcile(&c).await?;
        let ex = self.exchange(&c);
        // Risk exits continue while new entries are paused.
        let mut risk_errors = Vec::new();
        for mut t in self.store.open_trades()? {
            let result:Result<()>=async {
            if self.store.unresolved()?.iter().any(|o| o["pair"] == t.pair) {
                return Ok(());
            }
            if t.market == "futures" && self.sync_external_exit(&c, &t, &ex).await? {
                return Ok(());
            }
            let price = ex.price(&t.market, &t.pair).await?;
            let (peak, stop) = stop_price(&t, price);
            if t.market == "futures" && stop != t.stop_loss_abs {
                let ins = ex.instrument(&t.market, &t.pair).await?;
                let tick = ins["priceFilter"]["tickSize"]
                    .as_str()
                    .context("缺少价格精度")?;
                ex.post("/v5/position/trading-stop",json!({"category":"linear","symbol":symbol(&t.pair),"tpslMode":"Full","positionIdx":0,"stopLoss":quantize(stop,tick)?,"slTriggerBy":"LastPrice"})).await?;
            }
            t.peak = peak;
            t.stop_loss_abs = stop;
            self.store.update_trade(&t)?;
            if t.market == "spot" && price <= stop {
                self.close_position(
                    t.trade_id,
                    if peak >= t.open_rate * 1.06 {
                        "trailing_stop_loss"
                    } else {
                        "stop_loss"
                    },
                )
                .await?;
            }
            Ok(())
            }.await;
            if let Err(e) = result {
                risk_errors.push(e.to_string());
            }
        }
        self.store
            .set_state("risk_error", &risk_errors.join("；"))?;
        ensure!(
            risk_errors.is_empty(),
            "持仓风控异常：{}",
            risk_errors.join("；")
        );
        // AI is the only automatic entry and discretionary exit mode. The loop
        // above remains responsible for exchange-side stops and trailing stops
        // even while the bot is paused or AI analysis is disabled.
        Ok(())
    }
}
pub async fn scheduler(app: Arc<App>) {
    loop {
        let result = {
            let _guard = app.gate.lock().await;
            app.tick().await
        };
        if let Err(e) = result {
            app.store.log("ERROR", &e.to_string());
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sizing_uses_only_affordable_slots() {
        assert_eq!(entry_stake(6.0, 3, 5.0), Some(6.0 * 0.95));
        assert_eq!(entry_stake(10.0, 3, 5.0), Some(9.5));
        assert_eq!(entry_stake(20.0, 3, 5.0), Some(19.0 / 3.0));
        assert_eq!(entry_stake(100.0, 3, 5.0), Some(95.0 / 3.0));
        assert_eq!(entry_stake(100.0, 1, 5.0), Some(95.0));
        assert_eq!(entry_stake(5.0, 3, 5.0), None);
        assert_eq!(entry_stake(5.0 / 0.95, 3, 5.0), Some(5.0));
        assert_eq!(entry_stake(1.0, 3, 0.0), Some(0.95 / 3.0));
        assert_eq!(entry_stake(100.0, 0, 5.0), None);
        assert_eq!(entry_stake(-1.0, 3, 5.0), None);
        assert_eq!(entry_stake(f64::NAN, 3, 5.0), None);
        assert_eq!(entry_stake(f64::INFINITY, 3, 5.0), None);
        assert_eq!(entry_stake(100.0, 3, f64::NAN), None);
    }

    #[test]
    fn daily_loss_and_manual_confidence_are_enforced() {
        let mut c = Config {
            running: true,
            ..Config::default()
        };
        c.ai.enabled = true;
        assert!(entry_guard(&c, &[], 0.0, 100.0, "BTC/USDT", false, Some(0.1), true).is_err());
        assert!(entry_guard(&c, &[], -6.0, 94.0, "BTC/USDT", false, Some(0.9), true).is_err());
        assert!(entry_guard(&c, &[], 0.0, 100.0, "BTC/USDT", false, Some(f64::NAN), true).is_err());
        assert!(entry_guard(&c, &[], 0.0, 100.0, "BTC/USDT", false, Some(0.9), true).is_ok());
        c.market = "futures".into();
        c.short = true;
        assert!(entry_guard(&c, &[], 0.0, 100.0, "BTC/USDT", true, Some(0.9), true).is_ok());
    }
}
