//! DuckDB 导入复现：`cargo run -p finbox-store --example import_repro -- <parquet>`

use finbox_store::Db;

fn main() -> finbox_store::Result<()> {
    let path = std::env::args().nth(1).expect("usage: import_repro <parquet>");
    let db = Db::open("data/finbox.duckdb")?;
    let t = std::time::Instant::now();
    let n = db.import_daily_k_parquet(std::path::Path::new(&path))?;
    println!("imported {n} rows in {:?}", t.elapsed());
    Ok(())
}
