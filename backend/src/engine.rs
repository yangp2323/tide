use crate::{
    exchange::{category, quantize, symbol, Bybit, Fill},
    indicators,
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

/// Initial stop: 2.5 × ATR(14) of the 4h candles, kept between 2% and 10% of price.
pub fn stop_distance(atr: f64, price: f64) -> Option<f64> {
    let d = 2.5 * atr / price;
    (d.is_finite() && d > 0.0).then(|| d.clamp(0.02, 0.10))
}
/// Lower the per-trade leverage until the stop sits at most 60% of the way to the
/// isolated-margin liquidation price (about 1 / leverage), so the stop triggers first.
pub fn trade_leverage(max: u32, stop: f64) -> u32 {
    ((0.6 / stop + 1e-9).floor() as u32).clamp(1, max.max(1))
}
/// Per-trade leverage ceiling: the configured maximum, but never above the symbol's own limit
/// (which can be fractional, e.g. 12.5×).
pub fn leverage_cap(configured: u32, symbol_max: f64) -> u32 {
    if symbol_max.is_finite() && symbol_max >= 1.0 {
        configured.min(symbol_max.floor() as u32)
    } else {
        1
    }
}
/// Margin that loses `risk_pct` percent of `equity` if the stop is hit (before fees and slippage).
pub fn risk_stake(equity: f64, risk_pct: f64, stop: f64, leverage: u32) -> f64 {
    equity * risk_pct / 100.0 / stop / leverage as f64
}
/// Trailing starts once the price moved one stop distance in favor, then trails by half of it.
/// Trades opened before ATR stops keep the former fixed 6% / 3% rule.
pub fn trail_params(t: &Trade) -> (f64, f64) {
    if t.stop_dist > 0.0 {
        (t.stop_dist, t.stop_dist / 2.0)
    } else {
        (0.06, 0.03)
    }
}
/// Today's PnL for the daily loss limit: realized today plus every open position's floating loss.
pub fn daily_pnl(realized_today: f64, unrealized: &[f64]) -> f64 {
    realized_today + unrealized.iter().map(|u| u.min(0.0)).sum::<f64>()
}
pub fn stop_price(t: &Trade, price: f64) -> (f64, f64) {
    let (activate, trail) = trail_params(t);
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
    let stop = if favorable >= activate {
        if t.is_short {
            t.stop_loss_abs.min(peak * (1.0 + trail))
        } else {
            t.stop_loss_abs.max(peak * (1.0 - trail))
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
            let d = if t.stop_dist > 0.0 { t.stop_dist } else { 0.08 };
            t.stop_loss_abs = fill.price * if t.is_short { 1.0 + d } else { 1.0 - d };
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
            let funding_total = if t.market == "futures" {
                self.funding_since_open(&t).await
            } else {
                0.0
            };
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
            // Funding settles on the whole position; the closed slice takes its share of
            // whatever earlier partial closes have not booked yet.
            let funding = (funding_total - t.funding) * fraction;
            let mut remaining = t.clone();
            remaining.amount -= fill.qty;
            remaining.stake_amount *= 1.0 - fraction;
            remaining.entry_fee *= 1.0 - fraction;
            remaining.funding += funding;
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
            t.funding = funding;
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
                    - dust_value
                    + funding;
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
    /// Funding paid (negative) or received since the trade opened. A lookup failure must not
    /// block settlement, so it is logged and only the already booked funding is kept.
    async fn funding_since_open(&self, t: &Trade) -> f64 {
        let result: Result<f64> = async {
            let opened = chrono::NaiveDateTime::parse_from_str(&t.open_date, "%Y-%m-%d %H:%M:%S")?
                .and_utc()
                .timestamp_millis();
            self.exchange(&self.store.config()?)
                .funding(&t.pair, opened)
                .await
        }
        .await;
        result.unwrap_or_else(|e| {
            self.store.log(
                "WARNING",
                &format!("{} 资金费读取失败，未计入盈亏：{}", t.pair, e),
            );
            t.funding
        })
    }
    /// Account equity (spot holdings at market), free USDT and today's PnL for the loss limit.
    pub async fn account_risk(&self, ex: &Bybit, held: &[Trade]) -> Result<(f64, f64, f64)> {
        let (mut equity, free) = usdt(&ex.wallet().await?);
        let mut unrealized = vec![];
        for t in held {
            let price = ex.price(&t.market, &t.pair).await?;
            if t.market == "spot" {
                equity += t.amount * price;
            }
            unrealized.push(num(&t.mark(price)["profit_abs"]));
        }
        let pnl = daily_pnl(realized(&self.store.trades()?, true), &unrealized);
        Ok((equity, free, pnl))
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
        let held = self.store.open_trades()?;
        let (equity, free, pnl) = self.account_risk(&ex, &held).await?;
        ensure!(
            self.store
                .state("risk_error")?
                .unwrap_or_default()
                .is_empty(),
            "持仓核对异常，暂停新开仓；请查看运行日志"
        );
        let ai = tag != "trend";
        entry_guard(&c, &held, pnl, equity, pair, short, confidence, ai)?;
        let ins = ex.instrument(&c.market, pair).await?;
        let price = ex.price(&c.market, pair).await?;
        let used: f64 = held.iter().map(|t| t.stake_amount).sum();
        let budget = if let Some(cap) = c.capital {
            (cap - used).max(0.0).min(free)
        } else {
            free
        };
        let futures = c.market == "futures";
        let candles = ex.candles(&c.market, pair, 240, 300, None).await?;
        ensure!(candles.len() >= 30, "{} K 线不足，无法计算 ATR 止损", pair);
        let atr = indicators::calculate(&candles)
            .last()
            .map_or(0.0, |x| x.atr);
        let stop = stop_distance(atr, price).context("ATR 无效，无法计算止损")?;
        let leverage = if futures {
            trade_leverage(
                leverage_cap(c.leverage, num(&ins["leverageFilter"]["maxLeverage"])),
                stop,
            )
        } else {
            1
        };
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
        let cap = entry_stake(budget, c.max_open - held.len(), min_stake)
            .context("可用预算预留 5% 后不足该交易对的最小下单要求")?;
        let base = c.capital.map_or(equity, |cap| cap.min(equity));
        let wanted = risk_stake(base, c.risk_per_trade_pct, stop, leverage);
        ensure!(
            wanted.is_finite() && wanted >= min_stake,
            "按每笔 {}% 风险、{:.1}% 止损计算的仓位低于交易所最小下单额",
            c.risk_per_trade_pct,
            stop * 100.0
        );
        let stake = wanted.min(cap);
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
            stop_loss_abs: price * if short { 1.0 + stop } else { 1.0 - stop },
            peak: price,
            enter_tag: tag.into(),
            exit_reason: None,
            entry_fee: 0.0,
            stop_dist: stop,
            funding: 0.0,
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
    /// Move the exchange-hosted stop. Bybit answers 34040 when the rounded price equals the
    /// current stop; that is already the wanted state, not a failure.
    async fn set_stop(&self, ex: &Bybit, t: &Trade, price: &str) -> Result<()> {
        match ex.post("/v5/position/trading-stop",json!({"category":"linear","symbol":symbol(&t.pair),"tpslMode":"Full","positionIdx":0,"stopLoss":price,"slTriggerBy":"LastPrice"})).await {
            Err(e) if !e.downcast_ref::<crate::exchange::Rejected>().is_some_and(|r| r.1 == 34040) => Err(e),
            _ => Ok(()),
        }
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
                self.set_stop(ex, t, &desired).await?;
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
                            // The bot's own partial closes are already settled into t.amount.
                            && !e["orderLinkId"]
                                .as_str()
                                .filter(|l| !l.is_empty())
                                .is_some_and(|l| self.store.has_order(l).unwrap_or(false))
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
            let result: Result<()> = async {
                if self.store.unresolved()?.iter().any(|o| o["pair"] == t.pair) {
                    return Ok(());
                }
                if t.market == "futures" && self.sync_external_exit(&c, &t, &ex).await? {
                    return Ok(());
                }
                let price = ex.price(&t.market, &t.pair).await?;
                let (peak, stop) = stop_price(&t, price);
                let previous = t.stop_loss_abs;
                // Persist first: if the exchange update fails, the next sync_external_exit
                // sees the difference and restores the stop from this record.
                t.peak = peak;
                t.stop_loss_abs = stop;
                self.store.update_trade(&t)?;
                if t.market == "futures" && stop != previous {
                    let ins = ex.instrument(&t.market, &t.pair).await?;
                    let tick = ins["priceFilter"]["tickSize"]
                        .as_str()
                        .context("缺少价格精度")?;
                    let desired = quantize(stop, tick)?;
                    // Moves smaller than one tick round to the current stop; nothing to send.
                    if desired != quantize(previous, tick)? {
                        self.set_stop(&ex, &t, &desired).await?;
                    }
                }
                if t.market == "spot" && price <= stop {
                    self.close_position(
                        t.trade_id,
                        if peak >= t.open_rate * (1.0 + trail_params(&t).0) {
                            "trailing_stop_loss"
                        } else {
                            "stop_loss"
                        },
                    )
                    .await?;
                }
                Ok(())
            }
            .await;
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
    fn atr_stop_stays_ahead_of_liquidation() {
        assert_eq!(stop_distance(0.2, 10.0), Some(0.05));
        assert_eq!(stop_distance(0.01, 10.0), Some(0.02));
        assert_eq!(stop_distance(1.0, 10.0), Some(0.10));
        assert_eq!(stop_distance(0.0, 10.0), None);
        assert_eq!(stop_distance(f64::NAN, 10.0), None);
        for max in 1..=25 {
            for stop in [0.02, 0.035, 0.05, 0.08, 0.10] {
                let lev = trade_leverage(max, stop);
                assert!(lev >= 1 && lev <= max);
                // The stop must be well inside the ~1/leverage liquidation distance.
                assert!(stop <= 0.6 / lev as f64 + 1e-12, "{} {}", max, stop);
            }
        }
        assert_eq!(trade_leverage(25, 0.05), 12);
        assert_eq!(trade_leverage(3, 0.05), 3);
        assert_eq!(trade_leverage(25, 0.10), 6);
        // A 2% stop wants 25×, but a 12.5× symbol caps it at 12 instead of refusing the entry.
        assert_eq!(trade_leverage(leverage_cap(25, 12.5), 0.02), 12);
        assert_eq!(leverage_cap(10, 100.0), 10);
        assert_eq!(leverage_cap(10, 0.0), 1);
        assert_eq!(leverage_cap(10, f64::NAN), 1);
    }

    #[test]
    fn risk_sizing_and_daily_loss_include_floating_losses() {
        // 1% of 1000 at a 5% stop is 200 notional: 200 margin at 1×, 40 at 5×.
        assert!((risk_stake(1000.0, 1.0, 0.05, 1) - 200.0).abs() < 1e-9);
        assert!((risk_stake(1000.0, 1.0, 0.05, 5) - 40.0).abs() < 1e-9);
        assert_eq!(daily_pnl(-1.0, &[-3.0, 2.0]), -4.0);
        assert_eq!(daily_pnl(0.0, &[5.0]), 0.0);
        let mut c = Config {
            running: true,
            ..Config::default()
        };
        c.ai.enabled = true;
        // Realized flat, but a 6% floating loss blocks new AI entries at the 5% limit.
        let pnl = daily_pnl(0.0, &[-6.0]);
        assert!(entry_guard(&c, &[], pnl, 94.0, "BTC/USDT", false, Some(0.9), true).is_err());
    }

    #[test]
    fn trailing_uses_the_trade_stop_distance() {
        let mut t = Trade {
            trade_id: 1,
            pair: "BTC/USDT".into(),
            market: "spot".into(),
            is_short: false,
            leverage: 1,
            amount: 1.0,
            open_rate: 100.0,
            stake_amount: 100.0,
            open_date: now(),
            close_rate: None,
            close_date: None,
            profit_abs: 0.0,
            profit_ratio: 0.0,
            stop_loss_abs: 95.0,
            peak: 100.0,
            enter_tag: "ai".into(),
            exit_reason: None,
            entry_fee: 0.0,
            stop_dist: 0.05,
            funding: 0.0,
        };
        assert_eq!(stop_price(&t, 104.0), (104.0, 95.0));
        let (peak, stop) = stop_price(&t, 110.0);
        assert_eq!(peak, 110.0);
        assert!((stop - 107.25).abs() < 1e-9);
        t.is_short = true;
        t.peak = 100.0;
        t.stop_loss_abs = 105.0;
        let (_, stop) = stop_price(&t, 90.0);
        assert!((stop - 92.25).abs() < 1e-9);
        // Legacy trades without a recorded distance keep 6% / 3%.
        t.is_short = false;
        t.stop_dist = 0.0;
        t.peak = 100.0;
        t.stop_loss_abs = 92.0;
        let (_, stop) = stop_price(&t, 106.0);
        assert!((stop - 102.82).abs() < 1e-9);
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
