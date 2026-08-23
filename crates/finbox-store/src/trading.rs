//! finbox-store 交易持久化：账户、持仓、成交流水。
//!
//! 模拟盘状态全部落库，重启不丢；成交价来自真实行情（快照/日K）。

use crate::{Db, Result};
use finbox_core::{Account, OrderSide, Position, Trade};

/// 成交流水（查询用）。
#[derive(Debug, Clone)]
pub struct RecentTrade {
    pub thscode: String,
    pub name: String,
    pub side: OrderSide,
    pub price: f64,
    pub quantity: u32,
    pub amount: f64,
    pub fee: f64,
    pub ts_ms: i64,
}

impl Db {
    /// 读取模拟账户（不存在则按初始资金初始化）。
    pub fn get_or_init_account(&self, initial_capital: f64) -> Result<Account> {
        let mut stmt = self.conn.prepare(
            "SELECT cash, initial_capital FROM account WHERE id = 1",
        )?;
        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            Ok(Account { cash: row.get(0)?, initial_capital: row.get(1)? })
        } else {
            self.conn.execute(
                "INSERT INTO account (id, cash, initial_capital) VALUES (1, ?, ?)",
                duckdb::params![initial_capital, initial_capital],
            )?;
            Ok(Account { cash: initial_capital, initial_capital })
        }
    }

    pub fn set_account_cash(&self, cash: f64) -> Result<()> {
        self.conn.execute("UPDATE account SET cash = ? WHERE id = 1", duckdb::params![cash])?;
        Ok(())
    }

    /// 全部持仓。
    pub fn positions(&self) -> Result<Vec<Position>> {
        let mut stmt = self.conn.prepare(
            "SELECT thscode, name, quantity, avg_cost FROM positions ORDER BY thscode",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(Position {
                thscode: r.get(0)?,
                name: r.get(1)?,
                quantity: r.get::<_, i64>(2)? as u32,
                avg_cost: r.get(3)?,
            });
        }
        Ok(out)
    }

    /// 单只持仓。
    pub fn position(&self, thscode: &str) -> Result<Option<Position>> {
        let mut stmt =
            self.conn.prepare("SELECT thscode, name, quantity, avg_cost FROM positions WHERE thscode = ?")?;
        let mut rows = stmt.query(duckdb::params![thscode])?;
        match rows.next()? {
            Some(r) => Ok(Some(Position {
                thscode: r.get(0)?,
                name: r.get(1)?,
                quantity: r.get::<_, i64>(2)? as u32,
                avg_cost: r.get(3)?,
            })),
            None => Ok(None),
        }
    }

    /// 更新或新增持仓。
    pub fn upsert_position(&self, p: &Position) -> Result<()> {
        self.conn.execute(
            "INSERT INTO positions (thscode, name, quantity, avg_cost) VALUES (?, ?, ?, ?)
             ON CONFLICT (thscode) DO UPDATE SET
                name = excluded.name, quantity = excluded.quantity, avg_cost = excluded.avg_cost",
            duckdb::params![p.thscode, p.name, p.quantity as i64, p.avg_cost],
        )?;
        Ok(())
    }

    pub fn delete_position(&self, thscode: &str) -> Result<()> {
        self.conn.execute("DELETE FROM positions WHERE thscode = ?", duckdb::params![thscode])?;
        Ok(())
    }

    /// 追加成交流水。
    pub fn insert_trade(&self, t: &Trade) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO trades (thscode, name, side, price, quantity, amount, fee, decision_id, ts_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            duckdb::params![
                t.thscode,
                t.name,
                t.side.as_str(),
                t.price,
                t.quantity as i64,
                t.amount,
                t.fee,
                t.decision_id,
                chrono::Utc::now().timestamp_millis(),
            ],
        )?;
        Ok(self.last_insert_rowid())
    }

    /// 当日买入数量（T+1 校验用）。`start_ms`/`end_ms` 为当日 00:00~次日 00:00（Asia/Shanghai）毫秒戳。
    pub fn bought_between(&self, thscode: &str, start_ms: i64, end_ms: i64) -> Result<u32> {
        let qty: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(quantity), 0) FROM trades
             WHERE thscode = ? AND side = 'BUY' AND ts_ms >= ? AND ts_ms < ?",
            duckdb::params![thscode, start_ms, end_ms],
            |r| r.get(0),
        )?;
        Ok(qty as u32)
    }

    /// 总资产 = 现金 + 持仓市值（按最新快照价，无快照用成本价）。
    pub fn total_asset(&self, account: &Account) -> Result<f64> {
        let positions = self.positions()?;
        let mut mv = 0.0;
        for p in &positions {
            let price = self.latest_snapshot_price(&p.thscode)?.unwrap_or(p.avg_cost);
            mv += price * p.quantity as f64;
        }
        Ok(account.cash + mv)
    }

    /// 是否交易日（本地 trading_days 表，`date` 为 `yyyyMMdd`）。
    pub fn is_trading_day(&self, yyyymmdd: &str) -> Result<bool> {
        let v: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM trading_days WHERE date = ?",
            duckdb::params![yyyymmdd],
            |r| r.get(0),
        )?;
        Ok(v > 0)
    }

    /// 代码表查询名称。
    pub fn ticker_name(&self, thscode: &str) -> Result<String> {
        let v: String = self.conn.query_row(
            "SELECT name FROM tickers WHERE thscode = ?",
            duckdb::params![thscode],
            |r| r.get(0),
        )?;
        Ok(v)
    }

    /// 最新快照价。无快照返回 `Ok(None)`。
    pub fn latest_snapshot_price(&self, thscode: &str) -> Result<Option<f64>> {
        let v = self.conn.query_row(
            "SELECT (SELECT last_price FROM snapshots WHERE thscode = ?
                     ORDER BY ts_ms DESC LIMIT 1)",
            duckdb::params![thscode],
            |r| r.get::<_, Option<f64>>(0),
        )?;
        Ok(v)
    }

    /// 最近收盘价（日 K 最新一根），用于昨收。无记录返回 `Ok(None)`。
    pub fn prev_close(&self, thscode: &str) -> Result<Option<f64>> {
        let v = self.conn.query_row(
            "SELECT (SELECT close_price FROM daily_bars WHERE thscode = ?
                     ORDER BY date_ms DESC LIMIT 1)",
            duckdb::params![thscode],
            |r| r.get::<_, Option<f64>>(0),
        )?;
        Ok(v)
    }

    /// 最近 N 条成交流水（时间倒序）。
    pub fn recent_trades(&self, limit: u32) -> Result<Vec<RecentTrade>> {
        let mut stmt = self.conn.prepare(
            "SELECT thscode, name, side, price, quantity, amount, fee, ts_ms
             FROM trades ORDER BY ts_ms DESC LIMIT ?",
        )?;
        let mut rows = stmt.query(duckdb::params![limit as i64])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let side: String = r.get(2)?;
            out.push(RecentTrade {
                thscode: r.get(0)?,
                name: r.get(1)?,
                side: if side == "BUY" { finbox_core::OrderSide::Buy } else { finbox_core::OrderSide::Sell },
                price: r.get(3)?,
                quantity: r.get::<_, i64>(4)? as u32,
                amount: r.get(5)?,
                fee: r.get(6)?,
                ts_ms: r.get(7)?,
            });
        }
        Ok(out)
    }

    pub(crate) fn last_insert_rowid(&self) -> i64 {
        self.conn
            .query_row("SELECT last_insert_rowid()", [], |r| r.get(0))
            .unwrap_or(0)
    }

    /// 测试辅助：把某标的全部买入记录回拨到昨日，用于绕过 T+1 校验。
    pub fn backdate_buys(&self, thscode: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE trades SET ts_ms = ts_ms - 86400000 WHERE thscode = ? AND side = 'BUY'",
            duckdb::params![thscode],
        )?;
        Ok(())
    }
}
