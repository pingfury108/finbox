//! finbox-collector CLI：同花顺数据采集同步到本地 DuckDB。

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use finbox_collector::Collector;
use finbox_store::open_shared;
use hithink_sdk::Client;

#[derive(Parser)]
#[command(name = "finbox-collector", about = "同花顺数据采集同步 -> DuckDB")]
struct Cli {
    /// DuckDB 数据库路径（新架构：行情库 market.duckdb）
    #[arg(long, env = "FINBOX_DB", default_value = "data/market.duckdb")]
    db: PathBuf,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 首次全量建库：代码表 + 交易日历 + 全市场 10 年日 K + 复权事件
    Init {
        /// Parquet dump 缓存目录
        #[arg(long, default_value = "data/dumps")]
        dump_dir: PathBuf,
    },
    /// 日常增量：交易日历 + 日 K（落后 >7 交易日自动全量）+ 复权事件
    Sync {
        #[arg(long, default_value = "data/dumps")]
        dump_dir: PathBuf,
    },
    /// 刷新 A 股代码表
    Tickers,
    /// 刷新交易日历
    Calendar,
    /// 采集一次全市场行情快照
    Snapshot,
    /// 采集几大 A 股指数日 K
    Index {
        /// 拉取天数
        #[arg(long, default_value_t = 1200)]
        days: u32,
    },
    /// 库内统计
    Stats,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();

    let client = Client::from_env()?;
    let db = open_shared(&cli.db)?;
    let c = Collector::new(client, db);

    match cli.cmd {
        Cmd::Init { dump_dir } => {
            let n = c.sync_tickers().await?;
            println!("代码表: {n} 只标的");

            let days = c.client.trading_days().await?;
            c.upsert_trading_days(&days).await?;
            println!("交易日历: {} 个交易日", days.item.len());
            c.sync_daily_bars(&dump_dir, &days).await?;

            let n = c.import_adjustment_factors(&dump_dir).await?;
            println!("复权事件: {n} 行");
        }
        Cmd::Sync { dump_dir } => {
            let days = c.client.trading_days().await?;
            println!("交易日历: {} 个交易日", days.item.len());
            c.sync_daily_bars(&dump_dir, &days).await?;
            let n = c.import_adjustment_factors(&dump_dir).await?;
            println!("复权事件: {n} 行");
        }
        Cmd::Tickers => {
            let n = c.sync_tickers().await?;
            println!("代码表: {n} 只标的");
        }
        Cmd::Calendar => {
            let n = c.sync_trading_days().await?;
            println!("交易日历: {n} 个交易日");
        }
        Cmd::Snapshot => {
            let n = c.collect_market_snapshot().await?;
            println!("快照: {n} 只标的");
        }
        Cmd::Index { days } => {
            let n = c.sync_index_bars(days).await?;
            println!("指数日K: 共写入 {n} 根");
        }
        Cmd::Stats => {
            let s = c.db.lock().unwrap().stats()?;
            println!("代码表:     {}", s.tickers);
            println!("交易日历:   {}", s.trading_days);
            println!("日 K:       {}", s.daily_bars);
            println!("复权事件:   {}", s.adjustment_events);
            println!("快照:       {}", s.snapshots);
            println!("最新日K:    {}", s.last_bar_date.as_deref().unwrap_or("-"));
            println!("最新快照:   {}", s.last_snapshot_ts.map_or("-".into(), |t| t.to_string()));
        }
    }
    Ok(())
}
