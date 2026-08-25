//! A 股财务数据：利润表 / 资产负债表 / 现金流量表多期序列 + 五类财务指标。

use crate::{Client, Result};

impl Client {
    /// 合并利润表多期序列。`period`: `1`（年报）/ `2`（中报）等，见文档；`limit` 与 `start`/`end` 二选一。
    pub async fn income_statements(
        &self,
        thscode: &str,
        period: &str,
        limit: Option<u32>,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<serde_json::Value> {
        self.financials("/api/a-share/financials/income-statements", thscode, period, limit, start, end).await
    }

    /// 合并资产负债表多期序列。
    pub async fn balance_sheets(
        &self,
        thscode: &str,
        period: &str,
        limit: Option<u32>,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<serde_json::Value> {
        self.financials("/api/a-share/financials/balance-sheets", thscode, period, limit, start, end).await
    }

    /// 合并现金流量表多期序列。
    pub async fn cash_flow_statements(
        &self,
        thscode: &str,
        period: &str,
        limit: Option<u32>,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<serde_json::Value> {
        self.financials("/api/a-share/financials/cash-flow-statements", thscode, period, limit, start, end).await
    }

    /// 财务指标（成长/盈利/偿债/营运/现金流五类）。`report` 报告期，如 `2025-1`。
    pub async fn financial_indicators(&self, thscode: &str, report: &str) -> Result<serde_json::Value> {
        self.get(
            "/api/a-share/financials/indicators",
            &[("thscode", thscode.to_string()), ("report", report.to_string())],
        )
        .await
    }

    async fn financials(
        &self,
        path: &str,
        thscode: &str,
        period: &str,
        limit: Option<u32>,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<serde_json::Value> {
        let mut q: Vec<(&str, String)> = vec![
            ("thscode", thscode.to_string()),
            ("period", period.to_string()),
        ];
        if let Some(v) = limit { q.push(("limit", v.to_string())); }
        if let Some(v) = start { q.push(("start", v.to_string())); }
        if let Some(v) = end { q.push(("end", v.to_string())); }
        self.get(path, &q).await
    }
}
