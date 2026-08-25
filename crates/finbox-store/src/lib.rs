//! finbox-store：DuckDB 本地行情库。
//!
//! 表结构：
//! - `meta`            键值元数据（同步时间等）
//! - `trading_days`    交易日历（yyyyMMdd + 毫秒时间戳）
//! - `tickers`         标的代码表
//! - `daily_bars`      全市场日 K（未复权，PK: thscode+date_ms）
//! - `adjustment_events` 复权事件（分红/送股/配股）
//! - `snapshots`       盘中行情快照（PK: ts_ms+thscode）
//!
//! 注意：DuckDB 单写多读，写操作应经由唯一写入方（collector/app）串行执行。

use std::path::Path;
use std::sync::{Arc, Mutex};

use duckdb::{params, Config, Connection};

pub mod trading;
pub mod market;
pub mod decision;
pub mod review;

pub use decision::*;
pub use market::*;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, StoreError>;

/// 进程内共享的 DuckDB 句柄。
///
/// DuckDB 单写多读且同进程多连接为独立实例互不可见，故整个应用共享**单一**连接，
/// 写操作经 `Mutex` 串行化。
pub type SharedDb = Arc<Mutex<Db>>;

/// 打开共享句柄（旧格式，建全部表）。
pub fn open_shared(path: impl AsRef<Path>) -> Result<SharedDb> {
    Ok(Arc::new(Mutex::new(Db::open(path)?)))
}

/// 打开共享行情库句柄。
pub fn open_market_shared(path: impl AsRef<Path>) -> Result<SharedDb> {
    Ok(Arc::new(Mutex::new(Db::open_market(path)?)))
}

/// parquet 扩展自举：先试本地 LOAD；失败则自动下载安装（curl 30s 超时，防卡死）后再 LOAD。
///
/// 背景：DuckDB 默认 autoload 会连 extensions.duckdb.org（Cloudflare）下载扩展，
/// 国内网络下无超时挂起导致进程卡死。因此 autoload 已全局关闭，改由本函数显式管理：
/// 本地扩展文件位于 ~/.duckdb/extensions/v{version}/{platform}/，缺失时显式下载（带超时）。
fn ensure_parquet_extension(conn: &duckdb::Connection) -> Result<()> {
    if conn.execute_batch("LOAD parquet;").is_ok() {
        return Ok(());
    }
    install_parquet_extension(conn)?;
    conn.execute_batch("LOAD parquet;")
        .map_err(|e| StoreError::Extension(format!("下载后仍无法加载: {e}")))?;
    Ok(())
}

/// 下载并安装 parquet 扩展到本地扩展目录。curl/gunzip 走系统命令（自动读代理环境变量）。
fn install_parquet_extension(conn: &duckdb::Connection) -> Result<()> {
    let version: String = conn
        .query_row("SELECT version()", [], |r| r.get(0))
        .map_err(StoreError::Duckdb)?;
    let version = version.trim().trim_start_matches('v');
    let platform = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux_amd64",
        ("linux", "aarch64") => "linux_arm64",
        ("macos", "aarch64") => "osx_arm64",
        ("macos", "x86_64") => "osx_amd64",
        ("windows", "x86_64") => "windows_amd64",
        (os, arch) => return Err(StoreError::Extension(format!("不支持的平台 {os}/{arch}"))),
    };
    let home = std::env::var("HOME").map_err(|_| StoreError::Extension("HOME 未设置".into()))?;
    let dir = format!("{home}/.duckdb/extensions/v{version}/{platform}");
    std::fs::create_dir_all(&dir)?;
    let gz = format!("{dir}/parquet.duckdb_extension.gz");
    let url = format!("https://extensions.duckdb.org/v{version}/{platform}/parquet.duckdb_extension.gz");
    log::info!("下载 parquet 扩展: {url}");
    let ok = std::process::Command::new("curl")
        .args(["-sfL", "--max-time", "30", "-o", &gz, &url])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        return Err(StoreError::Extension(format!(
            "下载失败（30s 超时或网络不可达）: {url}。可手动下载后放入 {dir}/ 并 gunzip"
        )));
    }
    let ok = std::process::Command::new("gunzip")
        .args(["-f", &gz])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        return Err(StoreError::Extension(format!("gunzip 解压失败: {gz}")));
    }
    log::info!("parquet 扩展已安装到 {dir}");
    Ok(())
}

/// 打开共享账户库句柄。
pub fn open_account_shared(path: impl AsRef<Path>) -> Result<SharedDb> {
    Ok(Arc::new(Mutex::new(Db::open_account(path)?)))
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("DuckDB 错误: {0}")]
    Duckdb(#[from] duckdb::Error),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("parquet 扩展不可用: {0}")]
    Extension(String),
}

/// 标的代码行。
#[derive(Debug, Clone)]
pub struct TickerRow {
    pub thscode: String,
    pub ticker: String,
    pub name: String,
    pub exchange: Option<String>,
    pub asset_type: String,
    pub currency: String,
}

/// 交易日行（`date` 为 `yyyyMMdd`）。
#[derive(Debug, Clone)]
pub struct TradingDayRow {
    pub date: String,
    pub date_ms: i64,
}

/// 日 K 行（已推导出 `date` 为 `yyyy-MM-dd`）。
#[derive(Debug, Clone)]
pub struct DailyBarRow {
    pub thscode: String,
    pub date_ms: i64,
    pub date: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub turnover: f64,
}
/// 盘中行情快照行。
#[derive(Debug, Clone)]
pub struct SnapshotRow {
    pub thscode: String,
    pub last_price: f64,
    pub price_change: f64,
    pub price_change_ratio_pct: f64,
    pub open_price: f64,
    pub high_price: f64,
    pub low_price: f64,
    pub prev_price: f64,
    pub volume: f64,
    pub turnover: f64,
}

/// 库内统计。
#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub tickers: i64,
    pub trading_days: i64,
    pub daily_bars: i64,
    pub adjustment_events: i64,
    pub snapshots: i64,
    /// 最新日 K 日期（`yyyy-MM-dd`）
    pub last_bar_date: Option<String>,
    /// 最新快照时间戳（毫秒）
    pub last_snapshot_ts: Option<i64>,
}

/// 行情库表结构（共享，只读为主）。
const MARKET_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
    key   VARCHAR PRIMARY KEY,
    value VARCHAR
);
CREATE TABLE IF NOT EXISTS trading_days (
    date    VARCHAR PRIMARY KEY,
    date_ms BIGINT NOT NULL
);
CREATE TABLE IF NOT EXISTS tickers (
    thscode    VARCHAR PRIMARY KEY,
    ticker     VARCHAR NOT NULL,
    name       VARCHAR NOT NULL,
    exchange   VARCHAR,
    asset_type VARCHAR NOT NULL,
    currency   VARCHAR NOT NULL
);
CREATE TABLE IF NOT EXISTS daily_bars (
    thscode VARCHAR NOT NULL,
    date_ms BIGINT NOT NULL,
    date    VARCHAR NOT NULL,
    open_price  DOUBLE,
    high_price  DOUBLE,
    low_price   DOUBLE,
    close_price DOUBLE,
    volume      DOUBLE,
    turnover    DOUBLE,
    PRIMARY KEY (thscode, date_ms)
);
CREATE TABLE IF NOT EXISTS adjustment_events (
    thscode             VARCHAR NOT NULL,
    ex_date_ms          BIGINT NOT NULL,
    dividend_per_share  DOUBLE NOT NULL DEFAULT 0,
    per_share_bonus     DOUBLE NOT NULL DEFAULT 0,
    allotment_ratio     DOUBLE,
    allotment_price     DOUBLE,
    PRIMARY KEY (thscode, ex_date_ms)
);
CREATE TABLE IF NOT EXISTS snapshots (
    ts_ms   BIGINT NOT NULL,
    thscode VARCHAR NOT NULL,
    last_price             DOUBLE NOT NULL,
    price_change           DOUBLE,
    price_change_ratio_pct DOUBLE,
    open_price             DOUBLE,
    high_price             DOUBLE,
    low_price              DOUBLE,
    prev_price             DOUBLE,
    volume                 DOUBLE,
    turnover               DOUBLE,
    PRIMARY KEY (ts_ms, thscode)
);
CREATE INDEX IF NOT EXISTS ix_snapshots_thscode_ts ON snapshots (thscode, ts_ms);
"#;

/// 账户库表结构（每账户独立，读写）。
const ACCOUNT_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
    key   VARCHAR PRIMARY KEY,
    value VARCHAR
);
CREATE TABLE IF NOT EXISTS account (
    id               INTEGER PRIMARY KEY,
    cash             DOUBLE NOT NULL,
    initial_capital  DOUBLE NOT NULL
);
CREATE TABLE IF NOT EXISTS positions (
    thscode    VARCHAR PRIMARY KEY,
    name       VARCHAR NOT NULL,
    quantity   INTEGER NOT NULL,
    avg_cost   DOUBLE NOT NULL
);
CREATE SEQUENCE IF NOT EXISTS acct_id_seq;
CREATE TABLE IF NOT EXISTS trades (
    id          INTEGER PRIMARY KEY DEFAULT nextval('acct_id_seq'),
    thscode     VARCHAR NOT NULL,
    name        VARCHAR NOT NULL,
    side        VARCHAR(4) NOT NULL,
    price       DOUBLE NOT NULL,
    quantity    INTEGER NOT NULL,
    amount      DOUBLE NOT NULL,
    fee         DOUBLE NOT NULL DEFAULT 0,
    decision_id INTEGER,
    ts_ms       BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_trades_thscode_ts ON trades (thscode, ts_ms);
CREATE TABLE IF NOT EXISTS decision_logs (
    id           INTEGER PRIMARY KEY DEFAULT nextval('acct_id_seq'),
    ts_ms        BIGINT NOT NULL,
    model        VARCHAR NOT NULL,
    context      VARCHAR,
    raw_response VARCHAR,
    actions      VARCHAR,
    status       VARCHAR NOT NULL,
    note         VARCHAR
);
CREATE TABLE IF NOT EXISTS account_snapshots (
    id           INTEGER PRIMARY KEY DEFAULT nextval('acct_id_seq'),
    ts_ms        BIGINT NOT NULL,
    cash         DOUBLE NOT NULL,
    market_value DOUBLE NOT NULL,
    total_asset  DOUBLE NOT NULL
);
CREATE TABLE IF NOT EXISTS reviews (
    id          INTEGER PRIMARY KEY DEFAULT nextval('acct_id_seq'),
    decision_id INTEGER NOT NULL,
    days_after  INTEGER NOT NULL,
    ts_ms       BIGINT NOT NULL,
    summary     VARCHAR,
    pnl         DOUBLE
);
"#;

/// DuckDB 本地行情库。
pub struct Db {
    conn: Connection,
}

impl Db {
    /// 打开行情库（共享只读：tickers/日K/快照/交易日历/全局配置）。
    pub fn open_market(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_schema(path, MARKET_SCHEMA)
    }

    /// 打开账户库（每账户独立：账户/持仓/流水/决策/快照/复盘/账户配置）。
    pub fn open_account(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_schema(path, ACCOUNT_SCHEMA)
    }

    /// 兼容旧入口：建全部表（单库旧格式）。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        // 关闭扩展自动加载/安装：DuckDB 首次使用会尝试连 extensions.duckdb.org 拉扩展，
        // 在国内网络/代理环境下会长时间卡死（实测挂起于 Cloudflare :80）
        let config = Config::default().enable_autoload_extension(false)?;
        let conn = if path.as_os_str() == ":memory:" {
            Connection::open_in_memory_with_flags(config)?
        } else {
            Connection::open_with_flags(path, config)?
        };
        // parquet 扩展自举：LOAD 失败自动下载安装（日K导入依赖，失败仅告警不阻塞启动）
        if let Err(e) = ensure_parquet_extension(&conn) {
            log::warn!("parquet 扩展不可用: {e}（日K parquet 导入将失败）");
        }
        let _ = conn.execute_batch(MARKET_SCHEMA);
        let _ = conn.execute_batch(ACCOUNT_SCHEMA);
        Ok(Self { conn })
    }

    /// 从旧库（混合格式）迁移行情表到当前库（market 库用）。
    /// 使用 ATTACH + INSERT SELECT 跨库拷贝，行情表全量迁移。
    pub fn execute_migrate(&self, source: impl AsRef<Path>) -> Result<()> {
        let src = sql_str(source.as_ref());
        self.conn.execute_batch(&format!(
            "ATTACH {src} AS olddb (READ_ONLY);
             INSERT INTO tickers SELECT thscode, ticker, name, exchange, asset_type, currency FROM olddb.tickers ON CONFLICT DO NOTHING;
             INSERT INTO trading_days SELECT date, date_ms FROM olddb.trading_days ON CONFLICT DO NOTHING;
             INSERT INTO daily_bars SELECT thscode, date_ms, date, open_price, high_price, low_price, close_price, volume, turnover FROM olddb.daily_bars ON CONFLICT DO NOTHING;
             INSERT INTO snapshots SELECT ts_ms, thscode, last_price, price_change, price_change_ratio_pct, open_price, high_price, low_price, prev_price, volume, turnover FROM olddb.snapshots ON CONFLICT DO NOTHING;
             INSERT INTO adjustment_events SELECT thscode, ex_date_ms, dividend_per_share, per_share_bonus, allotment_ratio, allotment_price FROM olddb.adjustment_events ON CONFLICT DO NOTHING;
             DETACH olddb;"
        ))?;
        Ok(())
    }

    /// 表行数（迁移验证用）。
    pub fn count_rows(&self, table: &str) -> Result<i64> {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        Ok(self.conn.query_row(&sql, [], |r| r.get::<_, i64>(0))?)
    }

    fn open_with_schema(path: impl AsRef<Path>, schema: &str) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        // 关闭扩展自动加载/安装：DuckDB 首次使用会尝试连 extensions.duckdb.org 拉扩展，
        // 在国内网络/代理环境下会长时间卡死（实测挂起于 Cloudflare :80）
        let config = Config::default().enable_autoload_extension(false)?;
        let conn = if path.as_os_str() == ":memory:" {
            Connection::open_in_memory_with_flags(config)?
        } else {
            Connection::open_with_flags(path, config)?
        };
        // parquet 扩展自举（账户库等不依赖 parquet，失败静默降级）
        let _ = ensure_parquet_extension(&conn);
        conn.execute_batch(schema)?;
        Ok(Self { conn })
    }

    /// 代码表 upsert，返回写入行数。
    pub fn upsert_tickers(&self, rows: &[TickerRow]) -> Result<u64> {
        with_tx(&self.conn, |conn| {
            let mut stmt = conn.prepare(
                "INSERT INTO tickers VALUES (?, ?, ?, ?, ?, ?)
                 ON CONFLICT (thscode) DO UPDATE SET
                    ticker = excluded.ticker, name = excluded.name,
                    exchange = excluded.exchange, asset_type = excluded.asset_type,
                    currency = excluded.currency",
            )?;
            for r in rows {
                stmt.execute(params![
                    r.thscode, r.ticker, r.name, r.exchange, r.asset_type, r.currency
                ])?;
            }
            Ok(rows.len() as u64)
        })
    }

    /// 交易日历 upsert，返回写入行数。
    pub fn upsert_trading_days(&self, rows: &[TradingDayRow]) -> Result<u64> {
        with_tx(&self.conn, |conn| {
            let mut stmt = conn.prepare(
                "INSERT INTO trading_days VALUES (?, ?)
                 ON CONFLICT (date) DO NOTHING",
            )?;
            for r in rows {
                stmt.execute(params![r.date, r.date_ms])?;
            }
            Ok(rows.len() as u64)
        })
    }

    /// 批量插入日 K（测试/手工用）。
    pub fn insert_daily_bars(&self, rows: &[DailyBarRow]) -> Result<u64> {
        with_tx(&self.conn, |conn| {
            let mut stmt = conn.prepare(
                "INSERT INTO daily_bars VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (thscode, date_ms) DO UPDATE SET
                    date = excluded.date, open_price = excluded.open_price,
                    high_price = excluded.high_price, low_price = excluded.low_price,
                    close_price = excluded.close_price, volume = excluded.volume,
                    turnover = excluded.turnover",
            )?;
            for r in rows {
                stmt.execute(params![
                    r.thscode, r.date_ms, r.date, r.open, r.high, r.low, r.close, r.volume, r.turnover
                ])?;
            }
            Ok(rows.len() as u64)
        })
    }

    /// 插入一批同时间戳的行情快照（按 PK 去重），返回写入行数。
    pub fn insert_snapshots(&self, ts_ms: i64, rows: &[SnapshotRow]) -> Result<u64> {
        with_tx(&self.conn, |conn| {
            let mut stmt = conn.prepare(
                "INSERT INTO snapshots VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT DO NOTHING",
            )?;
            for r in rows {
                stmt.execute(params![
                    ts_ms,
                    r.thscode,
                    r.last_price,
                    r.price_change,
                    r.price_change_ratio_pct,
                    r.open_price,
                    r.high_price,
                    r.low_price,
                    r.prev_price,
                    r.volume,
                    r.turnover,
                ])?;
            }
            Ok(rows.len() as u64)
        })
    }

    /// 导入全市场日 K Parquet（未复权），按 (thscode, date_ms) UPSERT，返回写入行数。
    ///
    /// `date` 列由 `date_ms`（Asia/Shanghai 零点毫秒）加 8h 偏移推导为 `yyyy-MM-dd`。
    pub fn import_daily_k_parquet(&self, parquet: &Path) -> Result<u64> {        let p = sql_str(parquet);
        let sql = format!(
            "INSERT INTO daily_bars
             SELECT thscode, date_ms,
                    strftime(make_timestamp((date_ms + 28800000) * 1000), '%Y-%m-%d'),
                    open_price, high_price, low_price, close_price, volume, turnover
             FROM read_parquet({p})
             ON CONFLICT (thscode, date_ms) DO UPDATE SET
                open_price = excluded.open_price, high_price = excluded.high_price,
                low_price = excluded.low_price, close_price = excluded.close_price,
                volume = excluded.volume, turnover = excluded.turnover"
        );
        Ok(self.conn.execute(&sql, [])? as u64)
    }

    /// 导入全市场复权事件 Parquet，返回写入行数。
    pub fn import_adjustment_factors_parquet(&self, parquet: &Path) -> Result<u64> {
        let p = sql_str(parquet);
        let sql = format!(
            "INSERT INTO adjustment_events
             SELECT thscode, ex_date_ms, dividend_per_share, per_share_bonus,
                    allotment_ratio, allotment_price
             FROM read_parquet({p})
             ON CONFLICT (thscode, ex_date_ms) DO UPDATE SET
                dividend_per_share = excluded.dividend_per_share,
                per_share_bonus = excluded.per_share_bonus,
                allotment_ratio = excluded.allotment_ratio,
                allotment_price = excluded.allotment_price"
        );
        Ok(self.conn.execute(&sql, [])? as u64)
    }

    /// 最新日 K 日期（`yyyy-MM-dd`）。
    pub fn last_bar_date(&self) -> Result<Option<String>> {
        let v = self.conn.query_row(
            "SELECT max(date) FROM daily_bars",
            [],
            |r| r.get::<_, Option<String>>(0),
        )?;
        Ok(v)
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta VALUES (?, ?)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare("SELECT value FROM meta WHERE key = ?")?;
        let mut rows = stmt.query(params![key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    pub fn stats(&self) -> Result<Stats> {
        let count = |sql: &str| -> Result<i64> {
            Ok(self.conn.query_row(sql, [], |r| r.get::<_, i64>(0))?)
        };
        let last_bar_date = self.conn.query_row(
            "SELECT max(date) FROM daily_bars",
            [],
            |r| r.get::<_, Option<String>>(0),
        )?;
        let last_snapshot_ts = self.conn.query_row(
            "SELECT max(ts_ms) FROM snapshots",
            [],
            |r| r.get::<_, Option<i64>>(0),
        )?;
        Ok(Stats {
            tickers: count("SELECT count(*) FROM tickers")?,
            trading_days: count("SELECT count(*) FROM trading_days")?,
            daily_bars: count("SELECT count(*) FROM daily_bars")?,
            adjustment_events: count("SELECT count(*) FROM adjustment_events")?,
            snapshots: count("SELECT count(*) FROM snapshots")?,
            last_bar_date,
            last_snapshot_ts,
        })
    }
}

fn with_tx<T>(conn: &Connection, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    conn.execute_batch("BEGIN")?;
    match f(conn) {
        Ok(v) => {
            conn.execute_batch("COMMIT")?;
            Ok(v)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

fn sql_str(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open(":memory:").unwrap()
    }

    fn ticker(thscode: &str, name: &str) -> TickerRow {
        TickerRow {
            thscode: thscode.into(),
            ticker: thscode.split('.').next().unwrap().into(),
            name: name.into(),
            exchange: Some("SH".into()),
            asset_type: "a-share".into(),
            currency: "CNY".into(),
        }
    }

    fn snapshot(thscode: &str, last: f64) -> SnapshotRow {
        SnapshotRow {
            thscode: thscode.into(),
            last_price: last,
            price_change: 0.0,
            price_change_ratio_pct: 0.0,
            open_price: last,
            high_price: last,
            low_price: last,
            prev_price: last,
            volume: 100.0,
            turnover: 100.0 * last,
        }
    }

    #[test]
    fn date_ms_to_date_sql() {
        // 2025-01-01 00:00 Asia/Shanghai = 1735660800000
        let d: String = Db::open(":memory:")
            .unwrap()
            .conn
            .query_row(
                "SELECT strftime(make_timestamp((1735660800000 + 28800000) * 1000), '%Y-%m-%d')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(d, "2025-01-01");
    }

    #[test]
    fn upsert_tickers() {
        let db = db();
        db.upsert_tickers(&[ticker("600519.SH", "贵州茅台")]).unwrap();
        let mut renamed = ticker("600519.SH", "新名字");
        renamed.name = "贵州茅台".into();
        db.upsert_tickers(&[renamed]).unwrap();
        let s = db.stats().unwrap();
        assert_eq!(s.tickers, 1);
    }

    #[test]
    fn trading_days_and_snapshots() {
        let db = db();
        db.upsert_trading_days(&[TradingDayRow { date: "20250102".into(), date_ms: 1 }])
            .unwrap();
        db.insert_snapshots(100, &[snapshot("600519.SH", 1700.0)]).unwrap();
        // 重复插入同批数据：PK 去重，不报错
        db.insert_snapshots(100, &[snapshot("600519.SH", 1700.0)]).unwrap();
        let s = db.stats().unwrap();
        assert_eq!(s.trading_days, 1);
        assert_eq!(s.snapshots, 1);
        assert_eq!(s.last_snapshot_ts, Some(100));
        assert_eq!(s.last_bar_date, None);
    }

    #[test]
    fn meta_roundtrip() {
        let db = db();
        assert_eq!(db.meta_get("k").unwrap(), None);
        db.meta_set("k", "v1").unwrap();
        db.meta_set("k", "v2").unwrap();
        assert_eq!(db.meta_get("k").unwrap(), Some("v2".to_string()));
    }
}
