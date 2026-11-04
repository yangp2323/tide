use crate::{
    model::{num, Config, Trade},
    store::{atomic_json, Store},
};
use anyhow::{ensure, Result};
use serde_json::{json, Value};
use std::path::Path;

/// Read-only import. Never adopts unverified live orders or positions.
pub fn run(store: &Store, old: &Path) -> Result<()> {
    ensure!(
        !store.dir.join("config.json").exists(),
        "目标已有配置，禁止重复迁移"
    );
    let cfg: Value = serde_json::from_slice(&std::fs::read(old.join("config.json"))?)?;
    let private: Value = serde_json::from_slice(&std::fs::read(old.join("config-private.json"))?)?;
    let mut imported = vec![];
    for entry in std::fs::read_dir(old)? {
        let path = entry?.path();
        let name = path.file_name().unwrap().to_string_lossy();
        if name != "tradesv3.live.sqlite" && name != "tradesv3.live.futures.sqlite" {
            continue;
        }
        let db = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let open: i64 = db.query_row("SELECT count(*) FROM trades WHERE is_open=1", [], |r| {
            r.get(0)
        })?;
        ensure!(
            open == 0,
            "{} 中有 {} 笔未平仓交易，迁移前必须核对并处理",
            name,
            open
        );
        let mut q = db.prepare("SELECT * FROM trades ORDER BY id")?;
        let names: Vec<String> = q.column_names().iter().map(|s| s.to_string()).collect();
        let mut rows = q.query([])?;
        while let Some(row) = rows.next()? {
            let mut v = json!({});
            for (i, name) in names.iter().enumerate() {
                use rusqlite::types::ValueRef;
                v[name] = match row.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(n) => json!(n),
                    ValueRef::Real(n) => json!(n),
                    ValueRef::Text(s) => json!(String::from_utf8_lossy(s)),
                    _ => Value::Null,
                };
            }
            if v["close_date"].is_null() {
                continue;
            }
            let lev = num(&v["leverage"]).max(1.0) as u32;
            let price = num(&v["open_rate"]);
            imported.push(Trade {
                trade_id: 0,
                pair: v["pair"].as_str().unwrap_or("").into(),
                market: if name.contains("futures") {
                    "futures"
                } else {
                    "spot"
                }
                .into(),
                is_short: num(&v["is_short"]) != 0.0,
                leverage: lev,
                amount: num(&v["amount"]),
                open_rate: price,
                stake_amount: num(&v["stake_amount"]),
                open_date: v["open_date"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(19)
                    .collect(),
                close_rate: Some(num(&v["close_rate"])),
                close_date: Some(
                    v["close_date"]
                        .as_str()
                        .unwrap_or("")
                        .chars()
                        .take(19)
                        .collect(),
                ),
                profit_abs: num(&v["close_profit_abs"]),
                profit_ratio: num(&v["close_profit"]),
                stop_loss_abs: num(&v["stop_loss"]),
                take_profit_abs: 0.0,
                peak: price,
                enter_tag: v["enter_tag"].as_str().unwrap_or("trend").into(),
                exit_reason: v["exit_reason"].as_str().map(str::to_string),
                entry_fee: 0.0,
                stop_dist: 0.0,
                funding: 0.0,
            });
        }
    }
    let mut c = Config::default();
    c.market = cfg["trading_mode"].as_str().unwrap_or("spot").into();
    c.pairs = serde_json::from_value(cfg["exchange"]["pair_whitelist"].clone())?;
    c.max_open = cfg["max_open_trades"].as_u64().unwrap_or(3) as usize;
    c.leverage = if c.market == "futures" {
        cfg["panel_leverage"].as_u64().unwrap_or(1) as u32
    } else {
        1
    };
    c.short = c.market == "futures" && cfg["panel_short"].as_bool().unwrap_or(false);
    c.capital = cfg["available_capital"].as_f64();
    if private["exchange"].is_object() {
        c.exchange = serde_json::from_value(private["exchange"].clone())?;
    }
    if private["ai"].is_object() {
        c.ai = serde_json::from_value(private["ai"].clone())?;
    }
    if private["telegram"].is_object() {
        c.telegram = serde_json::from_value(private["telegram"].clone())?;
    }
    // Manual restart required after migration; never silently enable a rewritten live engine.
    c.running = false;
    crate::api::validate(&c)?;
    let auth: Value = serde_json::from_slice(&std::fs::read(old.join("panel-auth.json"))?)?;
    atomic_json(&store.dir.join("auth.json"), &auth)?;
    ensure!(store.trades()?.is_empty(), "目标已有交易记录，禁止重复导入");
    for t in &imported {
        store.insert_trade(t.clone(), "")?;
    }
    let log = old.join("ai/decisions.jsonl");
    if log.exists() {
        for (line_no, line) in std::fs::read_to_string(log)?.lines().enumerate() {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                store.decision(&v)?;
                if v["time"]
                    .as_str()
                    .is_some_and(|t| t.starts_with(&crate::model::day()))
                {
                    if let Some(acts) = v["actions"].as_array() {
                        for (i, a) in acts.iter().enumerate() {
                            if a["status"] == "executed" && a["action"] != "close" {
                                let link = format!("import-{}-{}", line_no, i);
                                store.reserve(
                                    &link,
                                    "entry",
                                    a["pair"].as_str().unwrap_or(""),
                                    &json!({}),
                                )?;
                                store.order_state(&link, "filled")?;
                            }
                        }
                    }
                }
            }
        }
    }
    store.save_config(&c)?;
    store.log(
        "INFO",
        &format!(
            "已从 Freqtrade 导入 {} 笔已平仓交易；新引擎保持暂停",
            imported.len()
        ),
    );
    println!(
        "迁移完成：{} 笔历史交易；旧数据保持原样；新引擎暂停",
        imported.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migration_refuses_open_trades_and_preserves_legacy_files() {
        let root = std::env::temp_dir().join(format!("tide-migrate-{}", uuid::Uuid::new_v4()));
        let old = root.join("old");
        std::fs::create_dir_all(&old).unwrap();
        let cfg = json!({"trading_mode":"spot","exchange":{"pair_whitelist":["BTC/USDT"]},"max_open_trades":3});
        atomic_json(&old.join("config.json"), &cfg).unwrap();
        atomic_json(&old.join("config-private.json"), &json!({})).unwrap();
        atomic_json(
            &old.join("panel-auth.json"),
            &crate::auth::Auth::create("admin".into(), "test").unwrap(),
        )
        .unwrap();
        let before = std::fs::read(old.join("config.json")).unwrap();
        let db = rusqlite::Connection::open(old.join("tradesv3.live.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE trades(id INTEGER,is_open INTEGER,pair TEXT,open_date TEXT,close_date TEXT,open_rate REAL,close_rate REAL,amount REAL,stake_amount REAL,close_profit_abs REAL,close_profit REAL); INSERT INTO trades VALUES(1,1,'BTC/USDT','2026-10-01 00:00:00',NULL,100,110,1,100,10,0.1);").unwrap();
        let store = Store::open(&root.join("new")).unwrap();
        assert!(run(&store, &old).is_err());
        assert!(!store.dir.join("config.json").exists());
        db.execute(
            "UPDATE trades SET is_open=0,close_date='2026-10-02 00:00:00'",
            [],
        )
        .unwrap();
        run(&store, &old).unwrap();
        assert_eq!(store.trades().unwrap().len(), 1);
        assert!(!store.config().unwrap().running);
        assert_eq!(before, std::fs::read(old.join("config.json")).unwrap());
        assert!(run(&store, &old).is_err());
        drop(db);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}
