//! finbox-core：交易领域模型与 A 股规则常量。
//!
//! 零外部依赖的纯领域层：模拟盘与实盘券商共享同一套模型。

pub mod account;
pub mod intent;
pub mod rules;

pub use account::{Account, Position, Trade};
pub use intent::{Execution, OrderIntent, OrderSide, RejectReason};
