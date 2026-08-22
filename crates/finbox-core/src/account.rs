//! 账户、持仓、成交流水模型（DuckDB 持久化）。

use super::OrderSide;

/// 模拟账户（单行）。
#[derive(Debug, Clone)]
pub struct Account {
    pub cash: f64,
    pub initial_capital: f64,
}

/// 持仓。
#[derive(Debug, Clone)]
pub struct Position {
    pub thscode: String,
    pub name: String,
    pub quantity: u32,
    pub avg_cost: f64,
}

/// 成交流水。`price` 为真实行情价。
#[derive(Debug, Clone)]
pub struct Trade {
    pub thscode: String,
    pub name: String,
    pub side: OrderSide,
    pub price: f64,
    pub quantity: u32,
    pub amount: f64,
    pub fee: f64,
    /// 触发本次交易的决策 ID（可空，手动/非 AI 触发的为 None）
    pub decision_id: Option<i64>,
}
