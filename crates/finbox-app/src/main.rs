//! finbox-app：主程序。
//!
//! 子命令：
//! - `run`        常驻调度（采集 + 决策 + 执行）
//! - `decide`     手动触发一轮决策（打印意图，不下单）
//! - `account`    查询账户
//! - `positions`  查询持仓
//! - `trades`     最近成交流水
//! - `screen`     查看本轮初筛候选

mod config;
mod scheduler;
mod web;

use clap::{Parser, Subcommand};
use finbox_decision::{DecisionEngine, LlmConfig};
use finbox_store::open_shared;
use finbox_trader::{Broker, SimBroker};

use crate::config::Config;
use crate::scheduler::Scheduler;

#[derive(Parser)]
#[command(name = "finbox", about = "AI 选股的 A 股模拟交易系统")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 常驻运行：采集 + 决策 + 执行
    Run,
    /// 启动 Web 界面（axum）
    Serve {
        /// 监听地址
        #[arg(long, default_value = "0.0.0.0:8000")]
        bind: String,
    },
    /// 手动一轮决策（打印意图，不下单）
    Decide,
    /// 查询账户
    Account,
    /// 查询持仓
    Positions,
    /// 最近成交流水
    Trades,
}

fn engine_from_cfg(cfg: &Config) -> anyhow::Result<(finbox_store::SharedDb, DecisionEngine, SimBroker)> {
    let db = open_shared(&cfg.db_path)?;
    let engine = DecisionEngine::new(
        db.clone(),
        LlmConfig {
            base_url: cfg.llm_base_url.clone(),
            api_key: cfg.llm_api_key.clone(),
            model: cfg.llm_model.clone(),
        },
        cfg.watchlist.clone(),
    );
    let broker = SimBroker::new(db.clone(), cfg.initial_capital);
    Ok((db, engine, broker))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    let cfg = Config::from_env()?;

    match cli.cmd {
        Cmd::Run => {
            let mut s = Scheduler::new(cfg)?;
            s.run().await?;
        }
        Cmd::Serve { bind } => {
            let db = open_shared(&cfg.db_path)?;
            let state = web::WebState { db, cfg: cfg.clone() };
            let app = web::router(state);
            let listener = tokio::net::TcpListener::bind(&bind).await?;
            log::info!("Web 界面已启动: http://{bind}");
            axum::serve(listener, app).await?;
        }
        Cmd::Decide => {
            let (_db, engine, _broker) = engine_from_cfg(&cfg)?;
            let result = engine.decide(cfg.screen_top_n).await?;
            println!("状态: {}", result.status);
            println!("备注: {}", result.note);
            for i in &result.intents {
                println!("  {} {} {}股", i.side.as_str(), i.thscode, i.quantity);
            }
        }
        Cmd::Account => {
            let (_db, _engine, broker) = engine_from_cfg(&cfg)?;
            let acct = broker.account().await?;
            println!("现金: {:.2}  初始: {:.2}", acct.cash, acct.initial_capital);
        }
        Cmd::Positions => {
            let (_db, _engine, broker) = engine_from_cfg(&cfg)?;
            let pos = broker.positions().await?;
            if pos.is_empty() {
                println!("（空仓）");
            }
            for p in &pos {
                println!("{} {} ×{} 成本 {:.3}", p.thscode, p.name, p.quantity, p.avg_cost);
            }
        }
        Cmd::Trades => {
            let (db, _engine, _broker) = engine_from_cfg(&cfg)?;
            let db = db.lock().unwrap();
            for t in db.recent_trades(20)? {
                println!(
                    "{:?} {} {} {}股 @ {:.2} 费 {:.2}",
                    t.ts_ms, t.side.as_str(), t.thscode, t.quantity, t.price, t.fee
                );
            }
        }
    }
    Ok(())
}
