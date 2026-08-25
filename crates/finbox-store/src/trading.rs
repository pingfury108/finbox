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
    /// 关联的决策日志 id（None 表示非 AI 决策触发，如风控）
    pub decision_id: Option<i64>,
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

    /// 追加成交流水，返回 id。
    pub fn insert_trade(&self, t: &Trade) -> Result<i64> {
        let id: i64 = self.conn.query_row(
            "INSERT INTO trades (thscode, name, side, price, quantity, amount, fee, decision_id, ts_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
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
            |r| r.get(0),
        )?;
        Ok(id)
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

    /// 总资产估算 = 现金 + 持仓市值（按成本价兜底，**不查行情表**，可在账户库独立调用）。
    /// 精确市值（按最新行情）需跨库，由上层（broker/risk）自行计算。
    pub fn total_asset_estimate(&self, account: &Account) -> Result<f64> {
        let positions = self.positions()?;
        let mv: f64 = positions.iter().map(|p| p.avg_cost * p.quantity as f64).sum();
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

    /// 按代码/名称模糊搜索标的（市场行情库）。返回 (thscode, name, ticker)。
    pub fn search_tickers(&self, q: &str, limit: u32) -> Result<Vec<(String, String, String)>> {
        let like = format!("%{q}%");
        let mut stmt = self.conn.prepare(
            "SELECT thscode, name, ticker FROM tickers
             WHERE thscode LIKE ? OR name LIKE ? OR ticker LIKE ?
             ORDER BY thscode LIMIT ?",
        )?;
        let mut rows = stmt.query(duckdb::params![&like, &like, &like, limit as i64])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push((r.get(0)?, r.get(1)?, r.get(2)?));
        }
        Ok(out)
    }

    /// 代码表查询名称。
    pub fn ticker_name(&self, thscode: &str) -> Result<String> {        let v: String = self.conn.query_row(
            "SELECT name FROM tickers WHERE thscode = ?",
            duckdb::params![thscode],
            |r| r.get(0),
        )?;
        Ok(v)
    }

    /// 最新快照完整行情（现价/涨额/涨幅%/时间戳）。
    pub fn latest_snapshot_full(&self, thscode: &str) -> Result<Option<(f64, f64, f64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT last_price, price_change, price_change_ratio_pct, ts_ms FROM snapshots
             WHERE thscode = ? ORDER BY ts_ms DESC LIMIT 1",
        )?;
        let mut rows = stmt.query(duckdb::params![thscode])?;
        match rows.next()? {
            Some(r) => Ok(Some((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))),
            None => Ok(None),
        }
    }

    /// 某标的今日行情快照（盘中实时）。返回 (最新价, 今开, 今日最高, 今日最低, 今日累计量)。
    /// 盘中未采集返回 None。
    pub fn today_snapshot(&self, thscode: &str) -> Result<Option<(f64, f64, f64, f64, f64)>> {
        let start_ms = chrono::Local::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_local_timezone(chrono::Local)
            .unwrap()
            .timestamp_millis();
        let mut stmt = self.conn.prepare(
            "SELECT last_price, open_price, high_price, low_price, volume FROM snapshots
             WHERE thscode = ? AND ts_ms >= ?
             ORDER BY ts_ms DESC LIMIT 1",
        )?;
        let mut rows = stmt.query(duckdb::params![thscode, start_ms])?;
        match rows.next()? {
            Some(r) => Ok(Some((
                r.get::<_, f64>(0)?,
                r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
            ))),
            None => Ok(None),
        }
    }

    /// 最新快照价。无快照返回 `Ok(None)`。
    pub fn latest_snapshot_price(&self, thscode: &str) -> Result<Option<f64>> {        let v = self.conn.query_row(
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

    /// 某只持仓最近一次买入时间（毫秒），用于持仓天数/超期清仓判断。
    pub fn position_bought_at(&self, thscode: &str) -> Result<Option<i64>> {
        let v: Option<i64> = self.conn.query_row(
            "SELECT MAX(ts_ms) FROM trades WHERE thscode = ? AND side = 'BUY'",
            duckdb::params![thscode],
            |r| r.get(0),
        )?;
        Ok(v)
    }

    /// 某次决策关联的成交。
    pub fn trades_for_decision(&self, decision_id: i64) -> Result<Vec<RecentTrade>> {
        let mut stmt = self.conn.prepare(
            "SELECT thscode, name, side, price, quantity, amount, fee, ts_ms
             FROM trades WHERE decision_id = ? ORDER BY ts_ms",
        )?;
        let mut rows = stmt.query(duckdb::params![decision_id])?;
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
                decision_id: Some(decision_id),
            });
        }
        Ok(out)
    }

    /// 最近 N 条成交流水（时间倒序）。
    pub fn recent_trades(&self, limit: u32) -> Result<Vec<RecentTrade>> {
        let mut stmt = self.conn.prepare(
            "SELECT thscode, name, side, price, quantity, amount, fee, ts_ms, decision_id
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
                decision_id: r.get(8)?,
            });
        }
        Ok(out)
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
