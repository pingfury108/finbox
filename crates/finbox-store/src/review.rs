//! finbox-store 复盘与账户快照。

use crate::{Db, Result};

/// 账户快照。
#[derive(Debug, Clone)]
pub struct AccountSnapshot {
    pub id: i64,
    pub ts_ms: i64,
    pub cash: f64,
    pub market_value: f64,
    pub total_asset: f64,
}

/// 复盘记录（查询用）。
#[derive(Debug, Clone)]
pub struct ReviewRow {
    pub decision_id: i64,
    pub days_after: u32,
    pub summary: String,
    pub pnl: f64,
}

impl Db {
    /// 插入账户快照，返回 id。
    pub fn insert_account_snapshot(
        &self,
        ts_ms: i64,
        cash: f64,
        market_value: f64,
        total_asset: f64,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO account_snapshots (ts_ms, cash, market_value, total_asset) VALUES (?, ?, ?, ?)",
            duckdb::params![ts_ms, cash, market_value, total_asset],
        )?;
        let id: i64 = self.conn.query_row("SELECT MAX(id) FROM account_snapshots", [], |r| r.get(0))?;
        Ok(id)
    }

    /// 全部账户快照（时间正序）。
    pub fn account_snapshots(&self) -> Result<Vec<AccountSnapshot>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, ts_ms, cash, market_value, total_asset FROM account_snapshots ORDER BY ts_ms",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(AccountSnapshot {
                id: r.get(0)?,
                ts_ms: r.get(1)?,
                cash: r.get(2)?,
                market_value: r.get(3)?,
                total_asset: r.get(4)?,
            });
        }
        Ok(out)
    }

    /// 最新账户快照。
    pub fn latest_account_snapshot(&self) -> Result<Option<AccountSnapshot>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, ts_ms, cash, market_value, total_asset FROM account_snapshots
             ORDER BY ts_ms DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(r) => Ok(Some(AccountSnapshot {
                id: r.get(0)?,
                ts_ms: r.get(1)?,
                cash: r.get(2)?,
                market_value: r.get(3)?,
                total_asset: r.get(4)?,
            })),
            None => Ok(None),
        }
    }

    /// 写入复盘记录。
    pub fn insert_review(&self, decision_id: i64, days_after: u32, summary: &str, pnl: f64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO reviews (decision_id, days_after, ts_ms, summary, pnl) VALUES (?, ?, ?, ?, ?)",
            duckdb::params![decision_id, days_after as i64, chrono::Utc::now().timestamp_millis(), summary, pnl],
        )?;
        Ok(())
    }

    /// 最近 N 条复盘记录（时间倒序）。
    pub fn recent_reviews(&self, limit: u32) -> Result<Vec<ReviewRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT decision_id, days_after, summary, pnl FROM reviews ORDER BY ts_ms DESC LIMIT ?",
        )?;
        let mut rows = stmt.query(duckdb::params![limit as i64])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(ReviewRow {
                decision_id: r.get(0)?,
                days_after: r.get::<_, i64>(1)? as u32,
                summary: r.get(2)?,
                pnl: r.get(3)?,
            });
        }
        Ok(out)
    }

    /// 某条决策的全部复盘结果（D+1/D+5/D+10）。
    pub fn reviews_for_decision(&self, decision_id: i64) -> Result<Vec<ReviewRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT decision_id, days_after, summary, pnl FROM reviews WHERE decision_id = ? ORDER BY days_after",
        )?;
        let mut rows = stmt.query(duckdb::params![decision_id])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(ReviewRow {
                decision_id: r.get(0)?,
                days_after: r.get::<_, i64>(1)? as u32,
                summary: r.get(2)?,
                pnl: r.get(3)?,
            });
        }
        Ok(out)
    }

    /// 某个决策是否已复盘过（避免重复）。
    pub fn review_exists(&self, decision_id: i64, days_after: u32) -> Result<bool> {
        let v: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM reviews WHERE decision_id = ? AND days_after = ?",
            duckdb::params![decision_id, days_after as i64],
            |r| r.get(0),
        )?;
        Ok(v > 0)
    }
}
