//! finbox-trader：交易执行层。
//!
//! [`Broker`] trait 按「第三方真实券商」的形态设计：模拟盘（[`SimBroker`]）
//! 与未来实盘券商实现同一接口，决策层只面向 `Broker`，切换实现零改动。

pub mod risk;
pub mod sim;

pub use risk::{RiskConfig, RiskManager, RiskReport};
pub use sim::SimBroker;

/// 单票仓位上限（占总资产比例）。
const MAX_POSITION_PCT: f64 = 0.20;
/// 持仓数量上限。
const MAX_POSITIONS: usize = 3;

use finbox_core::{Account, Execution, OrderIntent, Position, RejectReason};
use finbox_store::StoreError;

/// 券商执行错误。
#[derive(Debug, thiserror::Error)]
pub enum BrokerError {
    #[error("订单被拒: {0}")]
    Rejected(#[from] RejectReason),
    #[error("存储错误: {0}")]
    Store(#[from] StoreError),
}

/// 券商执行抽象（第三方形态）。
#[async_trait::async_trait]
pub trait Broker: Send + Sync {
    /// 提交委托。模拟盘直接撮合返回成交；实盘券商返回受理/成交结果。
    async fn submit(&self, intent: OrderIntent) -> Result<Execution, BrokerError>;
    /// 查询账户。
    async fn account(&self) -> Result<Account, BrokerError>;
    /// 查询持仓。
    async fn positions(&self) -> Result<Vec<Position>, BrokerError>;
}
