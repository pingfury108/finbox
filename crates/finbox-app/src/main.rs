//! finbox-app：主程序。
//!
//! 子命令：
//! - `run`                  单进程运行（采集 + 所有账户调度）
//! - `serve`                启动 Web
//! - `account create/list/rm <name>`  账户管理
//! - `decide --account <name>` 手动触发某账户一轮决策
//! - `account <name> info`  账户概览

mod accounts;
mod api;
mod config;
mod scheduler;
mod web;

use clap::{Parser, Subcommand};
use finbox_store::open_market_shared;

use crate::accounts::AccountInfo;
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
    /// 单进程运行：采集 + 所有账户调度
    Run,
    /// 启动 Web 界面
    Serve {
        #[arg(long, default_value = "0.0.0.0:8000")]
        bind: String,
    },
    /// 账户管理
    #[command(subcommand)]
    Account(AcctCmd),
    /// 手动触发某账户一轮决策
    Decide {
        #[arg(long)]
        account: String,
    },
}

#[derive(Subcommand)]
enum AcctCmd {
    /// 创建账户
    Create {
        name: String,
        #[arg(long, default_value_t = 200000.0)]
        capital: f64,
        #[arg(long, default_value = "")]
        watchlist: String,
    },
    /// 列出账户
    List,
    /// 删除账户
    Rm { name: String },
    /// 账户信息
    Info { name: String },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    let cfg = Config::from_env()?;

    match cli.cmd {
        Cmd::Run => {
            let s = Scheduler::new(cfg)?;
            s.run().await?;
        }
        Cmd::Serve { bind } => {
            let state = web::WebState::new(&cfg)?;
            let app = web::router(state);
            let listener = tokio::net::TcpListener::bind(&bind).await?;
            log::info!("Web 界面已启动: http://{bind}");
            axum::serve(listener, app).await?;
        }
        Cmd::Account(acct) => match acct {
            AcctCmd::Create { name, capital, watchlist } => {
                let info = accounts::create_account(&cfg.data_dir, &name, capital)?;
                if !watchlist.is_empty() {
                    let db = accounts::open_account(&cfg.data_dir, &info.name)?;
                    db.lock().unwrap().meta_set("watchlist", &watchlist)?;
                }
                println!("已创建账户「{}」 初始资金 {capital:.0} 库: {:?}", info.name, info.db_path);
            }
            AcctCmd::List => {
                let list = accounts::list_accounts(&cfg.data_dir)?;
                if list.is_empty() {
                    println!("（无账户）");
                }
                for a in &list {
                    println!("{}", a.name);
                }
            }
            AcctCmd::Rm { name } => {
                accounts::remove_account(&cfg.data_dir, &name)?;
                println!("已删除账户「{name}」");
            }
            AcctCmd::Info { name } => {
                let acct = accounts::open_account(&cfg.data_dir, &name)?;
                let db = acct.lock().unwrap();
                let a = db.get_or_init_account(cfg.initial_capital)?;
                let positions = db.positions()?;
                println!("账户「{name}」");
                println!("  现金 {:.2}  初始 {:.2}", a.cash, a.initial_capital);
                println!("  持仓 {} 只", positions.len());
                for p in &positions {
                    println!("    {} {} ×{} 成本 {:.3}", p.thscode, p.name, p.quantity, p.avg_cost);
                }
            }
        },
        Cmd::Decide { account } => {
            let market = open_market_shared(std::path::Path::new(&cfg.data_dir).join("market.duckdb"))?;
            let acct = accounts::open_account(&cfg.data_dir, &account)?;
            let ctx = scheduler::build_decision_engine(&cfg, market, acct);
            let result = ctx.decide(5).await?;
            println!("状态: {}  备注: {}", result.status, result.note);
            for i in &result.intents {
                println!("  {} {} {}股", i.side.as_str(), i.thscode, i.quantity);
            }
        }
    }
    Ok(())
}

// 供 Decide 命令复用账户信息
#[allow(dead_code)]
fn _account_info(_list: &[AccountInfo]) {}
