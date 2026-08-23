//! SimBroker 端到端演示：`cargo run -p finbox-trader --example trade_demo -- <duckdb>`
//!
//! 读取真实行情库（日K+快照），演示下单（交易时段外会被拒）。

use finbox_core::{OrderIntent, OrderSide};
use finbox_store::open_shared;
use finbox_trader::{Broker, SimBroker};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db_path = std::env::args().nth(1).unwrap_or_else(|| "data/finbox.duckdb".into());
    let db = open_shared(&db_path)?;
    let broker = SimBroker::new(db, 200_000.0);

    let acct = broker.account().await?;
    println!("账户现金: {:.2}（初始 {:.2}）", acct.cash, acct.initial_capital);

    let pos = broker.positions().await?;
    println!("持仓: {} 只", pos.len());
    for p in &pos {
        println!("  {} {} ×{} 成本 {:.3}", p.thscode, p.name, p.quantity, p.avg_cost);
    }

    // 演示买单（大概率因非交易时段被拒）
    let intent = OrderIntent {
        thscode: "600519.SH".into(),
        name: "贵州茅台".into(),
        side: OrderSide::Buy,
        quantity: 100,
        decision_id: None,
    };
    match broker.submit(intent).await {
        Ok(e) => println!("成交: {} {}股 @ {:.2} 费 {:.2} 余现金 {:.2}", e.intent.thscode, e.intent.quantity, e.price, e.fee, e.cash_after.unwrap_or(0.0)),
        Err(e) => println!("拒单: {e}"),
    }
    Ok(())
}
