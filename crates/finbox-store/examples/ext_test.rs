//! parquet 扩展可用性测试：`cargo run -p finbox-store --example ext_test -- <load|install>`
//!
//! - `load`    只测 LOAD parquet（bundled 是否内置）
//! - `install` 尝试 INSTALL parquet（可能卡网络，用外部 timeout 保护）

use duckdb::Connection;

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_else(|| "load".into());
    let mut c = Connection::open_in_memory().expect("open");
    match arg.as_str() {
        "load" => match c.execute_batch("LOAD parquet;") {
            Ok(_) => println!("LOAD parquet OK (bundled)"),
            Err(e) => println!("LOAD failed: {e}"),
        },
        "install" => match c.execute_batch("INSTALL parquet;") {
            Ok(_) => println!("INSTALL parquet OK"),
            Err(e) => println!("INSTALL failed: {e}"),
        },
        _ => println!("usage: ext_test <load|install>"),
    }
}
