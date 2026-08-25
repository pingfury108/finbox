//! 真实 API 联调测试。默认跳过，手动执行：
//!
//! ```bash
//! HITHINK_FINANCE_API_KEY=<key> cargo test --test live -- --ignored --nocapture
//! ```

use hithink_sdk::{Adjust, Client, DumpKind};

async fn client() -> Client {
    Client::from_env().expect("需要环境变量 HITHINK_FINANCE_API_KEY")
}

#[tokio::test]
#[ignore = "需要真实 API Key"]
async fn search_ticker() {
    let c = client().await;
    let data = c.search_tickers("600519", None, Some("a-share"), Some(3)).await.unwrap();
    assert_eq!(data.item[0].thscode, "600519.SH");
    assert_eq!(data.item[0].name, "贵州茅台");
}

#[tokio::test]
#[ignore = "需要真实 API Key"]
async fn price_snapshot_batch() {
    let c = client().await;
    let data = c.price_snapshot(Some(&["600519.SH", "000001.SZ"]), None, None).await.unwrap();
    assert_eq!(data.item.len(), 2);
    assert!(data.item[0].last_price.unwrap_or(0.0) > 0.0);
}

#[tokio::test]
#[ignore = "需要真实 API Key"]
async fn price_historical_one_year() {
    let c = client().await;
    // 2024-01-01 ~ 2025-01-01（毫秒）
    let data = c
        .price_historical("600519.SH", 1704038400000, 1735660800000, Adjust::Forward, None)
        .await
        .unwrap();
    assert!(data.item.len() > 200, "一年应约有 240 根日 K，实际 {}", data.item.len());
}

#[tokio::test]
#[ignore = "需要真实 API Key"]
async fn trading_days() {
    let c = client().await;
    let data = c.trading_days().await.unwrap();
    assert!(data.item.len() > 200);
}

#[tokio::test]
#[ignore = "需要真实 API Key"]
async fn valuation_snapshot() {
    let c = client().await;
    let data = c.valuation_snapshot(&["600519.SH"]).await.unwrap();
    assert_eq!(data.total, 1);
    assert!(data.item[0].pe_ttm.is_some() || data.item[0].pb_mrq.is_some());
}

#[tokio::test]
#[ignore = "需要真实 API Key"]
async fn dump_url_signed() {
    let c = client().await;
    let url = c.dump_download_url(DumpKind::DailyK10d).await.unwrap();
    assert!(url.presigned_url.contains("sig") || url.presigned_url.starts_with("http"));
}
