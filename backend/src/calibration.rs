//! Forecast calibration and sizing. The AI states, for each entry, how likely the trade is to
//! close in profit. Closed trades show how well those forecasts held up, and entries are gated
//! and sized by the calibrated forecast rather than by the model's word alone.
//!
//! Outcomes are measured in R: the price move from entry to exit over the initial stop distance,
//! so a full stop-out is about -1R whatever the position size or leverage.
use crate::model::Trade;
use serde_json::{json, Value};

/// How many trades the AI's own forecast counts for against observed results. With few closed
/// trades the forecast dominates; as results accumulate they take over.
pub const PRIOR_TRADES: f64 = 10.0;
/// Taker fee per side assumed when judging an entry before it fills (Bybit standard tier).
pub const TAKER_FEE: f64 = 0.00055;
/// Exit reason of a position the operator closed by hand on the exchange.
pub const EXTERNAL_CLOSE: &str = "external_close";
/// Forecast ranges the calibration table reports on. The last bound is open so 1.0 fits.
const BUCKETS: [(f64, f64); 5] = [(0.0, 0.4), (0.4, 0.5), (0.5, 0.6), (0.6, 0.7), (0.7, 1.01)];

/// A closed trade's result in R: its price move over the initial stop distance.
pub fn realized_r(t: &Trade) -> Option<f64> {
    let close = t.close_rate?;
    if !(t.stop_dist > 0.0 && t.open_rate > 0.0 && close.is_finite()) {
        return None;
    }
    let side = if t.is_short { -1.0 } else { 1.0 };
    Some((close - t.open_rate) / t.open_rate * side / t.stop_dist)
}

#[derive(Default, Clone)]
struct Bucket {
    n: usize,
    wins: usize,
    forecast: f64,
}

/// What the closed trades with a forecast say about the AI's forecasts and payoffs.
#[derive(Default)]
pub struct Calibration {
    buckets: [Bucket; 5],
    n: usize,
    wins: usize,
    brier: f64,
    win_r: Vec<f64>,
    loss_r: Vec<f64>,
}

fn bucket_of(p: f64) -> usize {
    BUCKETS
        .iter()
        .position(|(lo, hi)| p >= *lo && p < *hi)
        .unwrap_or(BUCKETS.len() - 1)
}

impl Calibration {
    /// Closed trades that carry a forecast; a win is a trade that closed with a net profit.
    /// A position the operator closed by hand on the exchange never reached the outcome the
    /// forecast was about, so it does not count.
    pub fn from_trades(trades: &[Trade]) -> Self {
        let mut c = Self::default();
        for t in trades
            .iter()
            .filter(|t| t.close_date.is_some() && t.exit_reason.as_deref() != Some(EXTERNAL_CLOSE))
        {
            let Some(p) = t.win_prob.filter(|p| (0.0..=1.0).contains(p)) else {
                continue;
            };
            let win = t.profit_abs > 0.0;
            let b = &mut c.buckets[bucket_of(p)];
            b.n += 1;
            b.wins += win as usize;
            b.forecast += p;
            c.n += 1;
            c.wins += win as usize;
            c.brier += (p - if win { 1.0 } else { 0.0 }).powi(2);
            if let Some(r) = realized_r(t) {
                if win {
                    c.win_r.push(r);
                } else {
                    c.loss_r.push(r);
                }
            }
        }
        c
    }
    /// The forecast moved toward how trades forecast in the same range actually ended.
    pub fn calibrated(&self, p: f64) -> f64 {
        let b = &self.buckets[bucket_of(p)];
        (b.wins as f64 + PRIOR_TRADES * p) / (b.n as f64 + PRIOR_TRADES)
    }
    /// Average win over average loss in R, moved from the planned reward-to-risk toward what
    /// closed trades achieved (exits often come before the take profit or the full stop).
    pub fn payoff(&self, planned: f64) -> f64 {
        let avg = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        if self.win_r.is_empty() || self.loss_r.is_empty() {
            return planned;
        }
        let loss = -avg(&self.loss_r);
        // Losers that closed near the entry give no loss size to compare wins against.
        if loss.is_nan() || loss <= 0.0 {
            return planned;
        }
        let observed = avg(&self.win_r) / loss;
        let n = (self.win_r.len() + self.loss_r.len()) as f64;
        (n * observed + PRIOR_TRADES * planned) / (n + PRIOR_TRADES)
    }
    pub fn to_json(&self) -> Value {
        let avg = |v: &[f64]| {
            if v.is_empty() {
                Value::Null
            } else {
                json!(v.iter().sum::<f64>() / v.len() as f64)
            }
        };
        json!({
            "trades_with_forecast":self.n,
            "realized_win_rate":if self.n == 0 {Value::Null} else {json!(self.wins as f64 / self.n as f64)},
            "brier_score":if self.n == 0 {Value::Null} else {json!(self.brier / self.n as f64)},
            "avg_win_r":avg(&self.win_r),
            "avg_loss_r":avg(&self.loss_r),
            "by_forecast":BUCKETS.iter().zip(&self.buckets).map(|((lo, hi), b)| json!({
                "forecast_range":format!("{:.0}%-{:.0}%", lo * 100.0, hi.min(1.0) * 100.0),
                "trades":b.n,
                "avg_forecast":if b.n == 0 {Value::Null} else {json!(b.forecast / b.n as f64)},
                "realized_win_rate":if b.n == 0 {Value::Null} else {json!(b.wins as f64 / b.n as f64)},
            })).collect::<Vec<_>>(),
        })
    }
}

/// The calibrated edge of one entry.
#[derive(Debug, Clone)]
pub struct Edge {
    pub forecast: f64,
    pub win_prob: f64,
    pub payoff: f64,
    pub fee_r: f64,
    /// Expected result in R after fees.
    pub ev_r: f64,
    /// Kelly fraction of equity to lose at the stop (0 when there is no edge).
    pub kelly: f64,
}

/// The entry's edge from forecast `p`, planned reward-to-risk `rr` and stop distance `stop`.
pub fn edge(cal: &Calibration, p: f64, rr: f64, stop: f64) -> Edge {
    let win_prob = cal.calibrated(p);
    let payoff = cal.payoff(rr);
    let fee_r = 2.0 * TAKER_FEE / stop;
    let ev_r = win_prob * payoff - (1.0 - win_prob) - fee_r;
    // Kelly for a bet that wins `payoff` or loses 1: expected gain over the win size.
    let kelly = if payoff > 0.0 {
        (ev_r / payoff).max(0.0)
    } else {
        0.0
    };
    Edge {
        forecast: p,
        win_prob,
        payoff,
        fee_r,
        ev_r,
        kelly,
    }
}

/// Suggested loss at the stop as a fraction of equity: a share of Kelly, capped.
pub fn suggested_risk(e: &Edge, fraction: f64, cap_pct: f64) -> f64 {
    (e.kelly * fraction).min(cap_pct / 100.0).max(0.0)
}

impl Edge {
    pub fn to_json(&self) -> Value {
        json!({"forecast":self.forecast,"calibrated_win_prob":self.win_prob,"payoff_r":self.payoff,"fee_r":self.fee_r,"expected_r":self.ev_r,"kelly":self.kelly})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closed(p: f64, profit: f64, open: f64, close: f64, stop: f64) -> Trade {
        let mut t: Trade = serde_json::from_value(json!({
            "trade_id":1,"pair":"BTC/USDT:USDT","market":"futures","is_short":false,"leverage":5,
            "amount":1.0,"open_rate":open,"stake_amount":10.0,"open_date":"2026-11-01 00:00:00",
            "close_rate":close,"close_date":"2026-11-01 04:00:00","profit_abs":profit,"profit_ratio":0.0,
            "stop_loss_abs":open * (1.0 - stop),"peak":open,"enter_tag":"ai","exit_reason":"stoploss_on_exchange",
            "stop_dist":stop
        }))
        .unwrap();
        t.win_prob = Some(p);
        t
    }

    #[test]
    fn results_are_measured_in_r_of_the_initial_stop() {
        let t = closed(0.6, 1.0, 100.0, 104.0, 0.02);
        assert!((realized_r(&t).unwrap() - 2.0).abs() < 1e-9);
        let mut s = closed(0.6, -1.0, 100.0, 102.0, 0.02);
        s.is_short = true;
        assert!((realized_r(&s).unwrap() + 1.0).abs() < 1e-9);
        s.stop_dist = 0.0;
        assert!(realized_r(&s).is_none());
    }

    #[test]
    fn forecasts_move_toward_what_the_same_range_achieved() {
        // No history: the forecast stands, and the plan's reward-to-risk too.
        let empty = Calibration::from_trades(&[]);
        assert!((empty.calibrated(0.6) - 0.6).abs() < 1e-12);
        assert!((empty.payoff(2.0) - 2.0).abs() < 1e-12);
        // Ten trades forecast at 60% won only three: a 60% forecast is now worth 45%.
        let mut trades = vec![];
        for i in 0..10 {
            let win = i < 3;
            trades.push(if win {
                closed(0.6, 1.0, 100.0, 102.0, 0.02)
            } else {
                closed(0.6, -1.0, 100.0, 98.0, 0.02)
            });
        }
        let cal = Calibration::from_trades(&trades);
        assert!((cal.calibrated(0.65) - (3.0 + 10.0 * 0.65) / 20.0).abs() < 1e-12);
        // Another range has no trades and keeps the forecast.
        assert!((cal.calibrated(0.45) - 0.45).abs() < 1e-12);
        // Wins averaged +1R and losses -1R: a planned 2R payoff is pulled halfway to 1.
        assert!((cal.payoff(2.0) - 1.5).abs() < 1e-12);
        let j = cal.to_json();
        assert_eq!(j["trades_with_forecast"], 10);
        assert_eq!(j["by_forecast"][3]["trades"], 10);
        assert!((j["realized_win_rate"].as_f64().unwrap() - 0.3).abs() < 1e-12);
        // Open trades and trades without a forecast do not count.
        let mut open = closed(0.9, 0.0, 100.0, 100.0, 0.02);
        open.close_date = None;
        let mut silent = closed(0.9, 1.0, 100.0, 102.0, 0.02);
        silent.win_prob = None;
        // Nor do positions the operator closed by hand.
        let mut by_hand = closed(0.9, -1.0, 100.0, 99.0, 0.02);
        by_hand.exit_reason = Some(EXTERNAL_CLOSE.into());
        assert_eq!(Calibration::from_trades(&[open, silent, by_hand]).n, 0);
    }

    #[test]
    fn edge_gates_on_expected_r_and_kelly_sizes_by_it() {
        let cal = Calibration::from_trades(&[]);
        // 55% to win 2R against 1R, fees 0.055R at a 2% stop: about +0.59R.
        let e = edge(&cal, 0.55, 2.0, 0.02);
        assert!((e.fee_r - 0.055).abs() < 1e-12);
        assert!((e.ev_r - (0.55 * 2.0 - 0.45 - 0.055)).abs() < 1e-12);
        assert!((e.kelly - e.ev_r / 2.0).abs() < 1e-12);
        // Half Kelly is about 14.9% of equity; the 5% cap applies.
        assert!((suggested_risk(&e, 0.5, 5.0) - 0.05).abs() < 1e-12);
        assert!((suggested_risk(&e, 0.1, 5.0) - e.kelly * 0.1).abs() < 1e-12);
        // A coin flip at 1:1 loses the fees: no edge, no size.
        let flat = edge(&cal, 0.5, 1.0, 0.02);
        assert!(flat.ev_r < 0.0 && flat.kelly == 0.0);
        assert_eq!(suggested_risk(&flat, 0.5, 5.0), 0.0);
    }
}
