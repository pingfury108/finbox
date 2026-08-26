//! 前复权日K：以最新价为基准，历史价格按除权事件递推回调。
//!
//! 公式（忽略配股，事件按时间从早到晚依次应用）：
//!   除权事件 (D=每股分红, B=每股送股比例) 之前的所有历史价：
//!     price' = (price - D) / (1 + B)   （开/高/低/收同调）
//!     volume' = volume × (1 + B)       （股本变大，成交股数放大）
//!     turnover 不变（金额真实）
//!
//! 原始表 `daily_bars` 不动（交易价格必须是真实价）；AI 分析读 `adj_daily_bars`。

use crate::{Db, Result};

/// 单票原始日K行。
struct RawBar {
    date_ms: i64,
    date: String,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
    turnover: f64,
}

impl Db {
    /// 单票前复权全量重算（删旧行再写入）。返回写入行数。
    pub fn rebuild_adj_bars_for(&self, thscode: &str) -> Result<u64> {
        // 1. 原始日K（时间正序）
        let mut stmt = self.conn.prepare(
            "SELECT date_ms, date, open_price, high_price, low_price, close_price, volume, turnover
             FROM daily_bars WHERE thscode = ? ORDER BY date_ms",
        )?;
        let mut rows = stmt.query(duckdb::params![thscode])?;
        let mut bars: Vec<RawBar> = Vec::new();
        while let Some(r) = rows.next()? {
            bars.push(RawBar {
                date_ms: r.get(0)?,
                date: r.get(1)?,
                open: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                high: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                low: r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                close: r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                volume: r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
                turnover: r.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
            });
        }
        if bars.is_empty() {
            return Ok(0);
        }

        // 2. 除权事件（时间正序）
        let events = self.adjustments_for(thscode, i64::MAX)?;

        // 3. 递推调整：事件从早到晚，调整 ex_date 之前的所有 bar
        for (ex_date, dividend, bonus, _ar, _ap) in &events {
            if *dividend == 0.0 && *bonus == 0.0 {
                continue;
            }
            for b in bars.iter_mut().filter(|b| b.date_ms < *ex_date) {
                let adj = |p: f64| (p - dividend) / (1.0 + bonus);
                b.open = adj(b.open);
                b.high = adj(b.high);
                b.low = adj(b.low);
                b.close = adj(b.close);
                b.volume *= 1.0 + bonus;
            }
        }

        // 4. 重写该票的 adj 行
        self.conn.execute("DELETE FROM adj_daily_bars WHERE thscode = ?", duckdb::params![thscode])?;
        let mut app = self.conn.appender("adj_daily_bars")?;
        for b in &bars {
            app.append_row(duckdb::params![
                thscode, b.date_ms, b.date,
                (b.open * 10000.0).round() / 10000.0,
                (b.high * 10000.0).round() / 10000.0,
                (b.low * 10000.0).round() / 10000.0,
                (b.close * 10000.0).round() / 10000.0,
                b.volume.round(),
                b.turnover,
            ])?;
        }
        app.flush()?;
        Ok(bars.len() as u64)
    }

    /// 每日增量更新（收盘后调用）：
    /// - 当天有除权事件的票 → 全量重算该票
    /// - 其他票 → 当日新 bar 原样复制（复权因子=1）
    /// 返回处理行数。
    pub fn adj_bars_daily_update(&self, today_ms: i64) -> Result<u64> {
        // 当天除权的票
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT thscode FROM adjustment_events WHERE ex_date_ms = ?",
        )?;
        let mut rows = stmt.query(duckdb::params![today_ms])?;
        let mut adjusted_codes = Vec::new();
        while let Some(r) = rows.next()? {
            adjusted_codes.push(r.get::<_, String>(0)?);
        }
        let mut total = 0u64;
        for code in &adjusted_codes {
            total += self.rebuild_adj_bars_for(code)?;
        }
        // 非除权票：当日 bar 直接复制
        let n = if adjusted_codes.is_empty() {
            self.conn.execute(
                "INSERT INTO adj_daily_bars
                 SELECT thscode, date_ms, date, open_price, high_price, low_price, close_price, volume, turnover
                 FROM daily_bars WHERE date_ms = ? ON CONFLICT DO NOTHING",
                duckdb::params![today_ms],
            )?
        } else {
            let placeholders = adjusted_codes.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "INSERT INTO adj_daily_bars
                 SELECT thscode, date_ms, date, open_price, high_price, low_price, close_price, volume, turnover
                 FROM daily_bars WHERE date_ms = ? AND thscode NOT IN ({placeholders}) ON CONFLICT DO NOTHING"
            );
            let mut params: Vec<Box<dyn duckdb::ToSql>> = vec![Box::new(today_ms)];
            for c in &adjusted_codes {
                params.push(Box::new(c.clone()));
            }
            let pref: Vec<&dyn duckdb::ToSql> = params.iter().map(|p| p.as_ref()).collect();
            self.conn.execute(&sql, pref.as_slice())?
        };
        Ok(total + n as u64)
    }

    /// 前复权表是否为空（判断是否需要全量重建）。
    pub fn adj_bars_empty(&self) -> Result<bool> {
        let v: i64 = self.conn.query_row("SELECT COUNT(*) FROM adj_daily_bars", [], |r| r.get(0))?;
        Ok(v == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_market_shared;

    #[test]
    fn adj_forward_adjust() {
        let db = open_market_shared(":memory:").unwrap();
        let m = db.lock().unwrap();
        // 3 根日K：D1=20元, D2=21元, D3=10.5元（D3 为除权日：10送10 后价格腰斩）
        for (i, (ms, close)) in [(1000i64, 20.0), (2000i64, 21.0), (3000i64, 10.5)].iter().enumerate() {
            m.conn().execute(
                "INSERT INTO daily_bars VALUES ('600000.SH', ?, ?, ?, ?, ?, ?, 1000, 10000)",
                duckdb::params![ms, format!("2026010{i}"), close, close, close, close],
            ).unwrap();
        }
        // 除权事件：D3 送股 10送10（B=1.0）
        m.conn().execute(
            "INSERT INTO adjustment_events VALUES ('600000.SH', 3000, 0.0, 1.0, NULL, NULL)",
            [],
        ).unwrap();
        let n = m.rebuild_adj_bars_for("600000.SH").unwrap();
        assert_eq!(n, 3);
        // 前复权后：D1 (20)/2=10, D2 21/2=10.5, D3 不变 10.5 —— 序列连续
        let closes: Vec<f64> = {
            let mut stmt = m.conn().prepare(
                "SELECT close_price FROM adj_daily_bars WHERE thscode='600000.SH' ORDER BY date_ms").unwrap();
            let mut rows = stmt.query([]).unwrap();
            let mut v = Vec::new();
            while let Some(r) = rows.next().unwrap() {
                v.push(r.get::<_, f64>(0).unwrap());
            }
            v
        };
        assert!((closes[0] - 10.0).abs() < 0.01);
        assert!((closes[1] - 10.5).abs() < 0.01);
        assert!((closes[2] - 10.5).abs() < 0.01);
        // 量放大一倍
        let vol: f64 = m.conn().query_row(
            "SELECT volume FROM adj_daily_bars WHERE thscode='600000.SH' AND date_ms=1000", [], |r| r.get(0)).unwrap();
        assert!((vol - 2000.0).abs() < 0.1);
    }
}
