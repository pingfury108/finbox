//! 查询耗时诊断：`cargo run -p finbox-store --example qbench -- <duckdb> <thscode>`

use finbox_store::Db;

fn time<F: FnOnce() -> T, T>(name: &str, f: F) -> T {
    let t = std::time::Instant::now();
    let r = f();
    println!("{name}: {:?}", t.elapsed());
    r
}

fn main() {
    let db_path = std::env::args().nth(1).unwrap_or_else(|| "data/finbox.duckdb".into());
    let thscode = std::env::args().nth(2).unwrap_or_else(|| "600519.SH".into());
    let db = Db::open(&db_path).unwrap();

    let snaps = time("latest_snapshots", || db.latest_snapshots().unwrap());
    println!("  {} 条", snaps.len());

    for _ in 0..3 {
        let bars = time("recent_bars(60)", || db.recent_bars(&thscode, 60).unwrap());
        println!("  {} 根", bars.len());
    }

    time("ticker_name", || db.ticker_name(&thscode).unwrap());
    time("positions", || db.positions().unwrap());
    time("account", || db.get_or_init_account(0.0).unwrap());
    time("recent_executed", || db.recent_executed_decisions(5).unwrap());
}
