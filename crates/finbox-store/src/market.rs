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

/// 初筛因子行（由 `market_screen_rows` 单条 SQL 产出）。
#[derive(Debug, Clone)]
pub struct ScreenRow {
    pub thscode: String,
    pub price: f64,
    pub pct: f64,
    pub turnover: f64,
    pub ma20: f64,
    pub ma60: f64,
    /// 近 5 日涨幅（%）
    pub chg5: f64,
    pub volume_ratio: Option<f64>,
    /// 当前价在 60 日高低点位置（0~1）
    pub position: Option<f64>,
}

impl Db {
    /// 全市场初筛行：单条 SQL 用窗口函数计算全部打分因子，避免逐只查询。
    ///
    /// 对每只股票返回：最新快照价/涨幅/成交额 + MA20/MA60/60日高低点位置/近5日涨幅/量比。
    pub fn market_screen_rows(&self) -> Result<Vec<ScreenRow>> {
        let mut stmt = self.conn.prepare(
            r#"WITH ranked AS (
                SELECT thscode, date_ms, close_price, high_price, low_price, volume, turnover,
                       ROW_NUMBER() OVER (PARTITION BY thscode ORDER BY date_ms DESC) AS rn,
                       COUNT(*)  OVER (PARTITION BY thscode) AS total_bars
                FROM daily_bars
            ),
            per AS (
                SELECT thscode,
                       MAX(CASE WHEN rn = 1 THEN close_price END) AS last_close,
                       MAX(CASE WHEN rn = 1 THEN turnover END) AS last_turnover,
                       MAX(total_bars) AS total_bars,
                       SUM(CASE WHEN rn <= 5  THEN close_price END) / 5.0 AS ma5,
                       SUM(CASE WHEN rn <= 20 THEN close_price END) / 20.0 AS ma20,
                       AVG(close_price) AS ma60,
                       MIN(low_price) AS min60,
                       MAX(high_price) AS max60,
                       SUM(CASE WHEN rn = 6 THEN close_price END) AS close6,
                       AVG(CASE WHEN rn BETWEEN 2 AND 6 THEN turnover END) AS avg5_turnover
                FROM ranked
                WHERE rn <= 60
                GROUP BY thscode
            )
            SELECT p.thscode, p.last_close, p.last_turnover, p.ma20, p.ma60,
                   p.min60, p.max60, p.close6, p.avg5_turnover,
                   s.price_change_ratio_pct
            FROM per p
            LEFT JOIN (
                SELECT thscode, price_change_ratio_pct, turnover
                FROM snapshots
                QUALIFY ROW_NUMBER() OVER (PARTITION BY thscode ORDER BY ts_ms DESC) = 1
            ) s ON p.thscode = s.thscode
            WHERE p.total_bars >= 60 AND p.last_close IS NOT NULL"#,
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let last_close: Option<f64> = r.get(1)?;
            let last_turnover: Option<f64> = r.get(2)?;
            let ma20: Option<f64> = r.get(3)?;
            let ma60: Option<f64> = r.get(4)?;
            let min60: Option<f64> = r.get(5)?;
            let max60: Option<f64> = r.get(6)?;
            let close6: Option<f64> = r.get(7)?;
            let avg5_turnover: Option<f64> = r.get(8)?;
            let pct: Option<f64> = r.get(9)?;
            let Some(last_close) = last_close else { continue };
            let pct = pct.unwrap_or(0.0);
            let chg5 = close6.map(|c6| (last_close / c6 - 1.0) * 100.0).unwrap_or(0.0);
            let volume_ratio = match (avg5_turnover, last_turnover) {
                (Some(a), Some(l)) if a > 0.0 => Some(l / a),
                _ => None,
            };
            let position = match (min60, max60) {
                (Some(lo), Some(hi)) if hi > lo => Some((last_close - lo) / (hi - lo)),
                _ => None,
            };
            out.push(ScreenRow {
                thscode: r.get(0)?,
                price: last_close,
                pct,
                turnover: last_turnover.unwrap_or(0.0),
                ma20: ma20.unwrap_or(0.0),
                ma60: ma60.unwrap_or(0.0),
                chg5,
                volume_ratio,
                position,
            });
        }
        Ok(out)
    }

    /// 市场涨跌家数：上涨家数 / 总家数（基于最新快照涨跌幅）。
    pub fn market_breadth(&self) -> Result<(u32, u32)> {
        let (up, total) = self.conn.query_row(
            "SELECT
                COUNT(*) FILTER (WHERE pct > 0),
                COUNT(*)
             FROM (
                SELECT s.price_change_ratio_pct AS pct
                FROM snapshots s
                JOIN (SELECT thscode, MAX(ts_ms) AS m FROM snapshots GROUP BY thscode) t
                  ON s.thscode = t.thscode AND s.ts_ms = t.m
                WHERE s.last_price > 0
             )",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )?;
        Ok((up as u32, total as u32))
    }

    /// 涨跌分布（最新快照分桶）：涨停(>=9.8%) / >5% / 0~5% / 平(±0.1%) / -5~0% / <-5% / 跌停(<=-9.8%)。
    pub fn market_distribution(&self) -> Result<Vec<(String, u32)>> {
        let mut stmt = self.conn.prepare(
            "SELECT bucket, COUNT(*) FROM (
                SELECT CASE
                    WHEN s.price_change_ratio_pct >= 9.8 THEN '涨停'
                    WHEN s.price_change_ratio_pct >= 5 THEN '>5%'
                    WHEN s.price_change_ratio_pct > 0.1 THEN '0~5%'
                    WHEN s.price_change_ratio_pct >= -0.1 THEN '平'
                    WHEN s.price_change_ratio_pct > -5 THEN '-5~0%'
                    WHEN s.price_change_ratio_pct > -9.8 THEN '<-5%'
                    ELSE '跌停' END AS bucket
                FROM snapshots s
                JOIN (SELECT thscode, MAX(ts_ms) AS m FROM snapshots GROUP BY thscode) t
                  ON s.thscode = t.thscode AND s.ts_ms = t.m
                WHERE s.last_price > 0
             ) GROUP BY bucket",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u32));
        }
        Ok(out)
    }

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
