//! 账户库 SQLite 实现（AccountDb）。
//!
//! 账户数据是事务型小数据（记账/持仓/决策留痕），且对一致性要求最高——
//! 用全球最成熟的 SQLite（WAL 模式），替代 DuckDB（分析型内核在高频小事务
//! + 进程被 kill 场景下出现过索引损坏事故）。
//!
//! 行情库（分析型重负载）仍用 DuckDB，见 `Db`。

use std::path::Path;
use std::sync::{Arc, Mutex};

use finbox_core::{Account, OrderSide, Position, Trade};
use crate::decision::DecisionLog;
use crate::review::{AccountSnapshot, ReviewRow};
use crate::trading::RecentTrade;
use crate::{Result, StoreError};

/// 账户库共享句柄。
pub type SharedAccountDb = Arc<Mutex<AccountDb>>;

/// 打开账户库（共享句柄）。
pub fn open_account_db(path: impl AsRef<Path>) -> Result<SharedAccountDb> {
    Ok(Arc::new(Mutex::new(AccountDb::open(path)?)))
}

const ACCOUNT_SCHEMA: &str = r#"
PRAGMA journal_mode=WAL;
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT
);
CREATE TABLE IF NOT EXISTS account (
    id              INTEGER PRIMARY KEY,
    cash            REAL NOT NULL,
    initial_capital REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS positions (
    thscode  TEXT PRIMARY KEY,
    name     TEXT NOT NULL,
    quantity INTEGER NOT NULL,
    avg_cost REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS trades (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    thscode     TEXT NOT NULL,
    name        TEXT NOT NULL,
    side        TEXT NOT NULL,
    price       REAL NOT NULL,
    quantity    INTEGER NOT NULL,
    amount      REAL NOT NULL,
    fee         REAL NOT NULL,
    decision_id INTEGER,
    ts_ms       INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS decision_logs (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms        INTEGER NOT NULL,
    model        TEXT,
    context      TEXT,
    raw_response TEXT,
    actions      TEXT,
    status       TEXT,
    note         TEXT
);
CREATE TABLE IF NOT EXISTS account_snapshots (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms        INTEGER NOT NULL,
    cash         REAL NOT NULL,
    market_value REAL NOT NULL,
    total_asset  REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS reviews (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    decision_id INTEGER NOT NULL,
    days_after  INTEGER NOT NULL,
    ts_ms       INTEGER NOT NULL,
    summary     TEXT,
    pnl         REAL
);
CREATE TABLE IF NOT EXISTS processed_adjustments (
    thscode    TEXT NOT NULL,
    ex_date_ms INTEGER NOT NULL,
    applied_ms INTEGER NOT NULL,
    PRIMARY KEY (thscode, ex_date_ms)
);
"#;

/// 账户库（SQLite）。
pub struct AccountDb {
    conn: rusqlite::Connection,
}

impl AccountDb {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = if path.as_os_str() == ":memory:" {
            rusqlite::Connection::open_in_memory()
        } else {
            rusqlite::Connection::open(path)
        }
        .map_err(|e| StoreError::Extension(format!("sqlite 打开失败: {e}")))?;
        conn.execute_batch(ACCOUNT_SCHEMA)
            .map_err(|e| StoreError::Extension(format!("sqlite schema 失败: {e}")))?;
        Ok(Self { conn })
    }

    /// 底层连接访问。
    pub fn conn(&self) -> &rusqlite::Connection {
        &self.conn
    }

    // ---------- 账户 ----------

    pub fn get_or_init_account(&self, initial_capital: f64) -> Result<Account> {
        let mut stmt = self.conn.prepare("SELECT cash, initial_capital FROM account WHERE id = 1")
            .map_err(se)?;
        let mut rows = stmt.query([]).map_err(se)?;
        if let Some(row) = rows.next().map_err(se)? {
            Ok(Account { cash: row.get(0).map_err(se)?, initial_capital: row.get(1).map_err(se)? })
        } else {
            self.conn.execute(
                "INSERT INTO account (id, cash, initial_capital) VALUES (1, ?, ?)",
                rusqlite::params![initial_capital, initial_capital],
            ).map_err(se)?;
            Ok(Account { cash: initial_capital, initial_capital })
        }
    }

    pub fn set_account_cash(&self, cash: f64) -> Result<()> {
        self.conn.execute("UPDATE account SET cash = ? WHERE id = 1", rusqlite::params![cash]).map_err(se)?;
        Ok(())
    }

    pub fn add_cash(&self, delta: f64) -> Result<()> {
        self.conn.execute("UPDATE account SET cash = cash + ? WHERE id = 1", rusqlite::params![delta]).map_err(se)?;
        Ok(())
    }

    // ---------- 持仓 ----------

    pub fn positions(&self) -> Result<Vec<Position>> {
        let mut stmt = self.conn.prepare("SELECT thscode, name, quantity, avg_cost FROM positions").map_err(se)?;
        let mut rows = stmt.query([]).map_err(se)?;
        let mut out = Vec::new();
        while let Some(r) = rows.next().map_err(se)? {
            out.push(Position {
                thscode: r.get(0).map_err(se)?,
                name: r.get(1).map_err(se)?,
                quantity: r.get::<_, i64>(2).map_err(se)? as u32,
                avg_cost: r.get(3).map_err(se)?,
            });
        }
        Ok(out)
    }

    pub fn position(&self, thscode: &str) -> Result<Option<Position>> {
        let mut stmt = self.conn.prepare("SELECT thscode, name, quantity, avg_cost FROM positions WHERE thscode = ?").map_err(se)?;
        let mut rows = stmt.query(rusqlite::params![thscode]).map_err(se)?;
        match rows.next().map_err(se)? {
            Some(r) => Ok(Some(Position {
                thscode: r.get(0).map_err(se)?,
                name: r.get(1).map_err(se)?,
                quantity: r.get::<_, i64>(2).map_err(se)? as u32,
                avg_cost: r.get(3).map_err(se)?,
            })),
            None => Ok(None),
        }
    }

    pub fn upsert_position(&self, p: &Position) -> Result<()> {
        self.conn.execute(
            "INSERT INTO positions VALUES (?, ?, ?, ?)
             ON CONFLICT(thscode) DO UPDATE SET name=excluded.name, quantity=excluded.quantity, avg_cost=excluded.avg_cost",
            rusqlite::params![p.thscode, p.name, p.quantity as i64, p.avg_cost],
        ).map_err(se)?;
        Ok(())
    }

    pub fn delete_position(&self, thscode: &str) -> Result<()> {
        self.conn.execute("DELETE FROM positions WHERE thscode = ?", rusqlite::params![thscode]).map_err(se)?;
        Ok(())
    }

    // ---------- 成交 ----------

    pub fn insert_trade(&self, t: &Trade) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO trades (thscode, name, side, price, quantity, amount, fee, decision_id, ts_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                t.thscode, t.name, t.side.as_str(), t.price, t.quantity as i64,
                t.amount, t.fee, t.decision_id, chrono::Utc::now().timestamp_millis(),
            ],
        ).map_err(se)?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn recent_trades(&self, limit: u32) -> Result<Vec<RecentTrade>> {
        let mut stmt = self.conn.prepare(
            "SELECT thscode, name, side, price, quantity, amount, fee, ts_ms, decision_id
             FROM trades ORDER BY ts_ms DESC LIMIT ?",
        ).map_err(se)?;
        let mut rows = stmt.query(rusqlite::params![limit as i64]).map_err(se)?;
        let mut out = Vec::new();
        while let Some(r) = rows.next().map_err(se)? {
            let side: String = r.get(2).map_err(se)?;
            out.push(RecentTrade {
                thscode: r.get(0).map_err(se)?,
                name: r.get(1).map_err(se)?,
                side: if side == "BUY" { OrderSide::Buy } else { OrderSide::Sell },
                price: r.get(3).map_err(se)?,
                quantity: r.get::<_, i64>(4).map_err(se)? as u32,
                amount: r.get(5).map_err(se)?,
                fee: r.get(6).map_err(se)?,
                ts_ms: r.get(7).map_err(se)?,
                decision_id: r.get(8).map_err(se)?,
            });
        }
        Ok(out)
    }

    pub fn trades_for_decision(&self, decision_id: i64) -> Result<Vec<RecentTrade>> {
        let mut stmt = self.conn.prepare(
            "SELECT thscode, name, side, price, quantity, amount, fee, ts_ms
             FROM trades WHERE decision_id = ? ORDER BY ts_ms",
        ).map_err(se)?;
        let mut rows = stmt.query(rusqlite::params![decision_id]).map_err(se)?;
        let mut out = Vec::new();
        while let Some(r) = rows.next().map_err(se)? {
            let side: String = r.get(2).map_err(se)?;
            out.push(RecentTrade {
                thscode: r.get(0).map_err(se)?,
                name: r.get(1).map_err(se)?,
                side: if side == "BUY" { OrderSide::Buy } else { OrderSide::Sell },
                price: r.get(3).map_err(se)?,
                quantity: r.get::<_, i64>(4).map_err(se)? as u32,
                amount: r.get(5).map_err(se)?,
                fee: r.get(6).map_err(se)?,
                ts_ms: r.get(7).map_err(se)?,
                decision_id: Some(decision_id),
            });
        }
        Ok(out)
    }

    /// 当日买入数量（T+1 校验用）。
    pub fn bought_between(&self, thscode: &str, start_ms: i64, end_ms: i64) -> Result<u32> {
        let qty: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(quantity), 0) FROM trades
             WHERE thscode = ? AND side = 'BUY' AND ts_ms >= ? AND ts_ms < ?",
            rusqlite::params![thscode, start_ms, end_ms],
            |r| r.get(0),
        ).map_err(se)?;
        Ok(qty as u32)
    }

    /// 某标的最近一次买入时间（超期持仓判断用）。
    pub fn position_bought_at(&self, thscode: &str) -> Result<Option<i64>> {
        let v: Option<i64> = self.conn.query_row(
            "SELECT MAX(ts_ms) FROM trades WHERE thscode = ? AND side = 'BUY'",
            rusqlite::params![thscode],
            |r| r.get(0),
        ).map_err(se)?;
        Ok(v)
    }

    /// 某标的最早买入时间（红利税持有期计算用）。
    pub fn first_buy_ms(&self, thscode: &str) -> Result<Option<i64>> {
        let v: Option<i64> = self.conn.query_row(
            "SELECT MIN(ts_ms) FROM trades WHERE thscode = ? AND side = 'BUY'",
            rusqlite::params![thscode],
            |r| r.get(0),
        ).map_err(se)?;
        Ok(v)
    }

    // ---------- 总资产估算 ----------

    pub fn total_asset_estimate(&self, account: &Account) -> Result<f64> {
        let positions = self.positions()?;
        let mv: f64 = positions.iter().map(|p| p.avg_cost * p.quantity as f64).sum();
        Ok(account.cash + mv)
    }

    // ---------- meta ----------

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare("SELECT value FROM meta WHERE key = ?").map_err(se)?;
        let mut rows = stmt.query(rusqlite::params![key]).map_err(se)?;
        match rows.next().map_err(se)? {
            Some(r) => Ok(Some(r.get(0).map_err(se)?)),
            None => Ok(None),
        }
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            rusqlite::params![key, value],
        ).map_err(se)?;
        Ok(())
    }

    /// 测试辅助：把某标的的买入记录时间拨回 1 天（绕过 T+1 边界）。
    pub fn backdate_buys(&self, thscode: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE trades SET ts_ms = ts_ms - 86400000 WHERE thscode = ? AND side = 'BUY'",
            rusqlite::params![thscode],
        ).map_err(se)?;
        Ok(())
    }

    // ---------- 除权标记 ----------

    pub fn is_adjustment_processed(&self, thscode: &str, ex_date_ms: i64) -> Result<bool> {
        let v: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM processed_adjustments WHERE thscode = ? AND ex_date_ms = ?",
            rusqlite::params![thscode, ex_date_ms],
            |r| r.get(0),
        ).map_err(se)?;
        Ok(v > 0)
    }

    pub fn mark_adjustment_processed(&self, thscode: &str, ex_date_ms: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO processed_adjustments VALUES (?, ?, ?)",
            rusqlite::params![thscode, ex_date_ms, chrono::Utc::now().timestamp_millis()],
        ).map_err(se)?;
        Ok(())
    }

    // ---------- 决策日志 ----------

    pub fn insert_decision_log(&self, log: &DecisionLog) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO decision_logs (ts_ms, model, context, raw_response, actions, status, note)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                log.ts_ms, log.model, log.context, log.raw_response, log.actions, log.status, log.note
            ],
        ).map_err(se)?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn recent_executed_decisions(&self, limit: u32) -> Result<Vec<DecisionLog>> {
        self.query_decisions("WHERE status = 'executed'", limit)
    }

    pub fn recent_decision_logs(&self, limit: u32) -> Result<Vec<DecisionLog>> {
        self.query_decisions("", limit)
    }

    fn query_decisions(&self, where_clause: &str, limit: u32) -> Result<Vec<DecisionLog>> {
        let sql = format!(
            "SELECT id, ts_ms, model, context, raw_response, actions, status, note
             FROM decision_logs {where_clause} ORDER BY ts_ms DESC LIMIT ?"
        );
        let mut stmt = self.conn.prepare(&sql).map_err(se)?;
        let mut rows = stmt.query(rusqlite::params![limit as i64]).map_err(se)?;
        let mut out = Vec::new();
        while let Some(r) = rows.next().map_err(se)? {
            out.push(DecisionLog {
                id: r.get(0).map_err(se)?,
                ts_ms: r.get(1).map_err(se)?,
                model: r.get::<_, Option<String>>(2).map_err(se)?.unwrap_or_default(),
                context: r.get::<_, Option<String>>(3).map_err(se)?.unwrap_or_default(),
                raw_response: r.get::<_, Option<String>>(4).map_err(se)?.unwrap_or_default(),
                actions: r.get::<_, Option<String>>(5).map_err(se)?.unwrap_or_default(),
                status: r.get::<_, Option<String>>(6).map_err(se)?.unwrap_or_default(),
                note: r.get::<_, Option<String>>(7).map_err(se)?.unwrap_or_default(),
            });
        }
        Ok(out)
    }

    pub fn update_decision_status(&self, id: i64, status: &str, note_suffix: Option<&str>) -> Result<()> {
        if let Some(suffix) = note_suffix {
            self.conn.execute(
                "UPDATE decision_logs SET status = ?, note = note || ? WHERE id = ?",
                rusqlite::params![status, suffix, id],
            ).map_err(se)?;
        } else {
            self.conn.execute(
                "UPDATE decision_logs SET status = ? WHERE id = ?",
                rusqlite::params![status, id],
            ).map_err(se)?;
        }
        Ok(())
    }

    // ---------- 复盘与快照 ----------

    pub fn insert_account_snapshot(&self, ts_ms: i64, cash: f64, market_value: f64, total_asset: f64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO account_snapshots (ts_ms, cash, market_value, total_asset) VALUES (?, ?, ?, ?)",
            rusqlite::params![ts_ms, cash, market_value, total_asset],
        ).map_err(se)?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn account_snapshots(&self) -> Result<Vec<AccountSnapshot>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, ts_ms, cash, market_value, total_asset FROM account_snapshots ORDER BY ts_ms",
        ).map_err(se)?;
        let mut rows = stmt.query([]).map_err(se)?;
        let mut out = Vec::new();
        while let Some(r) = rows.next().map_err(se)? {
            out.push(AccountSnapshot {
                id: r.get(0).map_err(se)?,
                ts_ms: r.get(1).map_err(se)?,
                cash: r.get(2).map_err(se)?,
                market_value: r.get(3).map_err(se)?,
                total_asset: r.get(4).map_err(se)?,
            });
        }
        Ok(out)
    }

    pub fn insert_review(&self, decision_id: i64, days_after: u32, summary: &str, pnl: f64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO reviews (decision_id, days_after, ts_ms, summary, pnl) VALUES (?, ?, ?, ?, ?)",
            rusqlite::params![decision_id, days_after as i64, chrono::Utc::now().timestamp_millis(), summary, pnl],
        ).map_err(se)?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn recent_reviews(&self, limit: u32) -> Result<Vec<ReviewRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT decision_id, days_after, summary, pnl FROM reviews ORDER BY ts_ms DESC LIMIT ?",
        ).map_err(se)?;
        let mut rows = stmt.query(rusqlite::params![limit as i64]).map_err(se)?;
        let mut out = Vec::new();
        while let Some(r) = rows.next().map_err(se)? {
            out.push(ReviewRow {
                decision_id: r.get(0).map_err(se)?,
                days_after: r.get::<_, i64>(1).map_err(se)? as u32,
                summary: r.get::<_, Option<String>>(2).map_err(se)?.unwrap_or_default(),
                pnl: r.get(3).map_err(se)?,
            });
        }
        Ok(out)
    }

    pub fn reviews_for_decision(&self, decision_id: i64) -> Result<Vec<ReviewRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT decision_id, days_after, summary, pnl FROM reviews WHERE decision_id = ? ORDER BY days_after",
        ).map_err(se)?;
        let mut rows = stmt.query(rusqlite::params![decision_id]).map_err(se)?;
        let mut out = Vec::new();
        while let Some(r) = rows.next().map_err(se)? {
            out.push(ReviewRow {
                decision_id: r.get(0).map_err(se)?,
                days_after: r.get::<_, i64>(1).map_err(se)? as u32,
                summary: r.get::<_, Option<String>>(2).map_err(se)?.unwrap_or_default(),
                pnl: r.get(3).map_err(se)?,
            });
        }
        Ok(out)
    }

    pub fn review_exists(&self, decision_id: i64, days_after: u32) -> Result<bool> {
        let v: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM reviews WHERE decision_id = ? AND days_after = ?",
            rusqlite::params![decision_id, days_after as i64],
            |r| r.get(0),
        ).map_err(se)?;
        Ok(v > 0)
    }

    // ---------- 重置 ----------

    pub fn reset_account(&self, initial_capital: f64) -> Result<()> {
        self.conn.execute_batch(&format!(
            "DELETE FROM positions;
             DELETE FROM trades;
             DELETE FROM decision_logs;
             DELETE FROM reviews;
             DELETE FROM account_snapshots;
             DELETE FROM processed_adjustments;
             UPDATE account SET cash = {initial_capital}, initial_capital = {initial_capital} WHERE id = 1;
             DELETE FROM meta WHERE key IN ('peak_asset', 'fuse_until_ms');"
        )).map_err(se)?;
        Ok(())
    }
}

/// rusqlite 错误转 StoreError。
fn se(e: rusqlite::Error) -> StoreError {
    StoreError::Extension(format!("sqlite: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_basic_flow() {
        let db = AccountDb::open(":memory:").unwrap();
        let acct = db.get_or_init_account(100_000.0).unwrap();
        assert_eq!(acct.cash, 100_000.0);
        db.set_account_cash(80_000.0).unwrap();
        let acct = db.get_or_init_account(100_000.0).unwrap();
        assert_eq!(acct.cash, 80_000.0);

        db.upsert_position(&Position {
            thscode: "600519.SH".into(), name: "贵州茅台".into(), quantity: 100, avg_cost: 1800.0,
        }).unwrap();
        assert_eq!(db.positions().unwrap().len(), 1);

        let id = db.insert_trade(&Trade {
            thscode: "600519.SH".into(), name: "贵州茅台".into(), side: OrderSide::Buy,
            price: 1800.0, quantity: 100, amount: 180_000.0, fee: 50.0, decision_id: None,
        }).unwrap();
        assert!(id > 0);
        assert_eq!(db.recent_trades(10).unwrap().len(), 1);

        db.meta_set("k", "v").unwrap();
        assert_eq!(db.meta_get("k").unwrap(), Some("v".into()));

        db.reset_account(50_000.0).unwrap();
        assert_eq!(db.positions().unwrap().len(), 0);
        assert_eq!(db.recent_trades(10).unwrap().len(), 0);
        assert_eq!(db.get_or_init_account(0.0).unwrap().cash, 50_000.0);
    }
}
