use crate::model::{now, Config, Trade};
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

pub struct Store {
    pub dir: PathBuf,
    db: Mutex<Connection>,
}
/// How long the AI's decision records and the event log are kept. Trades, orders and
/// the knowledge base are never removed.
const KEEP_DECISIONS_DAYS: i64 = 90;
const KEEP_EVENTS_DAYS: i64 = 30;
fn prune(db: &Connection) {
    let before = |days: i64| {
        (chrono::Utc::now() - chrono::Duration::days(days))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
    };
    let _ = db.execute(
        "DELETE FROM decisions WHERE json_extract(data,'$.time') < ?1",
        [before(KEEP_DECISIONS_DAYS)],
    );
    let _ = db.execute(
        "DELETE FROM events WHERE time < ?1",
        [before(KEEP_EVENTS_DAYS)],
    );
}
impl Store {
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let db = Connection::open(dir.join("tide.sqlite"))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;
          CREATE TABLE IF NOT EXISTS trades(id INTEGER PRIMARY KEY AUTOINCREMENT, data TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS orders(link TEXT PRIMARY KEY, day TEXT NOT NULL, action TEXT NOT NULL, pair TEXT NOT NULL, state TEXT NOT NULL, data TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY, time TEXT NOT NULL, level TEXT NOT NULL, message TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS decisions(id INTEGER PRIMARY KEY, data TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS state(key TEXT PRIMARY KEY, value TEXT NOT NULL);")?;
        migrate_schema(&db)?;
        let store = Self {
            dir: dir.into(),
            db: Mutex::new(db),
        };
        store.apply_ai_limits()?;
        store.seed_knowledge()?;
        prune(&store.db.lock().unwrap());
        Ok(store)
    }
    /// Once: the starter notes. Deleting them later is respected.
    fn seed_knowledge(&self) -> Result<()> {
        if self.state("knowledge_seed_v1")?.is_none() {
            for (title, content, source) in crate::knowledge_seed::SEEDS {
                self.knowledge_add(title, content, source, true, "seed")?;
            }
            self.set_state("knowledge_seed_v1", "1")?;
        }
        Ok(())
    }
    pub fn knowledge(&self) -> Result<Vec<Value>> {
        let db = self.db.lock().unwrap();
        let mut q = db.prepare(
            "SELECT id,title,content,source,enabled,updated,origin FROM knowledge ORDER BY id",
        )?;
        let rows = q
            .query_map([], |r| {
                Ok(json!({"id":r.get::<_,i64>(0)?,"title":r.get::<_,String>(1)?,"content":r.get::<_,String>(2)?,"source":r.get::<_,String>(3)?,"enabled":r.get::<_,i64>(4)? != 0,"updated":r.get::<_,String>(5)?,"origin":r.get::<_,String>(6)?}))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    pub fn knowledge_add(
        &self,
        title: &str,
        content: &str,
        source: &str,
        enabled: bool,
        origin: &str,
    ) -> Result<i64> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT INTO knowledge(title,content,source,enabled,updated,origin) VALUES (?,?,?,?,?,?)",
            params![title, content, source, enabled as i64, now(), origin],
        )?;
        Ok(db.last_insert_rowid())
    }
    pub fn knowledge_update(
        &self,
        id: i64,
        title: &str,
        content: &str,
        source: &str,
        enabled: bool,
    ) -> Result<bool> {
        Ok(self.db.lock().unwrap().execute(
            "UPDATE knowledge SET title=?,content=?,source=?,enabled=?,updated=? WHERE id=?",
            params![title, content, source, enabled as i64, now(), id],
        )? > 0)
    }
    pub fn knowledge_delete(&self, id: i64) -> Result<bool> {
        Ok(self
            .db
            .lock()
            .unwrap()
            .execute("DELETE FROM knowledge WHERE id=?", [id])?
            > 0)
    }
    /// Once: an existing configuration moves to the AI-managed limits, because those settings
    /// are no longer editable and would otherwise keep their old manual values.
    fn apply_ai_limits(&self) -> Result<()> {
        if self.state("ai_limits_v1")?.is_none() {
            if self.dir.join("config.json").exists() {
                let mut c = self.config()?;
                c.max_open = crate::model::MAX_OPEN;
                c.max_same_direction = 0;
                c.short = c.market == "futures";
                self.save_config(&c)?;
            }
            self.set_state("ai_limits_v1", "1")?;
        }
        // When the position cap changes, a saved limit above it follows it down.
        let cap = crate::model::MAX_OPEN.to_string();
        if self.state("max_open_cap")?.as_deref() != Some(cap.as_str()) {
            if self.dir.join("config.json").exists() {
                let mut c = self.config()?;
                if c.max_open > crate::model::MAX_OPEN {
                    c.max_open = crate::model::MAX_OPEN;
                    self.save_config(&c)?;
                }
            }
            self.set_state("max_open_cap", &cap)?;
        }
        Ok(())
    }
    pub fn config(&self) -> Result<Config> {
        let path = self.dir.join("config.json");
        let mut c: Config = if path.exists() {
            serde_json::from_slice(&std::fs::read(path)?)?
        } else {
            Config::default()
        };
        c.ai.normalize();
        Ok(c)
    }
    pub fn save_config(&self, c: &Config) -> Result<()> {
        let mut c = c.clone();
        c.ai.normalize();
        atomic_json(&self.dir.join("config.json"), &c)
    }
    pub fn trades(&self) -> Result<Vec<Trade>> {
        let db = self.db.lock().unwrap();
        let mut q = db.prepare("SELECT data FROM trades ORDER BY id DESC")?;
        let data = q
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        data.iter().map(|s| Ok(serde_json::from_str(s)?)).collect()
    }
    fn trades_where(&self, filter: &str, args: impl rusqlite::Params) -> Result<Vec<Trade>> {
        let db = self.db.lock().unwrap();
        let mut q = db.prepare(&format!(
            "SELECT data FROM trades WHERE {} ORDER BY id DESC",
            filter
        ))?;
        let data = q
            .query_map(args, |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        data.iter().map(|s| Ok(serde_json::from_str(s)?)).collect()
    }
    pub fn open_trades(&self) -> Result<Vec<Trade>> {
        self.trades_where("json_extract(data,'$.close_date') IS NULL", [])
    }
    /// Trades closed after `since` ("YYYY-MM-DD HH:MM:SS"), newest first.
    pub fn closed_since(&self, since: &str, limit: usize) -> Result<Vec<Trade>> {
        let mut v = self.trades_where("json_extract(data,'$.close_date') > ?1", [since])?;
        v.sort_by(|a, b| b.close_date.cmp(&a.close_date));
        v.truncate(limit);
        Ok(v)
    }
    /// Realized PnL of trades closed after `since` ("YYYY-MM-DD HH:MM:SS").
    pub fn realized_since(&self, since: &str) -> Result<f64> {
        let db = self.db.lock().unwrap();
        Ok(db.query_row(
            "SELECT COALESCE(SUM(json_extract(data,'$.profit_abs')),0) FROM trades WHERE json_extract(data,'$.close_date') > ?1",
            [since],
            |r| r.get(0),
        )?)
    }
    /// Realized PnL of trades closed on the given UTC day ("YYYY-MM-DD").
    pub fn realized_on(&self, day: &str) -> Result<f64> {
        let db = self.db.lock().unwrap();
        Ok(db.query_row(
            "SELECT COALESCE(SUM(json_extract(data,'$.profit_abs')),0) FROM trades WHERE json_extract(data,'$.close_date') LIKE ?1",
            [format!("{}%", day)],
            |r| r.get(0),
        )?)
    }
    pub fn log_stop(&self, trade_id: i64, pair: &str, stop: f64, tp: f64, source: &str) {
        if let Ok(db) = self.db.lock() {
            let _ = db.execute(
                "INSERT INTO stop_changes(time,trade_id,pair,stop,take_profit,source) VALUES (?,?,?,?,?,?)",
                params![now(), trade_id, pair, stop, tp, source],
            );
            let _ = db.execute(
                "DELETE FROM stop_changes WHERE id <= (SELECT MAX(id) FROM stop_changes) - 5000",
                [],
            );
        }
    }
    pub fn stop_history(&self, limit: usize) -> Result<Value> {
        let db = self.db.lock().unwrap();
        let mut q = db.prepare(
            "SELECT time,trade_id,pair,stop,take_profit,source FROM stop_changes ORDER BY id DESC LIMIT ?",
        )?;
        let rows = q
            .query_map([limit], |r| {
                Ok(json!({"time":r.get::<_,String>(0)?,"trade_id":r.get::<_,i64>(1)?,"pair":r.get::<_,String>(2)?,"stop":r.get::<_,f64>(3)?,"take_profit":r.get::<_,f64>(4)?,"source":r.get::<_,String>(5)?}))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!(rows))
    }
    pub fn insert_trade(&self, mut t: Trade, link: &str) -> Result<Trade> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        tx.execute("INSERT INTO trades(data) VALUES ('{}')", [])?;
        t.trade_id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE trades SET data=? WHERE id=?",
            params![serde_json::to_string(&t)?, t.trade_id],
        )?;
        tx.execute(
            "UPDATE orders SET state='filled',data=? WHERE link=?",
            params![serde_json::to_string(&t)?, link],
        )?;
        tx.commit()?;
        Ok(t)
    }
    pub fn update_trade(&self, t: &Trade) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE trades SET data=? WHERE id=?",
            params![serde_json::to_string(t)?, t.trade_id],
        )?;
        Ok(())
    }
    /// Record a filled add: the merged position and the order's final state together.
    pub fn settle_add(&self, merged: &Trade, link: &str) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        tx.execute(
            "UPDATE trades SET data=? WHERE id=?",
            params![serde_json::to_string(merged)?, merged.trade_id],
        )?;
        tx.execute("UPDATE orders SET state='filled' WHERE link=?", [link])?;
        tx.commit()?;
        Ok(())
    }
    pub fn settle_exit(&self, t: &Trade, remaining: Option<&Trade>, link: &str) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        if let Some(remaining) = remaining {
            tx.execute(
                "UPDATE trades SET data=? WHERE id=?",
                params![serde_json::to_string(remaining)?, remaining.trade_id],
            )?;
            let mut closed = t.clone();
            tx.execute("INSERT INTO trades(data) VALUES ('{}')", [])?;
            closed.trade_id = tx.last_insert_rowid();
            tx.execute(
                "UPDATE trades SET data=? WHERE id=?",
                params![serde_json::to_string(&closed)?, closed.trade_id],
            )?;
        } else {
            tx.execute(
                "UPDATE trades SET data=? WHERE id=?",
                params![serde_json::to_string(t)?, t.trade_id],
            )?;
        }
        tx.execute("UPDATE orders SET state='filled' WHERE link=?", [link])?;
        tx.commit()?;
        Ok(())
    }
    pub fn reserve(&self, link: &str, action: &str, pair: &str, data: &Value) -> Result<()> {
        self.db.lock().unwrap().execute(
            "INSERT INTO orders VALUES (?,?,?,?,?,?)",
            params![
                link,
                crate::model::day(),
                action,
                pair,
                "pending",
                data.to_string()
            ],
        )?;
        Ok(())
    }
    pub fn order_state(&self, link: &str, state: &str) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE orders SET state=? WHERE link=?",
            params![state, link],
        )?;
        Ok(())
    }
    /// Whether this order link was submitted by the bot (in any state).
    pub fn has_order(&self, link: &str) -> Result<bool> {
        Ok(self.db.lock().unwrap().query_row(
            "SELECT EXISTS(SELECT 1 FROM orders WHERE link=?)",
            [link],
            |r| r.get(0),
        )?)
    }
    pub fn unresolved(&self) -> Result<Vec<Value>> {
        let db = self.db.lock().unwrap();
        let mut q = db.prepare("SELECT link,action,pair,data FROM orders WHERE state='pending'")?;
        let rows=q.query_map([],|r|Ok(json!({"link":r.get::<_,String>(0)?,"action":r.get::<_,String>(1)?,"pair":r.get::<_,String>(2)?,"data":serde_json::from_str::<Value>(&r.get::<_,String>(3)?).unwrap_or(Value::Null)})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    pub fn log(&self, level: &str, message: &str) {
        tracing::info!(level, message);
        if let Ok(db) = self.db.lock() {
            let _ = db.execute(
                "INSERT INTO events(time,level,message) VALUES (?,?,?)",
                params![now(), level, message],
            );
            if db.last_insert_rowid() % 1000 == 0 {
                prune(&db);
            }
        }
    }
    pub fn logs(&self, limit: usize) -> Result<Value> {
        let db = self.db.lock().unwrap();
        let mut q = db.prepare("SELECT time,level,message FROM events ORDER BY id DESC LIMIT ?")?;
        let mut rows = q
            .query_map([limit], |r| {
                Ok(json!([
                    r.get::<_, String>(0)?,
                    "tide",
                    0,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?
                ]))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.reverse();
        Ok(json!({"logs":rows}))
    }
    pub fn decision(&self, v: &Value) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute("INSERT INTO decisions(data) VALUES (?)", [v.to_string()])?;
        if db.last_insert_rowid() % 100 == 0 {
            prune(&db);
        }
        Ok(())
    }
    /// Model calls and token use per UTC day over the last `days` days, newest first. Tokens only:
    /// what they cost depends on the model and the channel, which this does not know.
    pub fn usage_by_day(&self, days: i64) -> Result<Value> {
        let since = (chrono::Utc::now() - chrono::Duration::days(days.max(1) - 1))
            .format("%Y-%m-%d 00:00:00")
            .to_string();
        let rows = {
            let db = self.db.lock().unwrap();
            let mut q = db.prepare(
                "SELECT json_extract(data,'$.time'), json_extract(data,'$.source'), json_extract(data,'$.error') IS NOT NULL, json_extract(data,'$.usage') FROM decisions WHERE json_extract(data,'$.time') >= ?1",
            )?;
            let rows = q
                .query_map([since], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        r.get::<_, bool>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let mut by_day = std::collections::BTreeMap::<String, Value>::new();
        for (time, source, failed, usage) in rows {
            let d = by_day.entry(time[..10].to_string()).or_insert_with(|| {
                json!({"date":&time[..10],"calls":0,"failed":0,"by_source":{},"input":0,"cache_read":0,"cache_write":0,"output":0})
            });
            let add = |d: &mut Value, k: &str, n: u64| d[k] = json!(d[k].as_u64().unwrap_or(0) + n);
            add(d, "calls", 1);
            if failed {
                add(d, "failed", 1);
            }
            let n = d["by_source"][&source].as_u64().unwrap_or(0) + 1;
            d["by_source"][&source] = json!(n);
            if let Some(u) = usage.and_then(|u| serde_json::from_str::<Value>(&u).ok()) {
                let (input, read, write, output) = crate::model::token_use(&u);
                add(d, "input", input);
                add(d, "cache_read", read);
                add(d, "cache_write", write);
                add(d, "output", output);
            }
        }
        Ok(json!(by_day.into_values().rev().collect::<Vec<_>>()))
    }
    pub fn decisions(&self, limit: usize) -> Result<Vec<Value>> {
        let db = self.db.lock().unwrap();
        let mut q = db.prepare("SELECT data FROM decisions ORDER BY id DESC LIMIT ?")?;
        let rows = q
            .query_map([limit], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.iter().map(|s| Ok(serde_json::from_str(s)?)).collect()
    }
    pub fn state(&self, key: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT value FROM state WHERE key=?", [key], |r| r.get(0))
            .optional()?)
    }
    pub fn set_state(&self, key: &str, value: &str) -> Result<()> {
        self.db.lock().unwrap().execute(
            "INSERT INTO state VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}
/// Ordered schema upgrades, tracked with SQLite's user_version so a change never runs twice.
fn migrate_schema(db: &Connection) -> Result<()> {
    const STEPS: &[&str] = &[
        "CREATE INDEX IF NOT EXISTS trades_close ON trades(json_extract(data,'$.close_date'))",
        "CREATE TABLE IF NOT EXISTS stop_changes(id INTEGER PRIMARY KEY, time TEXT NOT NULL, trade_id INTEGER NOT NULL, pair TEXT NOT NULL, stop REAL NOT NULL, take_profit REAL NOT NULL, source TEXT NOT NULL)",
        "CREATE TABLE IF NOT EXISTS knowledge(id INTEGER PRIMARY KEY, title TEXT NOT NULL, content TEXT NOT NULL, source TEXT NOT NULL DEFAULT '', enabled INTEGER NOT NULL DEFAULT 1, updated TEXT NOT NULL, origin TEXT NOT NULL DEFAULT 'user')",
    ];
    let version: usize = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in STEPS.iter().enumerate().skip(version) {
        db.execute_batch(&format!(
            "BEGIN; {}; PRAGMA user_version={}; COMMIT;",
            sql,
            i + 1
        ))?;
    }
    Ok(())
}
pub fn atomic_json(path: &Path, data: &impl serde::Serialize) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(&tmp)?;
    use std::io::Write;
    file.write_all(&serde_json::to_vec_pretty(data)?)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path).context("保存配置失败")?;
    #[cfg(unix)]
    {
        std::fs::File::open(path.parent().unwrap())?.sync_all()?;
    }
    Ok(())
}
