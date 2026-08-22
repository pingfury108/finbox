//! A 股估值快照（仅最新值，无历史序列）。
//!
//! 五项估值指标允许为 `null` 或负数：`null` 表示上游无有效值，负数可能反映亏损或负现金流，
//! 不得补零或取绝对值。

use crate::{Client, Result};

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ValuationItem {
    pub thscode: String,
    pub ticker: String,
    #[serde(default)]
    pub name: Option<String>,
    /// 市盈率 TTM
    pub pe_ttm: Option<f64>,
    /// 市盈率 MRQ
    pub pe_mrq: Option<f64>,
    /// 市净率 MRQ
    pub pb_mrq: Option<f64>,
    /// 市销率 TTM
    pub ps_ttm: Option<f64>,
    /// 市现率 TTM
    pub pcf_ttm: Option<f64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ValuationData {
    /// 返回行中最新有效上游时间（毫秒）；无有效时间时为 `null`
    #[serde(default)]
    pub timestamp: Option<i64>,
    pub total: u32,
    /// 按规范化、去重后的请求顺序返回
    pub item: Vec<ValuationItem>,
}

/// 估值快照原始 token 上限。
pub const VALUATION_MAX_TOKENS: usize = 100;

impl Client {
    /// 批量查询 A 股最新估值快照。原始 token 最多 100 个（按未去重数量计）。
    pub async fn valuation_snapshot(&self, thscodes: &[&str]) -> Result<ValuationData> {
        assert!(
            thscodes.len() <= VALUATION_MAX_TOKENS,
            "估值快照原始 token 上限 {VALUATION_MAX_TOKENS}，当前 {}",
            thscodes.len()
        );
        let query = vec![("thscodes", thscodes.join(","))];
        self.get("/api/a-share/valuations/snapshot", &query).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valuation_item_parses_with_nulls() {
        let json = r#"{
            "thscode": "600519.SH", "ticker": "600519", "name": "贵州茅台",
            "pe_ttm": null, "pe_mrq": 25.5, "pb_mrq": -1.2,
            "ps_ttm": null, "pcf_ttm": 30.1
        }"#;
        let item: ValuationItem = serde_json::from_str(json).unwrap();
        assert!(item.pe_ttm.is_none());
        assert_eq!(item.pb_mrq, Some(-1.2));
    }
}
