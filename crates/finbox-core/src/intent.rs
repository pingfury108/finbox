//! 委托意图与成交结果：决策层与执行层之间的解耦契约。

use thiserror::Error;

/// 买卖方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderSide {
    Buy,
    Sell,
}

impl OrderSide {
    pub fn as_str(self) -> &'static str {
        match self {
            OrderSide::Buy => "BUY",
            OrderSide::Sell => "SELL",
        }
    }
}

/// 决策层产出的委托意图：只描述「想做什么」，不触碰执行。
#[derive(Debug, Clone)]
pub struct OrderIntent {
    pub thscode: String,
    pub name: String,
    pub side: OrderSide,
    /// 目标数量（股），整手
    pub quantity: u32,
    /// 触发来源的决策 ID
    pub decision_id: Option<i64>,
}

/// 下单被拒的原因（校验失败）。
#[derive(Debug, Error, Clone, PartialEq)]
pub enum RejectReason {
    #[error("非交易时段")]
    NotTradingTime,
    #[error("数量不符合板块整手规则（主板/创业板100股整手，科创板≥200股起）")]
    LotSize(u32),
    #[error("账户资金不满足该板块权限要求: {0}")]
    BoardNotAllowed(String),
    #[error("{0} 已涨停，禁止买入")]
    LimitUp(String),
    #[error("{0} 已跌停，禁止卖出")]
    LimitDown(String),
    #[error("资金不足: 需要 {0:.2}(含费), 可用 {1:.2}")]
    InsufficientFunds(f64, f64),
    #[error("持仓不足: {0} 持有 {1} 股")]
    InsufficientPosition(String, u32),
    #[error("T+1: {0} 当日买入部分不可卖, 可卖 {1} 股")]
    TPlusOne(String, u32),
    #[error("单票仓位超限: 上限 {0:.0}%")]
    PositionLimit(f64),
    #[error("持股数量超限: 上限 {0} 只")]
    MaxPositions(usize),
    #[error("无有效行情: {0}")]
    NoPrice(String),
    #[error("买入价超出涨停价")]
    PriceAboveLimitUp,
    #[error("卖出价低于跌停价")]
    PriceBelowLimitDown,
    #[error("其他: {0}")]
    Other(String),
}

/// 成交结果。
#[derive(Debug, Clone)]
pub struct Execution {
    pub intent: OrderIntent,
    /// 成交价（真实行情价）
    pub price: f64,
    /// 成交金额
    pub amount: f64,
    /// 费用
    pub fee: f64,
    /// 成交后账户现金（模拟盘）
    pub cash_after: Option<f64>,
}
