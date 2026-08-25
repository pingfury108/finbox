//! A 股特色数据：涨跌停池、连板天梯、热榜、飙升榜、龙虎榜、个股异动。
//!
//! 响应字段多且可能扩展，统一返回信封内 `data` 的 `serde_json::Value`。

use crate::{Client, Result};

/// 涨跌停/炸板池通用分页参数。全部可选，缺省用服务端默认（当日 / 第1页 / 50条 / 最新价倒序）。
#[derive(Debug, Clone, Default)]
pub struct PoolQuery {
    /// 交易日毫秒戳（Asia/Shanghai 00:00:00）；None = 服务端当前自然日
    pub date_ms: Option<i64>,
    /// 页码，>= 1
    pub page: Option<u32>,
    /// 每页 1-200
    pub size: Option<u32>,
    /// 排序字段：`last_price` / `continue_day_cnt` / `seal_money` / `limit_up_time`
    pub sort_field: Option<&'static str>,
    /// 排序方向：`asc` / `desc`
    pub sort_dir: Option<&'static str>,
}

impl PoolQuery {
    fn pairs(&self) -> Vec<(&'static str, String)> {
        let mut q: Vec<(&'static str, String)> = Vec::new();
        if let Some(v) = self.date_ms { q.push(("date_ms", v.to_string())); }
        if let Some(v) = self.page { q.push(("page", v.to_string())); }
        if let Some(v) = self.size { q.push(("size", v.to_string())); }
        if let Some(v) = self.sort_field { q.push(("sort_field", v.to_string())); }
        if let Some(v) = self.sort_dir { q.push(("sort_dir", v.to_string())); }
        q
    }
}

impl Client {
    /// 涨停池：按交易日返回涨停/连板股票。
    pub async fn limit_up_pool(&self, q: &PoolQuery) -> Result<serde_json::Value> {
        self.get("/api/a-share/special-data/limit-up-pool", &q.pairs()).await
    }

    /// 跌停池。
    pub async fn limit_down_pool(&self, q: &PoolQuery) -> Result<serde_json::Value> {
        self.get("/api/a-share/special-data/limit-down-pool", &q.pairs()).await
    }

    /// 炸板池（曾涨停后打开）。
    pub async fn limit_break_pool(&self, q: &PoolQuery) -> Result<serde_json::Value> {
        self.get("/api/a-share/special-data/limit-break-pool", &q.pairs()).await
    }

    /// 涨停连板天梯：近 30 交易日连板梯队矩阵。
    pub async fn limit_up_ladder(&self) -> Result<serde_json::Value> {
        self.get("/api/a-share/special-data/limit-up-ladder", &[]).await
    }

    /// 同花顺热股榜。`period`: `day`（24小时）/ `hour`（小时级）。
    pub async fn hot_stock_list(&self, period: Option<&str>) -> Result<serde_json::Value> {
        let q: Vec<(&str, String)> = period.map(|p| vec![("period", p.to_string())]).unwrap_or_default();
        self.get("/api/a-share/special-data/hot-stock-list", &q).await
    }

    /// 历史热股榜。`date`: `yyyy-MM-dd`。
    pub async fn hot_stock_list_history(&self, date: &str) -> Result<serde_json::Value> {
        self.get("/api/a-share/special-data/hot-stock-list-history", &[("date", date.to_string())]).await
    }

    /// 单只股票热榜排名走势。`start_date`/`end_date`: `yyyy-MM-dd`。
    pub async fn hot_stock_rank_trend(&self, thscode: &str, start_date: &str, end_date: &str) -> Result<serde_json::Value> {
        self.get(
            "/api/a-share/special-data/hot-stock-rank-trend",
            &[
                ("thscode", thscode.to_string()),
                ("start_date", start_date.to_string()),
                ("end_date", end_date.to_string()),
            ],
        )
        .await
    }

    /// 飙升榜。`period`: `day` / `hour`。
    pub async fn skyrocket_list(&self, period: Option<&str>) -> Result<serde_json::Value> {
        let q: Vec<(&str, String)> = period.map(|p| vec![("period", p.to_string())]).unwrap_or_default();
        self.get("/api/a-share/special-data/skyrocket-list", &q).await
    }

    /// 龙虎榜。`board_type`: `all` / `org`（机构）/ `hot_money`（游资）；`date`: `yyyy-MM-dd`（一年内）。
    pub async fn dragon_tiger_list(&self, board_type: Option<&str>, date: Option<&str>) -> Result<serde_json::Value> {
        let mut q: Vec<(&str, String)> = Vec::new();
        if let Some(v) = board_type { q.push(("board_type", v.to_string())); }
        if let Some(v) = date { q.push(("date", v.to_string())); }
        self.get("/api/a-share/special-data/dragon-tiger-list", &q).await
    }

    /// 当日个股异动列表（全市场）。`tag_codes` 可选，逗号分隔异动类型过滤。
    pub async fn anomaly_analysis_list(&self, tag_codes: Option<&str>) -> Result<serde_json::Value> {
        let q: Vec<(&str, String)> = tag_codes.map(|t| vec![("tag_codes", t.to_string())]).unwrap_or_default();
        self.get("/api/a-share/special-data/anomaly-analysis-list", &q).await
    }

    /// 按股票查当日异动原因。`thscodes` 逗号分隔。
    pub async fn anomaly_analysis_stock(&self, thscodes: &[&str]) -> Result<serde_json::Value> {
        self.get(
            "/api/a-share/special-data/anomaly-analysis-stock",
            &[("thscodes", thscodes.join(","))],
        )
        .await
    }
}
