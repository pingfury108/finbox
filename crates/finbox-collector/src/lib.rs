//! finbox-collector：同花顺数据采集同步到本地 DuckDB。
//!
//! 建库/增量优先走 Market Dumps（Parquet），不做逐股 REST 拉取。

use std::path::Path;

use anyhow::Result;
use finbox_store::{DailyBarRow, SharedDb, SnapshotRow, TickerRow, TradingDayRow};
use log::info;
use hithink_sdk::{Client, DumpKind, TradingDaysData};

/// 增量 dump 覆盖窗口（交易日）。落后超过此交易日数时必须全量重拉。
const DAILY_K_INCREMENT_WINDOW: usize = 7;

/// 快照分页每页条数。
const SNAPSHOT_PAGE_LIMIT: u32 = 1000;

/// 代码表分页每页条数。
const TICKER_PAGE_LIMIT: u32 = 10000;

pub struct Collector {
    pub client: Client,
    pub db: SharedDb,
}

impl Collector {
    pub fn new(client: Client, db: SharedDb) -> Self {
        Self { client, db }
    }

    /// 同步 A 股代码表（沪深京），返回标的数量。
    pub async fn sync_tickers(&self) -> Result<usize> {
        let mut all: Vec<TickerRow> = Vec::new();
        let mut offset = 0u32;
        loop {
            let page = self
                .client
                .list_tickers(Some("SH,SZ,BJ"), Some("a-share"), TICKER_PAGE_LIMIT, offset)
                .await?;
            let n = page.item.len();
            all.extend(page.item.into_iter().map(|t| TickerRow {
                thscode: t.thscode,
                ticker: t.ticker,
                name: t.name,
                exchange: t.exchange,
                asset_type: t.asset_type,
                currency: t.currency,
            }));
            if n < TICKER_PAGE_LIMIT as usize {
                break;
            }
            offset += TICKER_PAGE_LIMIT;
        }
        let n = all.len();
        self.db.lock().unwrap().upsert_tickers(&all)?;
        info!("[数据] 代码表同步完成: {n} 只标的");
        Ok(n)
    }

    /// 拉取并落库交易日历，返回交易日数量。
    pub async fn sync_trading_days(&self) -> Result<usize> {
        let data = self.client.trading_days().await?;
        self.upsert_trading_days(&data).await
    }

    /// 落库已拉取的交易日历，返回写入行数。
    pub async fn upsert_trading_days(&self, data: &TradingDaysData) -> Result<usize> {
        let rows: Vec<TradingDayRow> = data
            .item
            .iter()
            .map(|d| TradingDayRow { date: d.date.clone(), date_ms: d.date_ms })
            .collect();
        let n = rows.len();
        self.db.lock().unwrap().upsert_trading_days(&rows)?;
        info!("[数据] 交易日历同步完成: {n} 天");
        Ok(n)
    }

    /// 全市场 10 年日 K 全量导入（未复权），返回写入行数。
    pub async fn import_daily_k_full(&self, dump_dir: &Path) -> Result<u64> {
        let dest = dump_dir.join(DumpKind::DailyK.default_filename());
        self.client.download_dump(DumpKind::DailyK, &dest).await?;
        let n = self.db.lock().unwrap().import_daily_k_parquet(&dest)?;
        self.db.lock().unwrap().meta_set("last_daily_k_full_sync", &now_ms().to_string())?;
        info!("[数据] 全量日K导入完成: {n} 行 (全市场10年)");
        Ok(n)
    }

    /// 全市场复权事件导入，返回写入行数。
    pub async fn import_adjustment_factors(&self, dump_dir: &Path) -> Result<u64> {
        let dest = dump_dir.join(DumpKind::AdjustmentFactors.default_filename());
        self.client.download_dump(DumpKind::AdjustmentFactors, &dest).await?;
        let n = self.db.lock().unwrap().import_adjustment_factors_parquet(&dest)?;
        self.db.lock().unwrap().meta_set("last_adjustment_sync", &now_ms().to_string())?;
        info!("[数据] 复权事件导入完成: {n} 行");
        Ok(n)
    }

    /// 日 K 同步：本地为空或落后超过增量窗口时全量，否则 10 交易日增量。
    /// `calendar` 为刚拉取的交易日历（用于精确计算落后交易日数）。
    pub async fn sync_daily_bars(&self, dump_dir: &Path, calendar: &TradingDaysData) -> Result<u64> {
        let last = self.db.lock().unwrap().last_bar_date()?;
        match last {
            None => self.import_daily_k_full(dump_dir).await,
            Some(last) => {
                if missed_trading_days(&last, calendar) > DAILY_K_INCREMENT_WINDOW {
                    self.import_daily_k_full(dump_dir).await
                } else {
                    let dest = dump_dir.join(DumpKind::DailyK10d.default_filename());
                    self.client.download_dump(DumpKind::DailyK10d, &dest).await?;
                    let n = self.db.lock().unwrap().import_daily_k_parquet(&dest)?;
                    self.db.lock().unwrap().meta_set("last_daily_k_sync", &now_ms().to_string())?;
                    info!("[数据] 日K增量导入完成: {n} 行 (近10交易日)");
                    Ok(n)
                }
            }
        }
    }

    /// 采集几大 A 股指数日 K（复用 daily_bars 表，thscode 天然区分个股/指数）。
    /// 返回写入行数。
    pub async fn sync_index_bars(&self, days: u32) -> Result<u64> {
        // 几大指数：上证/深成/创业板/沪深300/中证500
        const INDEXES: &[(&str, &str)] = &[
            ("000001.SH", "上证指数"),
            ("399001.SZ", "深证成指"),
            ("399006.SZ", "创业板指"),
            ("000300.SH", "沪深300"),
            ("000905.SH", "中证500"),
        ];
        let now_ms = chrono::Utc::now().timestamp_millis();
        let start_ms = now_ms - (days as i64) * 86_400_000;
        let mut total = 0u64;
        for (code, name) in INDEXES {
            let data = self.client.index_price_historical(code, start_ms, now_ms).await?;
            let rows: Vec<DailyBarRow> = data
                .item
                .into_iter()
                .map(|b| DailyBarRow {
                    thscode: code.to_string(),
                    date_ms: b.date_ms,
                    date: fmt_date(b.date_ms),
                    open: b.open_price,
                    high: b.high_price,
                    low: b.low_price,
                    close: b.close_price,
                    volume: b.volume,
                    turnover: b.turnover,
                })
                .collect();
            let n = self.db.lock().unwrap().insert_daily_bars(&rows)?;
            info!("[数据] 指数 {name}({code}) 日K: {n} 根");
            total += n;
        }
        Ok(total)
    }

    /// 采集一次全市场行情快照（分页），返回标的数量。
    pub async fn collect_market_snapshot(&self) -> Result<usize> {
        let mut ts_ms: Option<i64> = None;
        let mut total = 0usize;
        let mut offset = 0u32;
        loop {
            let page = self
                .client
                .price_snapshot(None, Some(SNAPSHOT_PAGE_LIMIT), Some(offset))
                .await?;
            if ts_ms.is_none() {
                ts_ms = Some(page.timestamp.unwrap_or_else(now_ms));
            }
            let rows: Vec<SnapshotRow> = page
                .item
                .into_iter()
                .map(|s| SnapshotRow {
                    thscode: s.thscode,
                    // 停牌/新上市时开高低收、涨跌、量额可能缺失，落库为 0 表示该时点无有效值
                    last_price: s.last_price.unwrap_or(0.0),
                    price_change: s.price_change.unwrap_or(0.0),
                    price_change_ratio_pct: s.price_change_ratio_pct.unwrap_or(0.0),
                    open_price: s.open_price.unwrap_or(0.0),
                    high_price: s.high_price.unwrap_or(0.0),
                    low_price: s.low_price.unwrap_or(0.0),
                    prev_price: s.prev_price.unwrap_or(0.0),
                    volume: s.volume.unwrap_or(0.0),
                    turnover: s.turnover.unwrap_or(0.0),
                })
                .collect();
            let n = rows.len();
            self.db.lock().unwrap().insert_snapshots(ts_ms.unwrap(), &rows)?;
            total += n;
            if n < SNAPSHOT_PAGE_LIMIT as usize {
                break;
            }
            offset += SNAPSHOT_PAGE_LIMIT;
        }
        if let Some(ts) = ts_ms {
            self.db.lock().unwrap().meta_set("last_snapshot_ts", &ts.to_string())?;
        }
        info!("[数据] 全市场快照采集完成: {total} 只");
        Ok(total)
    }
}

/// 本地最新日 K（`yyyy-MM-dd`）之后已过去的交易日数。
fn missed_trading_days(last_bar_date: &str, calendar: &TradingDaysData) -> usize {
    let last = last_bar_date.replace('-', "");
    calendar.item.iter().filter(|d| d.date.as_str() > last.as_str()).count()
}

fn fmt_date(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hithink_sdk::TradingDay;

    fn calendar(dates: &[&str]) -> TradingDaysData {
        TradingDaysData {
            timestamp: 0,
            item: dates
                .iter()
                .map(|d| TradingDay { date_ms: 0, date: (*d).to_string() })
                .collect(),
        }
    }

    #[test]
    fn missed_trading_days_calc() {
        let cal = calendar(&["20250102", "20250103", "20250106", "20250107"]);
        assert_eq!(missed_trading_days("2025-01-03", &cal), 2);
        assert_eq!(missed_trading_days("2025-01-07", &cal), 0);
        assert_eq!(missed_trading_days("2024-12-31", &cal), 4);
    }
}
