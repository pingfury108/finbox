//! A 股交易规则常量与工具函数。

/// 一手股数（主板/创业板/北交所整手）。
pub const LOT_SIZE: u32 = 100;

/// 科创板最低买入股数（之后可按 1 股递增）。
pub const STAR_MIN_LOT: u32 = 200;

/// 板块资金门槛（开通权限所需初始资金，元）。
pub const STAR_BOARD_MIN_CAPITAL: f64 = 500_000.0;   // 科创板
pub const BJ_BOARD_MIN_CAPITAL: f64 = 500_000.0;     // 北交所
pub const GEM_BOARD_MIN_CAPITAL: f64 = 100_000.0;    // 创业板

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

/// 是否科创板（688/689 开头）。
pub fn is_star_board(thscode: &str) -> bool {
    thscode.starts_with("688") || thscode.starts_with("689")
}

/// 是否北交所（43/83/87/88/920 开头）。
pub fn is_bj_board(thscode: &str) -> bool {
    thscode.starts_with("43") || thscode.starts_with('8') || thscode.starts_with("920")
}

/// 是否创业板（300/301/302 开头）。
pub fn is_gem_board(thscode: &str) -> bool {
    thscode.starts_with("300") || thscode.starts_with("301") || thscode.starts_with("302")
}

/// 板块权限：按账户初始资金（对应开通权限时点的资产要求，开通后跌破不影响）。
pub fn board_allowed(initial_capital: f64, thscode: &str) -> bool {
    if is_star_board(thscode) {
        initial_capital >= STAR_BOARD_MIN_CAPITAL
    } else if is_bj_board(thscode) {
        initial_capital >= BJ_BOARD_MIN_CAPITAL
    } else if is_gem_board(thscode) {
        initial_capital >= GEM_BOARD_MIN_CAPITAL
    } else {
        true // 主板无门槛
    }
}

/// 买入数量校验：科创板 ≥200 股（之后 1 股递增）；其他板块 100 整手。
pub fn is_valid_buy_quantity(thscode: &str, qty: u32) -> bool {
    if qty == 0 {
        return false;
    }
    if is_star_board(thscode) {
        qty >= STAR_MIN_LOT
    } else {
        qty % LOT_SIZE == 0
    }
}

/// 卖出数量校验：整手卖出；持仓含零股时允许一次性清仓（不足整手部分一次卖完）。
pub fn is_valid_sell_quantity(thscode: &str, qty: u32, holding: u32) -> bool {
    if qty == 0 || qty > holding {
        return false;
    }
    if is_star_board(thscode) {
        return qty >= STAR_MIN_LOT || qty == holding;
    }
    qty % LOT_SIZE == 0 || qty == holding
}

/// 按 thscode 前缀与名称判断板块涨跌幅限制比例：
/// - 主板 10%；主板 ST/*ST 5%
/// - 创业板/科创板 20%（创业板 ST 注册制后仍 20%）
/// - 北交所 30%
pub fn limit_ratio_with_name(thscode: &str, name: &str) -> f64 {
    if is_gem_board(thscode) || is_star_board(thscode) {
        0.20
    } else if is_bj_board(thscode) {
        0.30
    } else if name.contains("ST") {
        0.05 // 主板风险警示股
    } else {
        0.10
    }
}

/// 兼容旧签名（无名称时按板块默认，不含 ST 判断）。
pub fn limit_ratio(thscode: &str) -> f64 {
    limit_ratio_with_name(thscode, "")
}

/// 规范化 A 股 thscode：LLM 可能返回纯 6 位代码，补全交易所后缀。
/// 规则：60/68→SH，00/30/01→SZ，4/8→BJ；已带后缀或非 6 位数字则原样返回。
pub fn normalize_thscode(symbol: &str) -> String {
    let s = symbol.trim().to_uppercase();
    if s.contains('.') {
        return s;
    }
    if !(s.len() == 6 && s.chars().all(|c| c.is_ascii_digit())) {
        return s;
    }
    let suffix = if s.starts_with("60") || s.starts_with("68") || s.starts_with("9") {
        "SH"
    } else if s.starts_with('4') || s.starts_with('8') {
        "BJ"
    } else {
        "SZ"
    };
    format!("{s}.{suffix}")
}

/// 计算涨停价（含涨跌幅限制的买入上限价）。
pub fn limit_up_price(prev_close: f64, thscode: &str, name: &str) -> f64 {
    round_price(prev_close * (1.0 + limit_ratio_with_name(thscode, name)))
}

/// 计算跌停价（含涨跌幅限制的卖出下限价）。
pub fn limit_down_price(prev_close: f64, thscode: &str, name: &str) -> f64 {
    round_price(prev_close * (1.0 - limit_ratio_with_name(thscode, name)))
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
        assert!((limit_up_price(10.0, "600519.SH", "贵州茅台") - 11.00).abs() < 1e-9);
        assert!((limit_down_price(10.0, "600519.SH", "贵州茅台") - 9.00).abs() < 1e-9);
        // 主板 ST：±5%
        assert!((limit_up_price(10.0, "600519.SH", "ST某某") - 10.50).abs() < 1e-9);
        // 创业板 20%：昨收 10 → 涨停 12.00
        assert!((limit_up_price(10.0, "300750.SZ", "宁德时代") - 12.00).abs() < 1e-9);
        // 四舍五入：昨收 10.12 主板 → 涨停 11.13
        assert!((limit_up_price(10.12, "600519.SH", "贵州茅台") - 11.13).abs() < 1e-9);
    }

    #[test]
    fn quantity_rules() {
        // 主板买入 100 整手
        assert!(is_valid_buy_quantity("600519.SH", 100));
        assert!(!is_valid_buy_quantity("600519.SH", 150));
        // 科创板 ≥200 起，1 股递增
        assert!(is_valid_buy_quantity("688981.SH", 200));
        assert!(is_valid_buy_quantity("688981.SH", 201));
        assert!(!is_valid_buy_quantity("688981.SH", 100));
        // 卖出：整手或一次性清仓
        assert!(is_valid_sell_quantity("600519.SH", 100, 150));
        assert!(is_valid_sell_quantity("600519.SH", 150, 150)); // 零股一次清
        assert!(!is_valid_sell_quantity("600519.SH", 50, 150)); // 零股不能只卖一部分
        assert!(is_valid_sell_quantity("688981.SH", 250, 250));
    }

    #[test]
    fn board_permission() {
        assert!(board_allowed(100_000.0, "600519.SH"));  // 主板无门槛
        assert!(board_allowed(100_000.0, "300750.SZ"));  // 创业板 10万
        assert!(!board_allowed(50_000.0, "300750.SZ"));  // 不足10万
        assert!(board_allowed(500_000.0, "688981.SH"));  // 科创板 50万
        assert!(!board_allowed(300_000.0, "688981.SH")); // 30万不够
        assert!(!board_allowed(300_000.0, "830799.BJ")); // 北交所 50万
        assert!(board_allowed(500_000.0, "830799.BJ"));
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

    #[test]
    fn normalize_thscode_cases() {
        assert_eq!(normalize_thscode("600519"), "600519.SH");
        assert_eq!(normalize_thscode("688981"), "688981.SH");
        assert_eq!(normalize_thscode("000001"), "000001.SZ");
        assert_eq!(normalize_thscode("300750"), "300750.SZ");
        assert_eq!(normalize_thscode("830799"), "830799.BJ");
        assert_eq!(normalize_thscode("600519.SH"), "600519.SH"); // 已带后缀
        assert_eq!(normalize_thscode("abc"), "ABC"); // 非代码原样
    }
}
