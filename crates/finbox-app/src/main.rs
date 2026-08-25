//! finbox：主程序（单二进制）。
//!
//! 子命令：
//! - `run`    启动整个系统：采集 + 所有账户调度 + Web（唯一启动命令）
//! - `init`   首次全量建库（代码表 + 交易日历 + 10年日K + 复权，新环境执行一次）
//! - `stats`  库内统计（排查用）

mod accounts;
mod api;
mod config;
mod scheduler;
mod web;

use clap::{Parser, Subcommand};
use finbox_collector::Collector;
use finbox_store::open_market_shared;
use std::path::PathBuf;

use crate::config::Config;
use crate::scheduler::Scheduler;

#[derive(Parser)]
#[command(name = "finbox", about = "AI 选股的 A 股模拟交易系统（单进程多账户）")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 启动整个系统：数据采集 + 所有账户调度 + Web 界面
    Run {
        /// 管理口令（保护设置页/新建/删除账户；也可用环境变量 ADMIN_KEY）
        #[arg(long)]
        admin_key: Option<String>,
    },
    /// 首次全量建库：代码表 + 交易日历 + 全市场 10 年日 K + 复权事件
    Init {
        /// Parquet dump 缓存目录
        #[arg(long, default_value = "data/dumps")]
        dump_dir: PathBuf,
    },
    /// 库内统计
    Stats,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    let cfg = Config::from_env()?;

    match cli.cmd {
        Cmd::Run { admin_key } => {
            let mut cfg = cfg;
            if let Some(k) = admin_key {
                cfg.admin_key = k;
            }
            let s = Scheduler::new(cfg)?;
            s.run().await?;
        }
        Cmd::Init { dump_dir } => {
            let market = open_market_shared(PathBuf::from(&cfg.data_dir).join("market.duckdb"))?;
            let c = Collector::new(hithink_sdk::Client::from_env()?, market);
            let n = c.sync_tickers().await?;
            println!("代码表: {n} 只标的");
            let days = c.client.trading_days().await?;
            c.upsert_trading_days(&days).await?;
            println!("交易日历: {} 个交易日", days.item.len());
            c.sync_daily_bars(&dump_dir, &days).await?;
            let n = c.import_adjustment_factors(&dump_dir).await?;
            println!("复权事件: {n} 行");
            println!("建库完成");
        }
        Cmd::Stats => {
            let market = open_market_shared(PathBuf::from(&cfg.data_dir).join("market.duckdb"))?;
            let s = market.lock().unwrap().stats()?;
            println!("代码表:     {}", s.tickers);
            println!("交易日历:   {}", s.trading_days);
            println!("日 K:       {}", s.daily_bars);
            println!("复权事件:   {}", s.adjustment_events);
            println!("快照:       {}", s.snapshots);
            println!("最新日K:    {}", s.last_bar_date.as_deref().unwrap_or("-"));
        }
    }
    Ok(())
}
