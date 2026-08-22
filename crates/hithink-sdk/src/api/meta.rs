//! 元信息：标的检索与代码表。
//!
//! <https://fuyao.aicubes.cn/docs/api-reference/>

use crate::{Client, Result};

/// 标的信息（检索与代码表共用结构）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TickerItem {
    /// 完整 thscode，如 `600519.SH`
    pub thscode: String,
    /// 纯代码，如 `600519`
    pub ticker: String,
    /// 展示名称
    pub name: String,
    /// 交易所后缀（`SH`/`SZ`/`BJ`），无后缀指数为 `null`
    #[serde(default)]
    pub exchange: Option<String>,
    /// 资产类别：`a-share` / `a-share-index` / `forex` / `fund-*` 等
    pub asset_type: String,
    /// 币种代码
    pub currency: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct TickerData {
    /// 数据就绪时间（毫秒）
    pub timestamp: i64,
    pub item: Vec<TickerItem>,
}

impl Client {
    /// 标的检索：按 thscode / 纯代码 / 中英文名跨市场消歧。
    ///
    /// `q` 支持子串；`asset_type` 支持逗号分隔多值（如 `fund-etf,fund-lof`）。
    pub async fn search_tickers(
        &self,
        q: &str,
        exchange: Option<&str>,
        asset_type: Option<&str>,
        limit: Option<u32>,
    ) -> Result<TickerData> {
        let mut query = vec![("q", q.to_string())];
        if let Some(v) = exchange {
            query.push(("exchange", v.to_string()));
        }
        if let Some(v) = asset_type {
            query.push(("asset_type", v.to_string()));
        }
        if let Some(v) = limit {
            query.push(("limit", v.to_string()));
        }
        self.get("/api/meta/tickers/search", &query).await
    }

    /// 标的列表：按交易所/资产类别批量获取代码表（分页）。
    pub async fn list_tickers(
        &self,
        exchange: Option<&str>,
        asset_type: Option<&str>,
        limit: u32,
        offset: u32,
    ) -> Result<TickerData> {
        let mut query = vec![("limit", limit.to_string()), ("offset", offset.to_string())];
        if let Some(v) = exchange {
            query.push(("exchange", v.to_string()));
        }
        if let Some(v) = asset_type {
            query.push(("asset_type", v.to_string()));
        }
        self.get("/api/meta/tickers/list", &query).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticker_item_parses() {
        let json = r#"{
            "thscode": "600519.SH", "ticker": "600519", "name": "贵州茅台",
            "exchange": "SH", "asset_type": "a-share", "currency": "CNY"
        }"#;
        let item: TickerItem = serde_json::from_str(json).unwrap();
        assert_eq!(item.thscode, "600519.SH");
        assert_eq!(item.asset_type, "a-share");
    }

    #[test]
    fn ticker_item_exchange_nullable() {
        let json = r#"{
            "thscode": "885001.TI", "ticker": "885001", "name": "某指数",
            "exchange": null, "asset_type": "a-share-index", "currency": "CNY"
        }"#;
        let item: TickerItem = serde_json::from_str(json).unwrap();
        assert!(item.exchange.is_none());
    }
}
