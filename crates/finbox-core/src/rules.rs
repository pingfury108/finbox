//! A 股交易规则常量与工具函数。

/// 一手股数（A 股整手交易）。
pub const LOT_SIZE: u32 = 100;

/// 持股数量上限（硬护栏）。
pub const MAX_POSITIONS: usize = 5;

/// 单票市值占总资产上限（硬护栏）。
pub const MAX_POSITION_PCT: f64 = 0.4;

/// 费用：佣金万 2.5，最低 5 元，双边。
pub const COMMISSION_RATE: f64 = 0.00025;
pub const COMMISSION_MIN: f64 = 5.0;
/// 印花税：卖出 0.05%。
pub const STAMP_RATE: f64 = 0.0005;
/// 过户费：万 0.1，双边。
pub const TRANSFER_RATE: f64 = 0.00001;

/// 交易时段（Asia/Shanghai）。返回 `(start_minutes, end_minutes)`。
/// 上午 9:30-11:30，下午 13:00-15:00。
pub const SESSIONS: [(u32, u32); 2] = [(9 * 60 + 30, 11 * 60 + 30), (13 * 60, 15 * 60)];

/// 按 thscode 前缀判断板块涨跌幅限制比例：
/// - 主板（60/00 开头等）10%
/// - 创业板（300/301）、科创板（688）20%
/// - 北交所（4/8 开头）30%
pub fn limit_ratio(thscode: &str) -> f64 {
    if thscode.starts_with("300") || thscode.starts_with("688") {
        0.20
    } else if thscode.starts_with('4') || thscode.starts_with('8') {
        0.30
    } else {
        0.10
    }
}

/// 计算涨停价（含涨跌幅限制的买入上限价）。
pub fn limit_up_price(prev_close: f64, thscode: &str) -> f64 {
    round_price(prev_close * (1.0 + limit_ratio(thscode)))
}

/// 计算跌停价（含涨跌幅限制的卖出下限价）。
pub fn limit_down_price(prev_close: f64, thscode: &str) -> f64 {
    round_price(prev_close * (1.0 - limit_ratio(thscode)))
}

/// 四舍五入到 0.01（A 股价格最小变动单位）。
pub fn round_price(p: f64) -> f64 {
    (p * 100.0).round() / 100.0
}

/// 判断当前时刻是否处于交易时段。
/// `now_local` 为 (weekday 1-7, 当天已过分钟数)，weekday 按 ISO（1=周一, 7=周日）。
pub fn is_trading_time(weekday_iso: u32, minute_of_day: u32) -> bool {
    if weekday_iso >= 6 {
        return false; // 周六周日
    }
    SESSIONS.iter().any(|(start, end)| *start <= minute_of_day && minute_of_day < *end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_ratio_by_board() {
        assert!((limit_ratio("600519.SH") - 0.10).abs() < 1e-9);
        assert!((limit_ratio("000001.SZ") - 0.10).abs() < 1e-9);
        assert!((limit_ratio("300750.SZ") - 0.20).abs() < 1e-9);
        assert!((limit_ratio("688981.SH") - 0.20).abs() < 1e-9);
        assert!((limit_ratio("830799.BJ") - 0.30).abs() < 1e-9);
        assert!((limit_ratio("430047.BJ") - 0.30).abs() < 1e-9);
    }

    #[test]
    fn limit_prices() {
        // 主板，昨收 10 元：涨停 11.00，跌停 9.00
        assert!((limit_up_price(10.0, "600519.SH") - 11.00).abs() < 1e-9);
        assert!((limit_down_price(10.0, "600519.SH") - 9.00).abs() < 1e-9);
        // 创业板 20%：昨收 10 → 涨停 12.00
        assert!((limit_up_price(10.0, "300750.SZ") - 12.00).abs() < 1e-9);
        // 四舍五入：昨收 10.12 主板 → 涨停 11.13
        assert!((limit_up_price(10.12, "600519.SH") - 11.13).abs() < 1e-9);
    }

    #[test]
    fn trading_time() {
        // 周一 9:30 / 11:29 / 13:00 / 15:00 前
        assert!(is_trading_time(1, 9 * 60 + 30));
        assert!(is_trading_time(1, 11 * 60 + 29));
        assert!(is_trading_time(1, 13 * 60));
        assert!(is_trading_time(1, 14 * 60 + 59));
        // 边界
        assert!(!is_trading_time(1, 11 * 60 + 30)); // 午休
        assert!(!is_trading_time(1, 12 * 60));
        assert!(!is_trading_time(1, 15 * 60)); // 收盘
        assert!(!is_trading_time(1, 9 * 60 + 29)); // 开盘前
        // 周末
        assert!(!is_trading_time(6, 10 * 60));
        assert!(!is_trading_time(7, 10 * 60));
    }
}
