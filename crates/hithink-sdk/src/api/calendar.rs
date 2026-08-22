//! A 股交易日历（固定窗口：近一年）。

use crate::{Client, Result};

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct TradingDay {
    /// 交易日（毫秒，Asia/Shanghai 00:00:00）
    pub date_ms: i64,
    /// 可读日期，格式 `yyyyMMdd`
    pub date: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct TradingDaysData {
    pub timestamp: i64,
    /// 按日期升序
    pub item: Vec<TradingDay>,
}

impl TradingDaysData {
    /// 今天是否为交易日（按本地日期 `yyyyMMdd` 匹配）。
    pub fn contains_date(&self, yyyymmdd: &str) -> bool {
        self.item.iter().any(|d| d.date == yyyymmdd)
    }

    /// 最近一个交易日（含今天，若今天是交易日）。
    pub fn latest_on_or_before(&self, yyyymmdd: &str) -> Option<&TradingDay> {
        self.item.iter().rev().find(|d| d.date.as_str() <= yyyymmdd)
    }
}

impl Client {
    /// A 股近一年交易日序列。窗口固定，不支持自定义范围。
    pub async fn trading_days(&self) -> Result<TradingDaysData> {
        self.get("/api/a-share/calendar/trading-days", &[]).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> TradingDaysData {
        TradingDaysData {
            timestamp: 0,
            item: vec![
                TradingDay { date_ms: 1, date: "20250102".into() },
                TradingDay { date_ms: 2, date: "20250103".into() },
                TradingDay { date_ms: 3, date: "20250106".into() },
            ],
        }
    }

    #[test]
    fn contains_and_latest() {
        let d = sample();
        assert!(d.contains_date("20250103"));
        assert!(!d.contains_date("20250104"));
        assert_eq!(d.latest_on_or_before("20250104").unwrap().date, "20250103");
        assert_eq!(d.latest_on_or_before("20250101"), None);
    }

    #[test]
    fn parse() {
        let json = r#"{"timestamp": 1, "item": [{"date_ms": 1735660800000, "date": "20250101"}]}"#;
        let d: TradingDaysData = serde_json::from_str(json).unwrap();
        assert_eq!(d.item[0].date, "20250101");
    }
}
