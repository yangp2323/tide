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
        Ok(Self {
            dir: dir.into(),
            db: Mutex::new(db),
        })
    }
    pub fn config(&self) -> Result<Config> {
        let path = self.dir.join("config.json");
        if path.exists() {
            Ok(serde_json::from_slice(&std::fs::read(path)?)?)
        } else {
            Ok(Config::default())
        }
    }
    pub fn save_config(&self, c: &Config) -> Result<()> {
        atomic_json(&self.dir.join("config.json"), c)
    }
    pub fn trades(&self) -> Result<Vec<Trade>> {
        let db = self.db.lock().unwrap();
        let mut q = db.prepare("SELECT data FROM trades ORDER BY id DESC")?;
        let data = q
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        data.iter().map(|s| Ok(serde_json::from_str(s)?)).collect()
    }
    pub fn open_trades(&self) -> Result<Vec<Trade>> {
        Ok(self
            .trades()?
            .into_iter()
            .filter(|t| t.close_date.is_none())
            .collect())
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
        self.db
            .lock()
            .unwrap()
            .execute("INSERT INTO decisions(data) VALUES (?)", [v.to_string()])?;
        Ok(())
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
