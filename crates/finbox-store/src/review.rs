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
        Ok(self.last_insert_rowid())
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
}
