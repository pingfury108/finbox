//! A 股行情：快照、历史 K 线、复权因子事件流。

use crate::{Adjust, Client, Result};

/// 行情快照记录。注意：快照**不含**标的中文名，需配合元信息端点解析。
/// 停牌/新上市等情况下开高低收、涨跌、量额可能为 `null`（未披露），用 `Option` 保留。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PriceSnapshotItem {
    pub thscode: String,
    pub ticker: String,
    /// 最新成交价
    pub last_price: Option<f64>,
    /// 相对前收盘价的涨跌额
    pub price_change: Option<f64>,
    /// 涨跌幅（百分比数值，如 `1.74` 表示 +1.74%）
    pub price_change_ratio_pct: Option<f64>,
    /// 当日开盘价
    pub open_price: Option<f64>,
    /// 当日最高价
    pub high_price: Option<f64>,
    /// 当日最低价
    pub low_price: Option<f64>,
    /// 前收盘价
    pub prev_price: Option<f64>,
    /// 成交量（股）
    pub volume: Option<f64>,
    /// 成交额
    pub turnover: Option<f64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SnapshotData {
    /// 数据就绪时间（毫秒）；按 thscodes 显式取数时为 `null`
    #[serde(default)]
    pub timestamp: Option<i64>,
    /// 全市场代码表总数（分页模式用于估算页数）
    #[serde(default)]
    pub total: Option<u64>,
    pub item: Vec<PriceSnapshotItem>,
}

/// 日 K 线。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PriceBarItem {
    /// K 线日期（毫秒，Asia/Shanghai）
    pub date_ms: i64,
    pub open_price: f64,
    pub high_price: f64,
    pub low_price: f64,
    pub close_price: f64,
    /// 成交量（股）
    pub volume: f64,
    /// 成交额
    pub turnover: f64,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct HistoricalData {
    /// 数据就绪时间（毫秒）
    pub timestamp: i64,
    pub item: Vec<PriceBarItem>,
}

/// 复权事件（现金分红 / 送股）。事件类型由数值字段隐式区分。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AdjustmentFactorItem {
    pub ticker: String,
    /// 除权除息日（毫秒，Asia/Shanghai 00:00:00）
    pub ex_date_ms: i64,
    /// 每股现金分红（税前）；非现金事件为 0
    pub dividend_per_share: f64,
    /// 每股送股比例（如 `0.1` 表示 10 送 1）；纯现金分红事件为 0
    pub per_share_bonus: f64,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AdjustmentFactorsData {
    pub thscode: String,
    pub ticker: String,
    /// 按 `ex_date_ms` 降序（最新在前）
    pub item: Vec<AdjustmentFactorItem>,
}

impl Client {
    /// 行情快照。`thscodes` 为 `Some` 时批量模式（忽略分页），为 `None` 时全市场分页模式。
    pub async fn price_snapshot(
        &self,
        thscodes: Option<&[&str]>,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<SnapshotData> {
        let mut query = Vec::new();
        match thscodes {
            Some(codes) if !codes.is_empty() => {
                query.push(("thscodes", codes.join(",")));
            }
            _ => {
                if let Some(v) = limit {
                    query.push(("limit", v.to_string()));
                }
                if let Some(v) = offset {
                    query.push(("offset", v.to_string()));
                }
            }
        }
        self.get("/api/a-share/prices/snapshot", &query).await
    }

    /// 单只 A 股历史日 K 线。时间窗口 ≤ 10 年；`start`/`end` 为毫秒时间戳。
    pub async fn price_historical(
        &self,
        thscode: &str,
        start_ms: i64,
        end_ms: i64,
        adjust: Adjust,
        offset: Option<u32>,
    ) -> Result<HistoricalData> {
        let mut query = vec![
            ("thscode", thscode.to_string()),
            ("interval", "1d".to_string()),
            ("start", start_ms.to_string()),
            ("end", end_ms.to_string()),
            ("adjust", adjust.as_str().to_string()),
        ];
        if let Some(v) = offset {
            query.push(("offset", v.to_string()));
        }
        self.get("/api/a-share/prices/historical", &query).await
    }

    /// 单只标的复权因子事件流（原始事件，非每日因子）。`from`/`to` 格式 `YYYY-MM-DD`。
    pub async fn adjustment_factors(
        &self,
        thscode: &str,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<AdjustmentFactorsData> {
        let mut query = vec![("thscode", thscode.to_string())];
        if let Some(v) = from {
            query.push(("from", v.to_string()));
        }
        if let Some(v) = to {
            query.push(("to", v.to_string()));
        }
        self.get("/api/a-share/corporate-actions/adjustment-factors", &query).await
    }

    /// 单只指数/板块历史日 K（无复权概念）。`start`/`end` 为毫秒时间戳，窗口 ≤ 10 年。
    pub async fn index_price_historical(
        &self,
        thscode: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<HistoricalData> {
        let query = vec![
            ("thscode", thscode.to_string()),
            ("interval", "1d".to_string()),
            ("start", start_ms.to_string()),
            ("end", end_ms.to_string()),
        ];
        self.get("/api/a-share-index/prices/historical", &query).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_item_parses() {
        let json = r#"{
            "thscode": "600519.SH", "ticker": "600519",
            "last_price": 1700.5, "price_change": 29.1, "price_change_ratio_pct": 1.74,
            "open_price": 1680.0, "high_price": 1710.0, "low_price": 1675.0,
            "prev_price": 1671.4, "volume": 12345678.0, "turnover": 987654321.0
        }"#;
        let item: PriceSnapshotItem = serde_json::from_str(json).unwrap();
        assert_eq!(item.last_price, Some(1700.5));
        assert_eq!(item.thscode, "600519.SH");
    }

    #[test]
    fn snapshot_item_nullable_fields() {
        let json = r#"{
            "thscode": "688001.SH", "ticker": "688001",
            "last_price": null, "price_change": null, "price_change_ratio_pct": null,
            "open_price": null, "high_price": null, "low_price": null,
            "prev_price": null, "volume": null, "turnover": null
        }"#;
        let item: PriceSnapshotItem = serde_json::from_str(json).unwrap();
        assert!(item.last_price.is_none());
        assert!(item.prev_price.is_none());
    }

    #[test]
    fn snapshot_data_timestamp_nullable() {
        let json = r#"{"timestamp": null, "total": null, "item": []}"#;
        let data: SnapshotData = serde_json::from_str(json).unwrap();
        assert!(data.timestamp.is_none());
        assert!(data.item.is_empty());
    }

    #[test]
    fn bar_parses() {
        let json = r#"{
            "date_ms": 1735660800000, "open_price": 1.0, "high_price": 2.0,
            "low_price": 0.5, "close_price": 1.5, "volume": 100.0, "turnover": 150.0
        }"#;
        let bar: PriceBarItem = serde_json::from_str(json).unwrap();
        assert_eq!(bar.date_ms, 1735660800000);
    }
}
