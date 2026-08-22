//! finbox-store 市场数据查询：决策上下文与初筛用。

use crate::{Db, Result};

/// 单只股票行情摘要（最新快照 + 近期日K统计）。
#[derive(Debug, Clone)]
pub struct QuoteSummary {
    pub thscode: String,
    /// 最新快照价
    pub last_price: Option<f64>,
    /// 涨跌幅 %（快照）
    pub pct: Option<f64>,
    /// 量比（暂无源，用换手近似占位）
    pub prev_close: Option<f64>,
}

/// 全市场最新快照（每只取 ts 最新一条）。
pub struct SnapshotView {
    pub thscode: String,
    pub last_price: f64,
    pub pct: f64,
    pub prev_price: f64,
    pub volume: f64,
    pub turnover: f64,
}

impl Db {
    /// 最新快照（每标的最新一条）。
    pub fn latest_snapshots(&self) -> Result<Vec<SnapshotView>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.thscode, s.last_price, s.price_change_ratio_pct, s.prev_price, s.volume, s.turnover
             FROM snapshots s
             JOIN (SELECT thscode, MAX(ts_ms) AS m FROM snapshots GROUP BY thscode) t
               ON s.thscode = t.thscode AND s.ts_ms = t.m
             WHERE s.last_price > 0
             ORDER BY s.thscode",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(SnapshotView {
                thscode: r.get(0)?,
                last_price: r.get(1)?,
                pct: r.get(2)?,
                prev_price: r.get(3)?,
                volume: r.get(4)?,
                turnover: r.get(5)?,
            });
        }
        Ok(out)
    }

    /// 单只股票近 `n` 根日 K（时间正序）。
    pub fn recent_bars(&self, thscode: &str, n: u32) -> Result<Vec<RecentBar>> {
        let mut stmt = self.conn.prepare(
            "SELECT date_ms, open_price, high_price, low_price, close_price, volume
             FROM daily_bars WHERE thscode = ?
             ORDER BY date_ms DESC LIMIT ?",
        )?;
        let mut rows = stmt.query(duckdb::params![thscode, n as i64])?;
        let mut bars: Vec<RecentBar> = Vec::new();
        while let Some(r) = rows.next()? {
            bars.push(RecentBar {
                date_ms: r.get(0)?,
                open: r.get(1)?,
                high: r.get(2)?,
                low: r.get(3)?,
                close: r.get(4)?,
                volume: r.get(5)?,
            });
        }
        bars.reverse();
        Ok(bars)
    }
}

/// 日 K（查询用）。
#[derive(Debug, Clone)]
pub struct RecentBar {
    pub date_ms: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

/// 量比（近 5 日均成交额 vs 当日成交额）。
#[derive(Debug, Clone)]
pub struct VolumeRatio {
    pub thscode: String,
    pub ratio: f64,
}

impl Db {
    /// 全市场量比，单条 SQL（窗口函数），避免逐只查询。
    pub fn market_volume_ratios(&self) -> Result<Vec<VolumeRatio>> {
        let mut stmt = self.conn.prepare(
            "WITH ranked AS (
                 SELECT thscode, date_ms, turnover,
                        ROW_NUMBER() OVER (PARTITION BY thscode ORDER BY date_ms DESC) AS rn
                 FROM daily_bars
             ),
             avg5 AS (
                 SELECT thscode, AVG(turnover) AS avg_turnover
                 FROM ranked WHERE rn > 1 AND rn <= 6
                 GROUP BY thscode
             ),
             latest AS (
                 SELECT thscode, turnover AS last_turnover
                 FROM ranked WHERE rn = 1
             )
             SELECT l.thscode, l.last_turnover / a.avg_turnover AS ratio
             FROM latest l JOIN avg5 a ON l.thscode = a.thscode
             WHERE a.avg_turnover > 0",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(VolumeRatio { thscode: r.get(0)?, ratio: r.get(1)? });
        }
        Ok(out)
    }
}
