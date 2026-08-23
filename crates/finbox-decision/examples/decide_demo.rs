//! 端到端决策演示：`cargo run -p finbox-decision --example decide_demo -- <duckdb>`
//!
//! 从真实行情库初筛 → 构建上下文 → 调 LLM（DeepSeek）→ 打印委托意图。
//! 需要环境变量 LLM_BASE_URL / LLM_API_KEY / LLM_MODEL。

use finbox_decision::{DecisionEngine, LlmConfig};
use finbox_store::open_shared;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db_path = std::env::args().nth(1).unwrap_or_else(|| "data/finbox.duckdb".into());
    let db = open_shared(&db_path)?;

    let config = LlmConfig {
        base_url: std::env::var("LLM_BASE_URL").unwrap_or_else(|_| "https://api.deepseek.com".into()),
        api_key: std::env::var("LLM_API_KEY").unwrap_or_default(),
        model: std::env::var("LLM_MODEL").unwrap_or_else(|_| "deepseek-chat".into()),
    };
    let watchlist: Vec<String> = std::env::var("WATCHLIST")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let engine = DecisionEngine::new(db, config, watchlist);
    let result = engine.decide(20).await?;

    println!("决策状态: {}", result.status);
    println!("备注: {}", result.note);
    println!("委托意图: {} 条", result.intents.len());
    for i in &result.intents {
        println!(
            "  {} {} {}股 (decision_id={:?})",
            i.side.as_str(),
            i.thscode,
            i.quantity,
            i.decision_id
        );
    }
    println!("原始输出:\n{}", result.raw_response);
    Ok(())
}
