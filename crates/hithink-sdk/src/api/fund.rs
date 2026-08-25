//! 公募基金数据：资料、持仓、业绩、经理、财务、诊断、募集、资讯与行情。
//!
//! 多数接口要求 `fund_type`：`otc`（场外）/ `exchange`（ETF、LOF）/ `reits`（公募 REITs）。
//! 返回统一为信封内 `data` 的 `serde_json::Value`。

use crate::{Client, Result};

/// 基金类型：`otc`（场外基金）/ `exchange`（ETF、LOF）/ `reits`（公募 REITs）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FundType {
    Otc,
    Exchange,
    Reits,
}

impl FundType {
    pub fn as_str(self) -> &'static str {
        match self {
            FundType::Otc => "otc",
            FundType::Exchange => "exchange",
            FundType::Reits => "reits",
        }
    }
}

impl Client {
    /// 基金基本资料（名称/规模/净值/管理人/经理等）。
    pub async fn fund_profile(&self, ft: FundType, thscode: &str) -> Result<serde_json::Value> {
        self.fund_pair("/api/fund/profile/detail", ft, thscode).await
    }

    /// 基金重仓持仓（股票/债券/基金及汇总指标）。
    pub async fn fund_holdings(&self, ft: FundType, thscode: &str) -> Result<serde_json::Value> {
        self.fund_pair("/api/fund/portfolio/holdings", ft, thscode).await
    }

    /// 基金历史股票持仓。`report_type` 与 `end_date` 必填（见文档枚举）。
    pub async fn fund_portfolio_stock_history(&self, ft: FundType, thscode: &str, report_type: &str, end_date: &str) -> Result<serde_json::Value> {
        let mut q = self.fund_pairs(ft, thscode);
        q.push(("report_type", report_type.to_string()));
        q.push(("end_date", end_date.to_string()));
        self.get("/api/fund/portfolio/stock-history", &q).await
    }

    /// 基金股票持仓报告日期列表。
    pub async fn fund_portfolio_stock_report_dates(&self, ft: FundType, thscode: &str, report_type: Option<&str>) -> Result<serde_json::Value> {
        let mut q = self.fund_pairs(ft, thscode);
        if let Some(v) = report_type { q.push(("report_type", v.to_string())); }
        self.get("/api/fund/portfolio/stock-report-dates", &q).await
    }

    /// 基金历史债券持仓。
    pub async fn fund_portfolio_bond_history(&self, ft: FundType, thscode: &str, report_type: &str, end_date: &str) -> Result<serde_json::Value> {
        let mut q = self.fund_pairs(ft, thscode);
        q.push(("report_type", report_type.to_string()));
        q.push(("end_date", end_date.to_string()));
        self.get("/api/fund/portfolio/bond-history", &q).await
    }

    /// 基金债券持仓报告日期列表。
    pub async fn fund_portfolio_bond_report_dates(&self, ft: FundType, thscode: &str, report_type: Option<&str>) -> Result<serde_json::Value> {
        let mut q = self.fund_pairs(ft, thscode);
        if let Some(v) = report_type { q.push(("report_type", v.to_string())); }
        self.get("/api/fund/portfolio/bond-report-dates", &q).await
    }

    /// 基金资产配置（股/债/现金等占比）。
    pub async fn fund_portfolio_asset_allocation(&self, ft: FundType, thscode: &str) -> Result<serde_json::Value> {
        self.fund_pair("/api/fund/portfolio/asset-allocation", ft, thscode).await
    }

    /// 基金行业配置。
    pub async fn fund_portfolio_industry_allocation(&self, ft: FundType, thscode: &str) -> Result<serde_json::Value> {
        self.fund_pair("/api/fund/portfolio/industry-allocation", ft, thscode).await
    }

    /// 基金净值序列。`range` 区间（如 `1y`）；`nav_type` 净值类型（可选）。
    pub async fn fund_performance_nav(&self, ft: FundType, thscode: &str, range: Option<&str>, nav_type: Option<&str>) -> Result<serde_json::Value> {
        let mut q = self.fund_pairs(ft, thscode);
        if let Some(v) = range { q.push(("range", v.to_string())); }
        if let Some(v) = nav_type { q.push(("nav_type", v.to_string())); }
        self.get("/api/fund/performance/nav", &q).await
    }

    /// 基金区间收益。
    pub async fn fund_performance_returns(&self, ft: FundType, thscode: &str) -> Result<serde_json::Value> {
        self.fund_pair("/api/fund/performance/returns", ft, thscode).await
    }

    /// 基金历史业绩指标。`start`/`end` 为毫秒戳。
    pub async fn fund_performance_indicators_historical(&self, ft: FundType, thscode: &str, start_ms: i64, end_ms: i64) -> Result<serde_json::Value> {
        let mut q = self.fund_pairs(ft, thscode);
        q.push(("start", start_ms.to_string()));
        q.push(("end", end_ms.to_string()));
        self.get("/api/fund/performance/indicators-historical", &q).await
    }

    /// 基金回撤指标（十个区间最大回撤）。
    pub async fn fund_performance_drawdowns(&self, ft: FundType, thscode: &str) -> Result<serde_json::Value> {
        self.fund_pair("/api/fund/performance/drawdowns", ft, thscode).await
    }

    /// 基金持有人结构。`merge_scope` 合并口径（可选）。
    pub async fn fund_holders_detail(&self, ft: FundType, thscode: &str, merge_scope: Option<&str>) -> Result<serde_json::Value> {
        let mut q = self.fund_pairs(ft, thscode);
        if let Some(v) = merge_scope { q.push(("merge_scope", v.to_string())); }
        self.get("/api/fund/holders/detail", &q).await
    }

    /// 基金前十大持有人。
    pub async fn fund_holders_top(&self, ft: FundType, thscode: &str, limit: Option<u32>) -> Result<serde_json::Value> {
        let mut q = self.fund_pairs(ft, thscode);
        if let Some(v) = limit { q.push(("limit", v.to_string())); }
        self.get("/api/fund/holders/top", &q).await
    }

    /// 基金分红记录。
    pub async fn fund_dividends(&self, ft: FundType, thscode: &str) -> Result<serde_json::Value> {
        self.fund_pair("/api/fund/corporate-actions/dividends", ft, thscode).await
    }

    /// 基金诊断详情（维度/同类对比/韧性指标）。
    pub async fn fund_diagnostics(&self, ft: FundType, thscode: &str) -> Result<serde_json::Value> {
        self.fund_pair("/api/fund/diagnostics/detail", ft, thscode).await
    }

    /// 基金财务指标。
    pub async fn fund_financials_indicators(&self, ft: FundType, thscode: &str) -> Result<serde_json::Value> {
        self.fund_pair("/api/fund/financials/indicators", ft, thscode).await
    }

    /// 基金利润表。
    pub async fn fund_financials_income_statements(&self, ft: FundType, thscode: &str) -> Result<serde_json::Value> {
        self.fund_pair("/api/fund/financials/income-statements", ft, thscode).await
    }

    /// 基金资产负债表。
    pub async fn fund_financials_balance_sheets(&self, ft: FundType, thscode: &str) -> Result<serde_json::Value> {
        self.fund_pair("/api/fund/financials/balance-sheets", ft, thscode).await
    }

    /// 基金资讯列表（游标分页）。
    pub async fn fund_news_article_list(&self, ft: FundType, thscode: &str, limit: Option<u32>, offset: Option<u32>) -> Result<serde_json::Value> {
        let mut q = self.fund_pairs(ft, thscode);
        if let Some(v) = limit { q.push(("limit", v.to_string())); }
        if let Some(v) = offset { q.push(("offset", v.to_string())); }
        self.get("/api/fund/news/article-list", &q).await
    }

    /// 基金募集列表。`subscribe`: `active`（当前募集）/ `upcoming`（即将募集）。
    pub async fn fund_offerings_list(&self, subscribe: &str) -> Result<serde_json::Value> {
        self.get("/api/fund/offerings/list", &[("subscribe", subscribe.to_string())]).await
    }

    /// 场内基金行情快照（仅 ETF）。
    pub async fn fund_market_snapshot(&self, thscode: &str) -> Result<serde_json::Value> {
        self.get("/api/fund/market/snapshot", &[("thscode", thscode.to_string())]).await
    }

    /// 场内基金历史日线行情（仅 ETF，近 5 年）。`interval` 可选（默认日线）。
    pub async fn fund_market_historical(&self, thscode: &str, start_ms: i64, end_ms: i64, interval: Option<&str>) -> Result<serde_json::Value> {
        let mut q: Vec<(&str, String)> = vec![
            ("thscode", thscode.to_string()),
            ("start", start_ms.to_string()),
            ("end", end_ms.to_string()),
        ];
        if let Some(v) = interval { q.push(("interval", v.to_string())); }
        self.get("/api/fund/market/historical", &q).await
    }

    /// 基金公司详情。
    pub async fn fund_company_detail(&self, company_id: &str) -> Result<serde_json::Value> {
        self.get("/api/fund/companies/detail", &[("company_id", company_id.to_string())]).await
    }

    /// 基金经理详情。
    pub async fn fund_manager_detail(&self, manager_id: &str) -> Result<serde_json::Value> {
        self.get("/api/fund/managers/detail", &[("manager_id", manager_id.to_string())]).await
    }

    /// 基金经理从业经历。
    pub async fn fund_manager_experience(&self, manager_id: &str) -> Result<serde_json::Value> {
        self.get("/api/fund/managers/experience", &[("manager_id", manager_id.to_string())]).await
    }

    /// 基金经理投资风格。
    pub async fn fund_manager_investment_style(&self, manager_id: &str) -> Result<serde_json::Value> {
        self.get("/api/fund/managers/investment-style", &[("manager_id", manager_id.to_string())]).await
    }

    /// 基金经理业绩（代表基金/同类/基准收益序列）。`range` 必填（如 `3y`）。
    pub async fn fund_manager_performance(&self, manager_id: &str, range: &str) -> Result<serde_json::Value> {
        self.get(
            "/api/fund/managers/performance",
            &[("manager_id", manager_id.to_string()), ("range", range.to_string())],
        )
        .await
    }

    fn fund_pairs(&self, ft: FundType, thscode: &str) -> Vec<(&'static str, String)> {
        vec![
            ("fund_type", ft.as_str().to_string()),
            ("thscode", thscode.to_string()),
        ]
    }

    async fn fund_pair(&self, path: &str, ft: FundType, thscode: &str) -> Result<serde_json::Value> {
        self.get(path, &self.fund_pairs(ft, thscode)).await
    }
}
