use crate::{
    exchange::{category, quantize, symbol, Bybit, Fill},
    indicators,
    model::{day, now, num, AiConfig, Config, Trade},
    App,
};
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::sync::Arc;

/// What backs a spot position's stop on the exchange.
enum SpotStop {
    /// A conditional order is resting on Bybit.
    Hosted,
    /// The conditional order fired and the trade was settled.
    Settled,
    /// None could be maintained; `tick` watches the price itself.
    Absent,
}
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
    ensure!(
        c.max_same_direction == 0
            || held.iter().filter(|t| t.is_short == short).count() < c.max_same_direction,
        "同方向持仓已达上限"
    );
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
/// Share of the budget that may be committed as margin; the rest covers fees and slippage.
pub const USABLE: f64 = 0.95;
/// Funds new entries may draw on: what is free on the exchange, within the optional capital limit.
pub fn trade_budget(capital: Option<f64>, used: f64, free: f64) -> f64 {
    capital.map_or(free, |cap| (cap - used).max(0.0).min(free))
}
/// Margin for a new entry: the AI's requested amount lowered to what may be committed, or,
/// without a request, an even split over the slots that can still be afforded.
pub fn entry_margin(
    requested: Option<f64>,
    budget: f64,
    slots: usize,
    minimum: f64,
) -> Option<f64> {
    match requested {
        Some(m) if budget.is_finite() && budget > 0.0 => Some(m.min(budget * USABLE)),
        Some(_) => None,
        None => entry_stake(budget, slots, minimum),
    }
}
/// Position slots are an upper bound; reserve 5% and divide only among affordable slots.
pub fn entry_stake(budget: f64, slots: usize, minimum: f64) -> Option<f64> {
    if !budget.is_finite() || budget <= 0.0 || slots == 0 || !minimum.is_finite() || minimum < 0.0 {
        return None;
    }
    let usable = budget * USABLE;
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
/// Leverage for an entry: the AI's request (or the safety limit when it has none), kept between
/// the configured floor and `allowed`. When the stop's safety limit is below the floor, the safety
/// limit wins so the stop still triggers before liquidation.
pub fn entry_leverage(requested: Option<u32>, floor: u32, allowed: u32) -> u32 {
    let allowed = allowed.max(1);
    requested
        .unwrap_or(allowed)
        .clamp(floor.clamp(1, allowed), allowed)
}
/// Whole minutes since `open_date` (UTC, as stored), or None if it cannot be read.
pub fn held_minutes(open_date: &str, now: chrono::NaiveDateTime) -> Option<i64> {
    let open = chrono::NaiveDateTime::parse_from_str(open_date, "%Y-%m-%d %H:%M:%S").ok()?;
    Some((now - open).num_minutes())
}
/// The AI's own close waits for the minimum holding time. An unreadable open time does not
/// block it, so a damaged record can still be closed.
pub fn ai_close_guard(open_date: &str, now: chrono::NaiveDateTime) -> Result<()> {
    let min = crate::model::MIN_HOLD_BEFORE_AI_CLOSE_MINUTES;
    match held_minutes(open_date, now) {
        Some(m) if m < min => bail!(
            "开仓才 {} 分钟，不足 {} 分钟，AI 不能主动平仓；止损照常生效，可用 set_stops 收紧止损",
            m.max(0),
            min
        ),
        _ => Ok(()),
    }
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
/// Stop-loss and take-profit the AI asks for when opening, as percentages of the entry price.
#[derive(Clone, Default)]
pub struct Stops {
    pub stop_pct: Option<f64>,
    pub take_profit_pct: Option<f64>,
    /// Margin in USDT the AI commits to this entry; lowered to what may still be committed.
    /// The position is this margin times the leverage, and there is no per-trade loss cap.
    pub margin_usdt: Option<f64>,
    /// Requested leverage; lowered to the symbol limit and to what keeps the stop ahead of liquidation.
    pub leverage: Option<u32>,
    /// The AI's reason for opening, kept on the trade.
    pub reason: Option<String>,
}
/// Smallest tradable order value in USDT: the exchange minimum rounded up to a whole quantity step.
pub fn min_order_notional(ins: &Value, futures: bool, price: f64) -> Option<f64> {
    let filter = &ins["lotSizeFilter"];
    let step: f64 = filter[if futures { "qtyStep" } else { "basePrecision" }]
        .as_str()?
        .parse()
        .ok()?;
    if !(step.is_finite() && step > 0.0 && price.is_finite() && price > 0.0) {
        return None;
    }
    let min_value = num(&filter[if futures {
        "minNotionalValue"
    } else {
        "minOrderAmt"
    }]);
    let min_qty = (min_value / price)
        .max(num(&filter["minOrderQty"]))
        .max(step);
    Some((min_qty / step).ceil() * step * price)
}
/// An AI-chosen stop distance is kept between 2% and 10% so leverage can still keep it ahead of liquidation.
pub fn requested_stop(pct: f64) -> Result<f64> {
    ensure!(pct.is_finite() && pct > 0.0, "止损百分比无效");
    Ok((pct / 100.0).clamp(0.02, 0.10))
}
pub fn take_profit_from_pct(price: f64, pct: f64, short: bool) -> Result<f64> {
    ensure!(
        pct.is_finite() && pct > 0.0 && (!short || pct < 100.0),
        "止盈百分比无效"
    );
    Ok(price
        * if short {
            1.0 - pct / 100.0
        } else {
            1.0 + pct / 100.0
        })
}
/// Validate an AI stop update against the current price. The stop may only tighten; a take
/// profit of 0 removes it.
pub fn check_new_stops(t: &Trade, price: f64, stop: Option<f64>, tp: Option<f64>) -> Result<()> {
    ensure!(stop.is_some() || tp.is_some(), "缺少止损或止盈价格");
    if let Some(s) = stop {
        ensure!(s.is_finite() && s > 0.0, "止损价格无效");
        if t.is_short {
            ensure!(s > price, "空单止损价必须高于当前价");
            ensure!(s <= t.stop_loss_abs, "止损只能向有利方向移动");
        } else {
            ensure!(s < price, "多单止损价必须低于当前价");
            ensure!(s >= t.stop_loss_abs, "止损只能向有利方向移动");
        }
    }
    if let Some(p) = tp.filter(|p| *p != 0.0) {
        ensure!(p.is_finite() && p > 0.0, "止盈价格无效");
        ensure!(
            if t.is_short { p < price } else { p > price },
            "止盈价必须在当前价的盈利一侧"
        );
    }
    Ok(())
}
/// Safety net between AI runs: once price has moved one stop distance in favor, the stop trails
/// the best price by half of it. Trades opened before ATR stops keep a fixed 6% / 3% rule.
pub fn trail_params(t: &Trade) -> (f64, f64) {
    if t.stop_dist > 0.0 {
        (t.stop_dist, t.stop_dist / 2.0)
    } else {
        (0.06, 0.03)
    }
}
/// New best price and stop. The stop only ever moves in the trade's favor, so it never undoes a
/// tighter stop the AI chose.
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
/// Drawdown in percent from the high-water mark of Tide's own trading PnL. Deposits and
/// withdrawals move the wallet but not that PnL, so they never count as a loss. The amount lost
/// is measured against what the account would hold back at the high with the funds in it now,
/// so a loss after a withdrawal still counts in full against the smaller balance.
pub fn drawdown_pct(pnl: f64, peak: f64, equity: f64) -> Option<f64> {
    if !(pnl.is_finite() && peak.is_finite() && equity.is_finite()) {
        return None;
    }
    let lost = (peak - pnl).max(0.0);
    let base = equity + lost;
    (base > 0.0).then(|| lost / base * 100.0)
}
/// Account-level circuit breakers: drawdown of Tide's trading PnL from its high-water mark, and
/// a run of losing trades closed since the bot was last started. `recent` is newest first.
pub fn breaker(c: &Config, recent: &[Trade], drawdown: Option<f64>) -> Result<()> {
    let limit = c.ai.max_drawdown_pct;
    if let Some(d) = drawdown.filter(|_| limit > 0.0) {
        ensure!(
            d < limit,
            "交易回撤 {:.1}% 达到熔断线 {}%（按 Tide 交易盈亏计算，不含出入金），暂停新开仓",
            d,
            limit
        );
    }
    let n = c.ai.max_consecutive_losses as usize;
    ensure!(
        n == 0 || recent.len() < n || !recent[..n].iter().all(|t| t.profit_abs < 0.0),
        "连续亏损 {} 笔，暂停新开仓；检查后重新启动机器人即可恢复",
        n
    );
    Ok(())
}
/// Today's PnL for the daily loss limit: realized today plus every open position's floating loss.
pub fn daily_pnl(realized_today: f64, unrealized: &[f64]) -> f64 {
    realized_today + unrealized.iter().map(|u| u.min(0.0)).sum::<f64>()
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
            if t.take_profit_abs > 0.0 && t.open_rate > 0.0 {
                t.take_profit_abs *= fill.price / t.open_rate;
            }
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
            let saved = self.store.insert_trade(t.clone(), link)?;
            self.store.log_stop(
                saved.trade_id,
                &saved.pair,
                saved.stop_loss_abs,
                saved.take_profit_abs,
                "open",
            );
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
            // Closes the AI or the user asked for are already known; the rest wake the AI.
            if t.exit_reason.as_deref() != Some("force_exit") {
                self.push_hook(json!({"type":"position_closed","pair":t.pair,"side":if t.is_short{"short"}else{"long"},"reason":t.exit_reason,"profit_usdt":t.profit_abs}));
            }
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
        let (equity, free, pnl, _) = self.account_view(ex, held).await?;
        Ok((equity, free, pnl))
    }
    /// `account_risk` plus the open positions' total floating PnL, for the drawdown breaker.
    pub async fn account_view(&self, ex: &Bybit, held: &[Trade]) -> Result<(f64, f64, f64, f64)> {
        let (mut equity, free) = usdt(&ex.wallet().await?);
        let mut unrealized = vec![];
        for t in held {
            let price = ex.price(&t.market, &t.pair).await?;
            if t.market == "spot" {
                equity += t.amount * price;
            }
            unrealized.push(num(&t.mark(price)["profit_abs"]));
        }
        let pnl = daily_pnl(self.store.realized_on(&day())?, &unrealized);
        Ok((equity, free, pnl, unrealized.iter().sum()))
    }
    /// When the circuit breakers were last re-based (the bot's last start), set to now if never.
    /// Losses from before it existed must not block the first entry after an upgrade.
    pub fn breaker_since(&self) -> Result<String> {
        match self.store.state("breaker_since")?.filter(|s| !s.is_empty()) {
            Some(s) => Ok(s),
            None => {
                let started = now();
                self.store.set_state("breaker_since", &started)?;
                Ok(started)
            }
        }
    }
    /// Tide's trading PnL since the breakers were re-based (realized plus open positions), its
    /// high-water mark, and the drawdown from that mark. A missing mark starts at the current
    /// PnL, so nothing before it counts as a loss.
    pub fn track_drawdown(&self, equity: f64, open_pnl: f64) -> Result<Option<f64>> {
        let pnl = self.store.realized_since(&self.breaker_since()?)? + open_pnl;
        if !pnl.is_finite() {
            return Ok(None);
        }
        let stored = self
            .store
            .state("pnl_peak")?
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|p| p.is_finite());
        let peak = stored.map_or(pnl, |p| p.max(pnl));
        if stored != Some(peak) {
            self.store.set_state("pnl_peak", &peak.to_string())?;
        }
        Ok(drawdown_pct(pnl, peak, equity))
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
        stops: Stops,
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
        let (equity, free, pnl, open_pnl) = self.account_view(&ex, &held).await?;
        ensure!(
            self.store
                .state("risk_error")?
                .unwrap_or_default()
                .is_empty(),
            "持仓核对异常，暂停新开仓；请查看运行日志"
        );
        let ai = tag != "trend";
        entry_guard(&c, &held, pnl, equity, pair, short, confidence, ai)?;
        let drawdown = self.track_drawdown(equity, open_pnl)?;
        let since = self.breaker_since()?;
        breaker(
            &c,
            &self
                .store
                .closed_since(&since, c.ai.max_consecutive_losses as usize)?,
            drawdown,
        )?;
        let ins = ex.instrument(&c.market, pair).await?;
        let price = ex.price(&c.market, pair).await?;
        let used: f64 = held.iter().map(|t| t.stake_amount).sum();
        let budget = trade_budget(c.capital, used, free);
        let futures = c.market == "futures";
        let stop = match stops.stop_pct {
            Some(pct) => requested_stop(pct)?,
            None => {
                let candles = ex.candles(&c.market, pair, 240, 300, None).await?;
                ensure!(candles.len() >= 30, "{} K 线不足，无法计算 ATR 止损", pair);
                let atr = indicators::calculate(&candles)
                    .last()
                    .map_or(0.0, |x| x.atr);
                stop_distance(atr, price).context("ATR 无效，无法计算止损")?
            }
        };
        // Every entry gets a take profit; without an explicit one it is twice the stop distance.
        let take_profit =
            take_profit_from_pct(price, stops.take_profit_pct.unwrap_or(stop * 200.0), short)?;
        let leverage = if futures {
            let allowed = trade_leverage(
                leverage_cap(c.leverage, num(&ins["leverageFilter"]["maxLeverage"])),
                stop,
            );
            entry_leverage(stops.leverage, c.min_leverage, allowed)
        } else {
            1
        };
        if let Some(m) = stops.margin_usdt {
            ensure!(m.is_finite() && m > 0.0, "保证金金额无效");
        }
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
        let min_stake = min_order_notional(&ins, futures, price)
            .context("交易所价格或下单精度无效")?
            / leverage as f64;
        let stake = entry_margin(
            stops.margin_usdt,
            budget,
            c.max_open - held.len(),
            min_stake,
        )
        .context("可用预算预留 5% 后不足该交易对的最小下单要求")?;
        ensure!(
            stake.is_finite() && stake >= min_stake,
            "按 {:.2} USDT 保证金、{} 倍杠杆计算的仓位低于交易所最小下单额（至少需要 {:.2} USDT 保证金）",
            stake,
            leverage,
            min_stake
        );
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
            take_profit_abs: take_profit,
            peak: price,
            enter_tag: tag.into(),
            exit_reason: None,
            entry_fee: 0.0,
            stop_dist: stop,
            entry_reason: stops.reason.clone().unwrap_or_default(),
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
            if take_profit > 0.0 {
                body["takeProfit"] = json!(quantize(take_profit, tick)?);
                body["tpTriggerBy"] = json!("LastPrice");
                body["tpOrderType"] = json!("Market");
            }
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
        // The resting spot stop locks the coins; it has to go before the market sell.
        if t.market == "spot" {
            self.cancel_spot_stop(&ex, &t).await?;
        }
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
    /// Push the exchange-hosted stop and take profit ("0" removes the latter). Bybit answers
    /// 34040 when nothing changed; that is already the wanted state, not a failure.
    async fn set_stop(&self, ex: &Bybit, t: &Trade, stop: &str, take_profit: &str) -> Result<()> {
        match ex.post("/v5/position/trading-stop",json!({"category":"linear","symbol":symbol(&t.pair),"tpslMode":"Full","positionIdx":0,"stopLoss":stop,"slTriggerBy":"LastPrice","takeProfit":take_profit,"tpTriggerBy":"LastPrice"})).await {
            Err(e) if !e.downcast_ref::<crate::exchange::Rejected>().is_some_and(|r| r.1 == 34040) => Err(e),
            _ => Ok(()),
        }
    }
    /// AI-requested change of an open position's stop (tighten only) and take profit.
    pub async fn set_stops(&self, id: i64, stop: Option<f64>, tp: Option<f64>) -> Result<()> {
        let c = self.store.config()?;
        self.reconcile(&c).await?;
        let mut t = self
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
        let price = ex.price(&t.market, &t.pair).await?;
        check_new_stops(&t, price, stop, tp)?;
        if let Some(s) = stop {
            t.stop_loss_abs = s;
        }
        if let Some(p) = tp {
            t.take_profit_abs = p.max(0.0);
        }
        // Persist first: if the exchange update fails, sync_external_exit restores it.
        self.store.update_trade(&t)?;
        if t.market == "futures" {
            self.push_stops(&ex, &t).await?;
        }
        self.store.log_stop(
            t.trade_id,
            &t.pair,
            t.stop_loss_abs,
            t.take_profit_abs,
            "ai",
        );
        self.store.log(
            "INFO",
            &format!(
                "{} 止损 {} 止盈 {}",
                t.pair, t.stop_loss_abs, t.take_profit_abs
            ),
        );
        Ok(())
    }
    async fn push_stops(&self, ex: &Bybit, t: &Trade) -> Result<()> {
        let ins = ex.instrument(&t.market, &t.pair).await?;
        let tick = ins["priceFilter"]["tickSize"]
            .as_str()
            .context("缺少价格精度")?;
        let tp = if t.take_profit_abs > 0.0 {
            quantize(t.take_profit_abs, tick)?
        } else {
            "0".into()
        };
        self.set_stop(ex, t, &quantize(t.stop_loss_abs, tick)?, &tp)
            .await
    }
    /// Keep a conditional market sell on Bybit at the trade's stop, so a spot position stays
    /// protected while this process is down. Failures are logged and the price check in `tick`
    /// remains as the fallback.
    async fn sync_spot_stop(&self, ex: &Bybit, t: &Trade) -> SpotStop {
        match self.try_spot_stop(ex, t).await {
            Ok(state) => state,
            Err(e) => {
                self.store.log(
                    "WARNING",
                    &format!("{} 现货止损单维护失败，改由程序监控：{}", t.pair, e),
                );
                SpotStop::Absent
            }
        }
    }
    async fn try_spot_stop(&self, ex: &Bybit, t: &Trade) -> Result<SpotStop> {
        let key = format!("spot_stop:{}", t.trade_id);
        let ins = ex.instrument(&t.market, &t.pair).await?;
        let tick_size = ins["priceFilter"]["tickSize"]
            .as_str()
            .context("缺少价格精度")?;
        let step = ins["lotSizeFilter"]["basePrecision"]
            .as_str()
            .context("缺少下单精度")?;
        let trigger = quantize(t.stop_loss_abs, tick_size)?;
        if let Some(link) = self.store.state(&key)?.filter(|l| !l.is_empty()) {
            match ex.lookup_stop(&t.market, &t.pair, &link).await? {
                Some(o) if matches!(o["orderStatus"].as_str(), Some("Untriggered" | "New")) => {
                    if (num(&o["triggerPrice"]) - trigger.parse::<f64>()?).abs()
                        > tick_size.parse::<f64>()? * 0.5
                    {
                        ex.post("/v5/order/amend",json!({"category":"spot","symbol":symbol(&t.pair),"orderLinkId":link,"triggerPrice":trigger})).await?;
                    }
                    return Ok(SpotStop::Hosted);
                }
                Some(_) => match ex.confirm_stop(&t.market, &t.pair, &link).await? {
                    // Triggered but not final yet: keep waiting, do not sell a second time.
                    None => return Ok(SpotStop::Hosted),
                    Some(fill) if fill.qty > 0.0 => {
                        self.store.set_state(&key, "")?;
                        let order = json!({"link":link,"action":"exit","data":{"trade":t,"reason":"stop_loss_on_exchange","request":{"qty":fill.qty},"qty_step":step}});
                        self.settle(&order, fill).await?;
                        let _ = self
                            .telegram(&self.store.config()?, &format!("止损单成交 {}", t.pair))
                            .await;
                        return Ok(SpotStop::Settled);
                    }
                    Some(_) => self.store.set_state(&key, "")?,
                },
                None => self.store.set_state(&key, "")?,
            }
        }
        // Never stack a second stop: adopt one that is already resting (for example after the
        // saved link was lost) instead of creating another.
        if let Some(link) = ex
            .resting_stops(&t.pair)
            .await?
            .iter()
            .filter(|o| o["side"] == "Sell")
            .filter(|o| matches!(o["orderStatus"].as_str(), Some("Untriggered" | "New")))
            .find_map(|o| o["orderLinkId"].as_str().filter(|l| !l.is_empty()))
        {
            self.store.set_state(&key, link)?;
            return Ok(SpotStop::Hosted);
        }
        let link = format!("s{}", uuid::Uuid::new_v4().simple());
        let body = json!({"category":"spot","symbol":symbol(&t.pair),"side":"Sell","orderType":"Market","qty":quantize(t.amount,step)?,"marketUnit":"baseCoin","triggerPrice":trigger,"orderFilter":"StopOrder","orderLinkId":link});
        // Remember the link first: a lost response must not leave an untracked order behind.
        self.store.set_state(&key, &link)?;
        if let Err(e) = ex.post("/v5/order/create", body).await {
            if e.downcast_ref::<crate::exchange::Rejected>()
                .is_some_and(|e| e.definitive())
            {
                self.store.set_state(&key, "")?;
            }
            return Err(e);
        }
        Ok(SpotStop::Hosted)
    }
    /// Remove the resting stop before the position is sold. A stop that has already fired is
    /// left alone and the sale is refused until the system has settled it.
    async fn cancel_spot_stop(&self, ex: &Bybit, t: &Trade) -> Result<()> {
        let key = format!("spot_stop:{}", t.trade_id);
        let Some(link) = self.store.state(&key)?.filter(|l| !l.is_empty()) else {
            return Ok(());
        };
        match ex.lookup_stop(&t.market, &t.pair, &link).await? {
            None => {}
            Some(o) => match o["orderStatus"].as_str().unwrap_or("") {
                "Untriggered" | "New" => {
                    ex.post(
                        "/v5/order/cancel",
                        json!({"category":"spot","symbol":symbol(&t.pair),"orderLinkId":link}),
                    )
                    .await?;
                }
                "Cancelled" | "Deactivated" | "Rejected" => {}
                _ => bail!("{} 的止损单已触发，等待系统核对后再操作", t.pair),
            },
        }
        self.store.set_state(&key, "")
    }
    /// Once a minute, pause the bot if Tide's trading PnL has fallen past the drawdown limit.
    /// Existing positions keep their stops.
    async fn watchdog(&self, c: &Config, ex: &Bybit) -> Result<()> {
        if !c.running || c.ai.max_drawdown_pct <= 0.0 {
            return Ok(());
        }
        let nowts = chrono::Utc::now().timestamp();
        let last: i64 = self
            .store
            .state("watchdog_at")?
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if nowts - last < 60 {
            return Ok(());
        }
        self.store.set_state("watchdog_at", &nowts.to_string())?;
        let held = self.store.open_trades()?;
        let (equity, _, _, open_pnl) = self.account_view(ex, &held).await?;
        let drawdown = self.track_drawdown(equity, open_pnl)?;
        if let Err(e) = breaker(
            &Config {
                ai: AiConfig {
                    max_consecutive_losses: 0,
                    ..c.ai.clone()
                },
                ..c.clone()
            },
            &[],
            drawdown,
        ) {
            let mut paused = self.store.config()?;
            paused.running = false;
            self.store.save_config(&paused)?;
            self.store
                .log("ERROR", &format!("熔断，机器人已暂停：{}", e));
            let _ = self
                .telegram(c, &format!("熔断，机器人已暂停：{}", e))
                .await;
        }
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
            // Restore the exchange-hosted stop and take profit if either was removed or
            // differs from the record. Never rely only on the submission-time quote.
            let p = positions
                .iter()
                .find(|p| {
                    p["symbol"] == symbol(&t.pair)
                        && p["side"] == if t.is_short { "Sell" } else { "Buy" }
                })
                .context("缺少合约仓位")?;
            let ins = ex.instrument(&t.market, &t.pair).await?;
            let tick = num(&ins["priceFilter"]["tickSize"]);
            let want_sl: f64 = quantize(
                t.stop_loss_abs,
                ins["priceFilter"]["tickSize"]
                    .as_str()
                    .context("缺少价格精度")?,
            )?
            .parse()?;
            let want_tp: f64 = if t.take_profit_abs > 0.0 {
                quantize(
                    t.take_profit_abs,
                    ins["priceFilter"]["tickSize"]
                        .as_str()
                        .context("缺少价格精度")?,
                )?
                .parse()?
            } else {
                0.0
            };
            if (num(&p["stopLoss"]) - want_sl).abs() > tick * 0.5
                || (num(&p["takeProfit"]) - want_tp).abs() > tick * 0.5
            {
                self.push_stops(ex, t).await?;
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
        let exit_price = value / qty;
        let reason = if t.take_profit_abs > 0.0
            && (exit_price - t.take_profit_abs).abs() < (exit_price - t.stop_loss_abs).abs()
        {
            "take_profit_on_exchange"
        } else {
            "stoploss_on_exchange"
        };
        let order = json!({"link":link,"action":"exit","data":{"trade":t,"reason":reason}});
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
        let open = self.store.open_trades()?;
        let open_ids: Vec<i64> = open.iter().map(|t| t.trade_id).collect();
        for mut t in open {
            let result: Result<()> = async {
                if self.store.unresolved()?.iter().any(|o| o["pair"] == t.pair) {
                    return Ok(());
                }
                if t.market == "futures" && self.sync_external_exit(&c, &t, &ex).await? {
                    return Ok(());
                }
                let spot_stop = if t.market == "spot" {
                    match self.sync_spot_stop(&ex, &t).await {
                        SpotStop::Settled => return Ok(()),
                        state => Some(state),
                    }
                } else {
                    None
                };
                let price = ex.price(&t.market, &t.pair).await?;
                let events = self
                    .hooks
                    .lock()
                    .unwrap()
                    .position_events(&c.ai.hooks, &t, price);
                for e in events {
                    self.push_hook(e);
                }
                let (peak, stop) = stop_price(&t, price);
                let previous = t.stop_loss_abs;
                // Persist first: if the exchange update fails, the next sync_external_exit
                // sees the difference and restores the stop from this record.
                if peak != t.peak || stop != previous {
                    t.peak = peak;
                    t.stop_loss_abs = stop;
                    self.store.update_trade(&t)?;
                }
                if stop != previous {
                    self.store
                        .log_stop(t.trade_id, &t.pair, stop, t.take_profit_abs, "trail");
                    if t.market == "futures" {
                        let ins = ex.instrument(&t.market, &t.pair).await?;
                        let tick = ins["priceFilter"]["tickSize"]
                            .as_str()
                            .context("缺少价格精度")?;
                        // Moves smaller than one tick round to the current stop; nothing to send.
                        if quantize(stop, tick)? != quantize(previous, tick)? {
                            self.push_stops(&ex, &t).await?;
                        }
                    }
                }
                if t.market == "spot" {
                    // A resting exchange stop does the stop-loss; the price check backs it up.
                    let hosted = matches!(spot_stop, Some(SpotStop::Hosted));
                    let reason = if !hosted && price <= t.stop_loss_abs {
                        Some("stop_loss")
                    } else if t.take_profit_abs > 0.0 && price >= t.take_profit_abs {
                        Some("take_profit")
                    } else {
                        None
                    };
                    if let Some(reason) = reason {
                        self.close_position(t.trade_id, reason).await?;
                    }
                }
                Ok(())
            }
            .await;
            if let Err(e) = result {
                risk_errors.push(e.to_string());
            }
        }
        self.hooks.lock().unwrap().forget_closed(&open_ids);
        if let Err(e) = self.watchdog(&c, &ex).await {
            self.store.log("WARNING", &format!("回撤检查失败：{}", e));
        }
        self.store
            .set_state("risk_error", &risk_errors.join("；"))?;
        ensure!(
            risk_errors.is_empty(),
            "持仓风控异常：{}",
            risk_errors.join("；")
        );
        // AI is the only automatic entry and discretionary exit mode. The loop
        // above remains responsible for the stop-loss and take-profit set by the AI
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
    fn ai_closes_wait_for_the_minimum_holding_time() {
        let at = |s: &str| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap();
        let open = "2026-11-04 00:37:00";
        assert_eq!(held_minutes(open, at("2026-11-04 00:45:30")), Some(8));
        assert!(ai_close_guard(open, at("2026-11-04 00:45:30")).is_err());
        assert!(ai_close_guard(open, at("2026-11-04 01:36:59")).is_err());
        assert!(ai_close_guard(open, at("2026-11-04 01:37:00")).is_ok());
        // A clock behind the open time still waits; an unreadable time does not block.
        assert!(ai_close_guard(open, at("2026-11-04 00:30:00")).is_err());
        assert!(ai_close_guard("", at("2026-11-04 00:30:00")).is_ok());
    }

    #[test]
    fn leverage_stays_between_the_floor_and_the_safety_limit() {
        // A request inside the range is kept; below the floor it is raised, above it lowered.
        assert_eq!(entry_leverage(Some(5), 3, 10), 5);
        assert_eq!(entry_leverage(Some(2), 3, 10), 3);
        assert_eq!(entry_leverage(Some(20), 3, 10), 10);
        // No request: the safety limit.
        assert_eq!(entry_leverage(None, 3, 10), 10);
        // A wide stop's safety limit below the floor wins over the floor.
        assert_eq!(entry_leverage(Some(2), 10, 7), 7);
        assert_eq!(entry_leverage(Some(1), 1, 7), 1);
        assert_eq!(entry_leverage(Some(0), 0, 0), 1);
    }

    #[test]
    fn ai_margin_is_capped_by_the_budget_and_daily_loss_includes_floating_losses() {
        // The AI's margin is used as asked, and lowered to 95% of what is free.
        assert_eq!(entry_margin(Some(30.0), 100.0, 5, 5.0), Some(30.0));
        assert_eq!(entry_margin(Some(500.0), 100.0, 5, 5.0), Some(95.0));
        assert_eq!(entry_margin(Some(30.0), 0.0, 5, 5.0), None);
        // Without a request the budget is split over the affordable slots.
        assert_eq!(entry_margin(None, 100.0, 5, 5.0), Some(19.0));
        // A capital limit counts the margin already in use.
        assert_eq!(trade_budget(None, 40.0, 60.0), 60.0);
        assert_eq!(trade_budget(Some(50.0), 40.0, 60.0), 10.0);
        assert_eq!(trade_budget(Some(30.0), 40.0, 60.0), 0.0);
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
    fn ai_stops_are_clamped_and_only_tighten() {
        assert_eq!(requested_stop(0.5).unwrap(), 0.02);
        assert_eq!(requested_stop(4.0).unwrap(), 0.04);
        assert_eq!(requested_stop(30.0).unwrap(), 0.10);
        assert!(requested_stop(0.0).is_err() && requested_stop(f64::NAN).is_err());
        assert!((take_profit_from_pct(100.0, 8.0, false).unwrap() - 108.0).abs() < 1e-9);
        assert!((take_profit_from_pct(100.0, 8.0, true).unwrap() - 92.0).abs() < 1e-9);
        assert!(take_profit_from_pct(100.0, 100.0, true).is_err());
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
            take_profit_abs: 0.0,
            peak: 100.0,
            enter_tag: "ai".into(),
            exit_reason: None,
            entry_fee: 0.0,
            stop_dist: 0.05,
            entry_reason: String::new(),
            funding: 0.0,
        };
        assert!(check_new_stops(&t, 110.0, Some(105.0), Some(120.0)).is_ok());
        assert!(check_new_stops(&t, 110.0, Some(95.0), None).is_ok());
        assert!(check_new_stops(&t, 110.0, Some(94.0), None).is_err());
        assert!(check_new_stops(&t, 110.0, Some(111.0), None).is_err());
        assert!(check_new_stops(&t, 110.0, None, Some(105.0)).is_err());
        assert!(check_new_stops(&t, 110.0, None, Some(0.0)).is_ok());
        assert!(check_new_stops(&t, 110.0, None, None).is_err());
        t.is_short = true;
        t.stop_loss_abs = 105.0;
        assert!(check_new_stops(&t, 90.0, Some(100.0), Some(80.0)).is_ok());
        assert!(check_new_stops(&t, 90.0, Some(106.0), None).is_err());
        assert!(check_new_stops(&t, 90.0, Some(89.0), None).is_err());
        assert!(check_new_stops(&t, 90.0, None, Some(95.0)).is_err());
    }

    fn sample(short: bool, profit: f64) -> Trade {
        Trade {
            trade_id: 1,
            pair: "BTC/USDT".into(),
            market: "spot".into(),
            is_short: short,
            leverage: 1,
            amount: 1.0,
            open_rate: 100.0,
            stake_amount: 100.0,
            open_date: now(),
            close_rate: None,
            close_date: None,
            profit_abs: profit,
            profit_ratio: 0.0,
            stop_loss_abs: 95.0,
            take_profit_abs: 0.0,
            peak: 100.0,
            enter_tag: "ai".into(),
            exit_reason: None,
            entry_fee: 0.0,
            stop_dist: 0.05,
            entry_reason: String::new(),
            funding: 0.0,
        }
    }
    #[test]
    fn trailing_floor_never_loosens_a_stop() {
        let t = sample(false, 0.0);
        assert_eq!(stop_price(&t, 104.0), (104.0, 95.0));
        let (peak, stop) = stop_price(&t, 110.0);
        assert_eq!(peak, 110.0);
        assert!((stop - 107.25).abs() < 1e-9);
        // A tighter stop chosen by the AI stays when it is already above the trail.
        let mut tight = sample(false, 0.0);
        tight.stop_loss_abs = 109.0;
        assert_eq!(stop_price(&tight, 110.0).1, 109.0);
        let mut short = sample(true, 0.0);
        short.stop_loss_abs = 105.0;
        let (_, stop) = stop_price(&short, 90.0);
        assert!((stop - 92.25).abs() < 1e-9);
        let mut legacy = sample(false, 0.0);
        legacy.stop_dist = 0.0;
        legacy.stop_loss_abs = 92.0;
        assert!((stop_price(&legacy, 106.0).1 - 102.82).abs() < 1e-9);
    }

    #[test]
    fn breakers_stop_new_entries_after_drawdown_or_a_losing_streak() {
        let mut c = Config::default();
        c.ai.max_drawdown_pct = 20.0;
        c.ai.max_consecutive_losses = 3;
        assert!(breaker(&c, &[], Some(0.0)).is_ok());
        assert!(breaker(&c, &[], Some(19.0)).is_ok());
        assert!(breaker(&c, &[], Some(20.0)).is_err());
        assert!(breaker(&c, &[], None).is_ok());
        let loss = sample(false, -1.0);
        let win = sample(false, 2.0);
        assert!(breaker(&c, &[loss.clone(), loss.clone()], Some(0.0)).is_ok());
        assert!(breaker(&c, &[loss.clone(), win.clone(), loss.clone()], Some(0.0)).is_ok());
        assert!(breaker(&c, &[loss.clone(), loss.clone(), loss.clone()], Some(0.0)).is_err());
        c.ai.max_consecutive_losses = 0;
        c.ai.max_drawdown_pct = 0.0;
        assert!(breaker(&c, &[loss.clone(), loss.clone(), loss], Some(99.0)).is_ok());
    }

    #[test]
    fn drawdown_follows_trading_pnl_not_deposits_or_withdrawals() {
        // At the high: nothing lost.
        assert_eq!(drawdown_pct(5.0, 5.0, 100.0), Some(0.0));
        // Withdrawing 70 of 90 leaves the trading PnL alone: still no drawdown (the 11-06
        // false trigger saw 92.7% here).
        assert_eq!(drawdown_pct(0.0, 0.0, 20.0), Some(0.0));
        // A deposit does not hide a loss either: 10 lost, now 190 in the account → 10 / 200.
        assert_eq!(drawdown_pct(-10.0, 0.0, 190.0), Some(5.0));
        // After a withdrawal a loss counts against the smaller balance: 4 lost, 16 left → 20%.
        assert_eq!(drawdown_pct(-4.0, 0.0, 16.0), Some(20.0));
        // Profit above the mark is not a drawdown; an empty account has none to measure.
        assert_eq!(drawdown_pct(8.0, 5.0, 100.0), Some(0.0));
        assert_eq!(drawdown_pct(0.0, 0.0, 0.0), None);
        assert_eq!(drawdown_pct(f64::NAN, 0.0, 10.0), None);
    }

    #[test]
    fn minimum_order_is_rounded_up_to_a_whole_step() {
        let ins = json!({"lotSizeFilter":{"qtyStep":"0.001","minNotionalValue":"5","minOrderQty":"0.001"}});
        // 5 USDT is only 0.0000653 BTC, so the 0.001 BTC minimum quantity decides.
        assert!((min_order_notional(&ins, true, 76455.0).unwrap() - 76.455).abs() < 1e-6);
        let cheap =
            json!({"lotSizeFilter":{"qtyStep":"0.1","minNotionalValue":"5","minOrderQty":"0.1"}});
        assert!((min_order_notional(&cheap, true, 3.7).unwrap() - 5.18).abs() < 1e-9);
        assert!(min_order_notional(&json!({}), true, 1.0).is_none());
        assert!(min_order_notional(&ins, true, 0.0).is_none());
    }

    #[test]
    fn same_direction_positions_are_capped() {
        let mut c = Config {
            running: true,
            pairs: vec!["A/USDT".into(), "B/USDT".into(), "C/USDT".into()],
            market: "futures".into(),
            short: true,
            max_same_direction: 2,
            ..Config::default()
        };
        let mut a = sample(false, 0.0);
        a.pair = "A/USDT".into();
        let mut b = sample(false, 0.0);
        b.pair = "B/USDT".into();
        let held = [a, b];
        assert!(entry_guard(&c, &held, 0.0, 100.0, "C/USDT", false, None, false).is_err());
        assert!(entry_guard(&c, &held, 0.0, 100.0, "C/USDT", true, None, false).is_ok());
        c.max_same_direction = 0;
        assert!(entry_guard(&c, &held, 0.0, 100.0, "C/USDT", false, None, false).is_ok());
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
