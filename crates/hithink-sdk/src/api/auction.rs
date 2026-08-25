//! A 股集合竞价数据：竞价快照与短线风向标基准。

use crate::{Client, Result};

impl Client {
    /// 集合竞价快照。`thscodes` 一个或多个（服务端去重）；`stage`: `live`（实时）/ `final`（终态，默认）。
    pub async fn auction_snapshot(&self, thscodes: &[&str], stage: Option<&str>) -> Result<serde_json::Value> {
        let mut q: Vec<(&str, String)> = vec![("thscodes", thscodes.join(","))];
        if let Some(s) = stage { q.push(("stage", s.to_string())); }
        self.get("/api/a-share/auction/snapshot", &q).await
    }

    /// 短线风向标竞价基准。`date`: `yyyy-MM-dd`，缺省为上海时区当日。
    pub async fn auction_short_term_benchmark(&self, date: Option<&str>) -> Result<serde_json::Value> {
        let q: Vec<(&str, String)> = date.map(|d| vec![("date", d.to_string())]).unwrap_or_default();
        self.get("/api/a-share/auction/short-term-benchmark", &q).await
    }
}
