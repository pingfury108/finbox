//! 同花顺金融数据服务 REST API SDK。
//!
//! 上游文档: <https://fuyao.aicubes.cn/docs/api-reference/overview/>
//!
//! 协议要点:
//! - 全部端点为 `GET`，认证头 `X-api-key`
//! - 响应信封 `{code, message, request_id, data}`，HTTP 200 不代表业务成功，必须判 `code == 0`
//! - 时间戳为毫秒 Unix 时间戳；`null` 表示未披露，不得补零
//!
//! ```no_run
//! use hithink_sdk::Client;
//!
//! #[tokio::main]
//! async fn main() -> hithink_sdk::Result<()> {
//!     let client = Client::from_env()?;
//!     let days = client.trading_days().await?;
//!     println!("近一年交易日数量: {}", days.item.len());
//!     Ok(())
//! }
//! ```

mod client;
mod error;

pub mod api;

pub use client::Client;
pub use error::{Error, Result};

pub use api::calendar::{TradingDay, TradingDaysData};
pub use api::fund::FundType;
pub use api::market_dumps::{DownloadUrl, DumpKind};
pub use api::meta::{TickerData, TickerItem};
pub use api::prices::{AdjustmentFactorItem, AdjustmentFactorsData, HistoricalData, PriceBarItem, PriceSnapshotItem, SnapshotData};
pub use api::special::PoolQuery;
pub use api::valuations::{ValuationData, ValuationItem};

/// 复权方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adjust {
    /// 不复权，保留原始成交价格
    None,
    /// 前复权（默认口径）
    Forward,
    /// 后复权
    Backward,
}

impl Adjust {
    pub fn as_str(self) -> &'static str {
        match self {
            Adjust::None => "none",
            Adjust::Forward => "forward",
            Adjust::Backward => "backward",
        }
    }
}

impl Default for Adjust {
    fn default() -> Self {
        Adjust::Forward
    }
}
