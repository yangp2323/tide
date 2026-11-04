use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Candle {
    pub time: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Indicator {
    pub ema20: f64,
    pub ema50: f64,
    pub ema200: f64,
    pub adx: f64,
    pub rsi: f64,
    pub atr: f64,
    pub enter_long: bool,
    pub enter_short: bool,
    pub exit_long: bool,
    pub exit_short: bool,
}
fn ema(xs: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![0.0; xs.len()];
    if xs.len() < n {
        return out;
    }
    out[n - 1] = xs[..n].iter().sum::<f64>() / n as f64;
    for i in n..xs.len() {
        out[i] = out[i - 1] + 2.0 / (n as f64 + 1.0) * (xs[i] - out[i - 1]);
    }
    out
}
pub fn calculate(cs: &[Candle]) -> Vec<Indicator> {
    let close: Vec<_> = cs.iter().map(|c| c.close).collect();
    let e20 = ema(&close, 20);
    let e50 = ema(&close, 50);
    let e200 = ema(&close, 200);
    let mut out = Vec::new();
    let (mut tr, mut plus, mut minus, mut gain, mut loss, mut adx) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    let mut dxs = Vec::new();
    for (i, c) in cs.iter().enumerate() {
        if i > 0 {
            let prev = &cs[i - 1];
            let up = c.high - prev.high;
            let down = prev.low - c.low;
            let vtr = (c.high - c.low)
                .max((c.high - prev.close).abs())
                .max((c.low - prev.close).abs());
            let p = if up > down && up > 0.0 { up } else { 0.0 };
            let m = if down > up && down > 0.0 { down } else { 0.0 };
            let diff = c.close - prev.close;
            if i <= 14 {
                tr += vtr;
                plus += p;
                minus += m;
                gain += diff.max(0.0);
                loss += (-diff).max(0.0);
            } else {
                tr = tr - tr / 14.0 + vtr;
                plus = plus - plus / 14.0 + p;
                minus = minus - minus / 14.0 + m;
                gain = gain - gain / 14.0 + diff.max(0.0);
                loss = loss - loss / 14.0 + (-diff).max(0.0);
            }
            if i >= 14 {
                let dx = if plus + minus > 0.0 {
                    100.0 * (plus - minus).abs() / (plus + minus)
                } else {
                    0.0
                };
                if i < 28 {
                    dxs.push(dx);
                    if i == 27 {
                        adx = dxs.iter().sum::<f64>() / 14.0;
                    }
                } else {
                    adx = (adx * 13.0 + dx) / 14.0;
                }
            }
        }
        let ready = i >= 219;
        let cross_up = i > 0 && e20[i] > e50[i] && e20[i - 1] <= e50[i - 1];
        let cross_down = i > 0 && e20[i] < e50[i] && e20[i - 1] >= e50[i - 1];
        out.push(Indicator {
            ema20: e20[i],
            ema50: e50[i],
            ema200: e200[i],
            adx,
            rsi: if gain + loss > 0.0 {
                gain / (gain + loss) * 100.0
            } else {
                0.0
            },
            atr: tr / 14.0,
            enter_long: ready && c.volume > 0.0 && c.close > e200[i] && adx > 20.0 && cross_up,
            enter_short: ready && c.volume > 0.0 && c.close < e200[i] && adx > 20.0 && cross_down,
            exit_long: ready && c.volume > 0.0 && cross_down,
            exit_short: ready && c.volume > 0.0 && cross_up,
        });
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn increasing_series_has_expected_indicators() {
        let cs: Vec<_> = (0..300)
            .map(|i| Candle {
                time: i,
                open: 100.0 + i as f64,
                high: 102.0 + i as f64,
                low: 99.0 + i as f64,
                close: 101.0 + i as f64,
                volume: 1.0,
            })
            .collect();
        let xs = calculate(&cs);
        let x = xs.last().unwrap();
        assert!((x.adx - 100.0).abs() < 0.001);
        assert!((x.rsi - 100.0).abs() < 0.001);
        assert!(x.ema20 > x.ema50 && x.ema50 > x.ema200);
        assert!(!x.enter_long);
    }
}
