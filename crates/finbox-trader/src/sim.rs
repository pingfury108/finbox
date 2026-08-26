//! 模拟盘券商：真实行情价成交，严格遵循 A 股规则。
//!
//! 规则（与旧版 Python 引擎一致）：
//! - 交易时段：工作日 9:30-11:30 / 13:00-15:00
//! - 整手 100 股；T+1（当日买入次日可卖）
//! - 涨跌停：主板 10% / 创业科创 20% / 北交所 30%，涨停禁买、跌停禁卖
//! - 费用：佣金万 2.5（最低 5 元，双边）+ 印花税 0.05%（卖出）+ 过户费 0.001%（双边）
//! - 硬护栏：单票 ≤20% 总资产，持股 ≤3 只
//!
//! 成交价 = 最新行情快照价（盘中），无快照回退昨收价。钱是假的，价格是真的。
//!
//! 双库架构：行情（价格/昨收）读 market 库，账户（持仓/现金/流水）读写 account 库。
//! 铁律：**不同时持有两把锁** —— 先锁 market 取价，再锁 account 交易。

use chrono::{Datelike, Local, Timelike};
use finbox_core::rules::{
    board_allowed, is_trading_time, is_valid_buy_quantity, is_valid_sell_quantity,
    limit_down_price, limit_up_price, round_price, COMMISSION_MIN, COMMISSION_RATE,
    STAMP_RATE, TRANSFER_RATE,
};
use finbox_core::{Account, Execution, OrderIntent, OrderSide, Position, RejectReason, Trade};
use finbox_store::SharedDb;

use crate::{Broker, BrokerError};

/// 模拟盘券商。持有行情库（只读）与账户库（读写）。
pub struct SimBroker {
    market: SharedDb,
    acct: SharedDb,
    initial_capital: f64,
}

impl SimBroker {
    pub fn new(market: SharedDb, acct: SharedDb, initial_capital: f64) -> Self {
        Self { market, acct, initial_capital }
    }
}

#[async_trait::async_trait]
impl Broker for SimBroker {
    async fn submit(&self, intent: OrderIntent) -> Result<Execution, BrokerError> {
        // 先取行情（锁 market，短）→ 再交易（锁 acct）
        let (price, prev_close) = {
            let m = self.market.lock().unwrap();
            let price = market_price(&m, &intent.thscode)?;
            // 新股上市前 5 个交易日无涨跌幅限制：不做涨跌停拦截
            let is_new = m.days_since_first_bar(&intent.thscode).ok().flatten().map(|d| d < 5).unwrap_or(false);
            let prev = if is_new {
                None
            } else {
                m.prev_close(&intent.thscode).map_err(BrokerError::Store)?
            };
            (price, prev)
        };
        let mut acct = self.acct.lock().unwrap();
        let exec = match intent.side {
            OrderSide::Buy => buy(&mut acct, &intent, price, prev_close, self.initial_capital),
            OrderSide::Sell => sell(&mut acct, &intent, price, prev_close, self.initial_capital),
        }?;
        Ok(exec)
    }

    async fn account(&self) -> Result<Account, BrokerError> {
        let db = self.acct.lock().unwrap();
        Ok(db.get_or_init_account(self.initial_capital)?)
    }

    async fn positions(&self) -> Result<Vec<Position>, BrokerError> {
        let db = self.acct.lock().unwrap();
        Ok(db.positions()?)
    }
}

/// 成交价：最新快照价优先，无快照用昨收。
fn market_price(market: &finbox_store::Db, thscode: &str) -> Result<f64, RejectReason> {
    let price = market
        .latest_snapshot_price(thscode)
        .map_err(|e| RejectReason::Other(e.to_string()))?
        .or(market
            .prev_close(thscode)
            .map_err(|e| RejectReason::Other(e.to_string()))?)
        .ok_or_else(|| RejectReason::NoPrice(thscode.to_string()))?;
    if price <= 0.0 {
        return Err(RejectReason::NoPrice(thscode.to_string()));
    }
    Ok(price)
}

fn fees(amount: f64, side: OrderSide) -> f64 {
    let commission = (amount * COMMISSION_RATE).max(COMMISSION_MIN);
    let stamp = if side == OrderSide::Sell { amount * STAMP_RATE } else { 0.0 };
    round_price(commission + stamp + amount * TRANSFER_RATE)
}

fn buy(
    acct: &mut finbox_store::Db,
    intent: &OrderIntent,
    price: f64,
    prev_close: Option<f64>,
    initial_capital: f64,
) -> Result<Execution, RejectReason> {
    check_trading_time()?;
    if !is_valid_buy_quantity(&intent.thscode, intent.quantity) {
        return Err(RejectReason::LotSize(intent.quantity));
    }
    // 板块资金权限（按初始资金，对应真实开通权限时点）
    if !board_allowed(initial_capital, &intent.thscode) {
        return Err(RejectReason::BoardNotAllowed(intent.thscode.clone()));
    }
    if let Some(prev) = prev_close {
        let limit_up = limit_up_price(prev, &intent.thscode, &intent.name);
        if price >= limit_up {
            return Err(RejectReason::LimitUp(intent.thscode.clone()));
        }
    }

    let amount = round_price(price * intent.quantity as f64);
    let fee = fees(amount, OrderSide::Buy);
    let account = acct
        .get_or_init_account(initial_capital)
        .map_err(|e| RejectReason::Other(e.to_string()))?;
    if amount + fee > account.cash {
        return Err(RejectReason::InsufficientFunds(amount + fee, account.cash));
    }

    // 硬护栏：单票 ≤20% 总资产，持股 ≤3 只（用 avg_cost 估算，保守够用）
    let total = acct
        .total_asset_estimate(&account)
        .map_err(|e| RejectReason::Other(e.to_string()))?;
    let position = acct
        .position(&intent.thscode)
        .map_err(|e| RejectReason::Other(e.to_string()))?;
    let post_mv = (position.as_ref().map(|p| p.quantity as f64 * price).unwrap_or(0.0)) + amount;
    if post_mv > total * crate::MAX_POSITION_PCT {
        return Err(RejectReason::PositionLimit(crate::MAX_POSITION_PCT * 100.0));
    }
    if position.is_none() {
        let held = acct.positions().map_err(|e| RejectReason::Other(e.to_string()))?.len();
        if held >= crate::MAX_POSITIONS {
            return Err(RejectReason::MaxPositions(crate::MAX_POSITIONS));
        }
    }

    // 记账
    let cash_after = round_price(account.cash - amount - fee);
    acct.set_account_cash(cash_after).map_err(|e| RejectReason::Other(e.to_string()))?;
    let name = if intent.name.is_empty() {
        // 名称在行情库，这里可能缺失；账户库无法查行情，用占位并在记录时保留
        intent.thscode.clone()
    } else {
        intent.name.clone()
    };
    if let Some(p) = position {
        let new_qty = p.quantity + intent.quantity;
        let avg_cost = round_price((p.avg_cost * p.quantity as f64 + amount) / new_qty as f64);
        acct.upsert_position(&Position {
            thscode: p.thscode,
            name: p.name,
            quantity: new_qty,
            avg_cost,
        })
        .map_err(|e| RejectReason::Other(e.to_string()))?;
    } else {
        acct.upsert_position(&Position {
            thscode: intent.thscode.clone(),
            name,
            quantity: intent.quantity,
            avg_cost: price,
        })
        .map_err(|e| RejectReason::Other(e.to_string()))?;
    }

    acct.insert_trade(&Trade {
        thscode: intent.thscode.clone(),
        name: intent.name.clone(),
        side: OrderSide::Buy,
        price,
        quantity: intent.quantity,
        amount,
        fee,
        decision_id: intent.decision_id,
    })
    .map_err(|e| RejectReason::Other(e.to_string()))?;

    Ok(Execution { intent: intent.clone(), price, amount, fee, cash_after: Some(cash_after) })
}

fn sell(
    acct: &mut finbox_store::Db,
    intent: &OrderIntent,
    price: f64,
    prev_close: Option<f64>,
    initial_capital: f64,
) -> Result<Execution, RejectReason> {
    check_trading_time()?;

    let position = acct
        .position(&intent.thscode)
        .map_err(|e| RejectReason::Other(e.to_string()))?
        .ok_or_else(|| RejectReason::InsufficientPosition(intent.thscode.clone(), 0))?;
    if position.quantity < intent.quantity {
        return Err(RejectReason::InsufficientPosition(intent.thscode.clone(), position.quantity));
    }
    // 整手卖出；零股（除权送股产生）须一次性清仓
    if !is_valid_sell_quantity(&intent.thscode, intent.quantity, position.quantity) {
        return Err(RejectReason::LotSize(intent.quantity));
    }

    // T+1：当日买入部分不可卖
    let (start_ms, end_ms) = today_range_ms();
    let today_bought = acct
        .bought_between(&intent.thscode, start_ms, end_ms)
        .map_err(|e| RejectReason::Other(e.to_string()))?;
    let sellable = position.quantity.saturating_sub(today_bought);
    if intent.quantity > sellable {
        return Err(RejectReason::TPlusOne(intent.thscode.clone(), sellable));
    }

    if let Some(prev) = prev_close {
        let limit_down = limit_down_price(prev, &intent.thscode, &position.name);
        if price <= limit_down {
            return Err(RejectReason::LimitDown(intent.thscode.clone()));
        }
    }

    let amount = round_price(price * intent.quantity as f64);
    let fee = fees(amount, OrderSide::Sell);
    let account = acct
        .get_or_init_account(initial_capital)
        .map_err(|e| RejectReason::Other(e.to_string()))?;
    let cash_after = round_price(account.cash + amount - fee);
    acct.set_account_cash(cash_after).map_err(|e| RejectReason::Other(e.to_string()))?;

    let new_qty = position.quantity - intent.quantity;
    if new_qty == 0 {
        acct.delete_position(&intent.thscode).map_err(|e| RejectReason::Other(e.to_string()))?;
    } else {
        acct.upsert_position(&Position {
            thscode: position.thscode,
            name: position.name,
            quantity: new_qty,
            avg_cost: position.avg_cost,
        })
        .map_err(|e| RejectReason::Other(e.to_string()))?;
    }

    acct.insert_trade(&Trade {
        thscode: intent.thscode.clone(),
        name: intent.name.clone(),
        side: OrderSide::Sell,
        price,
        quantity: intent.quantity,
        amount,
        fee,
        decision_id: intent.decision_id,
    })
    .map_err(|e| RejectReason::Other(e.to_string()))?;

    Ok(Execution { intent: intent.clone(), price, amount, fee, cash_after: Some(cash_after) })
}

/// 当前时刻的 (周几ISO 1-7, 当天分钟数)。
fn now_weekday_minute() -> (u32, u32) {
    let now = Local::now();
    (now.weekday().num_days_from_monday() + 1, now.hour() * 60 + now.minute())
}

/// 仅测试编译：置 true 时跳过交易时段校验。
#[cfg(test)]
thread_local! {
    static FORCE_TRADING_TIME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn check_trading_time() -> Result<(), RejectReason> {
    if FORCE_TRADING_TIME.with(|f| f.get()) {
        Ok(())
    } else {
        let (wd, m) = now_weekday_minute();
        if is_trading_time(wd, m) {
            Ok(())
        } else {
            Err(RejectReason::NotTradingTime)
        }
    }
}

#[cfg(not(test))]
fn check_trading_time() -> Result<(), RejectReason> {
    let (wd, m) = now_weekday_minute();
    if is_trading_time(wd, m) {
        Ok(())
    } else {
        Err(RejectReason::NotTradingTime)
    }
}

/// 今日 00:00 ~ 次日 00:00（Asia/Shanghai）毫秒时间戳。
fn today_range_ms() -> (i64, i64) {
    let now = Local::now();
    let start = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(chrono::Local)
        .unwrap()
        .timestamp_millis();
    (start, start + 86_400_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use finbox_store::{DailyBarRow, SnapshotRow};

    fn force_trading_time_set(v: bool) {
        FORCE_TRADING_TIME.with(|f| f.set(v));
    }

    /// 内存行情库 + 账户库。
    fn setup() -> (SharedDb, SharedDb) {
        force_trading_time_set(true);
        let market = finbox_store::open_market_shared(":memory:").unwrap();
        let acct = finbox_store::open_account_shared(":memory:").unwrap();
        {
            let m = market.lock().unwrap();
            m.insert_daily_bars(&[DailyBarRow {
                thscode: "600519.SH".into(),
                date_ms: 1,
                date: "2026-08-21".into(),
                open: 10.0,
                high: 10.8,
                low: 9.9,
                close: 10.0,
                volume: 0.0,
                turnover: 0.0,
            }])
            .unwrap();
            m.insert_snapshots(
                100,
                &[SnapshotRow {
                    thscode: "600519.SH".into(),
                    last_price: 10.5,
                    price_change: 0.5,
                    price_change_ratio_pct: 5.0,
                    open_price: 10.2,
                    high_price: 10.8,
                    low_price: 10.1,
                    prev_price: 10.0,
                    volume: 1000.0,
                    turnover: 10500.0,
                }],
            )
            .unwrap();
        }
        (market, acct)
    }

    fn intent(qty: u32) -> OrderIntent {
        OrderIntent {
            thscode: "600519.SH".into(),
            name: "贵州茅台".into(),
            side: OrderSide::Buy,
            quantity: qty,
            decision_id: None,
        }
    }

    fn buy_direct(acct: &mut finbox_store::Db, price: f64, qty: u32) -> Result<Execution, RejectReason> {
        buy(acct, &intent(qty), price, Some(10.0), 200000.0)
    }

    #[test]
    fn buy_happy_path() {
        let (m, a) = setup();
        let mut acct = a.lock().unwrap();
        let e = buy_direct(&mut acct, 10.5, 100).unwrap();
        assert!((e.price - 10.5).abs() < 1e-9);
        assert!((e.amount - 1050.0).abs() < 1e-9);
        assert!((e.fee - 5.01).abs() < 1e-9, "fee={}", e.fee);
        let p = acct.position("600519.SH").unwrap().unwrap();
        assert_eq!(p.quantity, 100);
    }

    #[test]
    fn buy_rejects_lot_size() {
        let (_, a) = setup();
        let mut acct = a.lock().unwrap();
        assert!(matches!(buy_direct(&mut acct, 10.5, 150), Err(RejectReason::LotSize(_))));
    }

    #[test]
    fn buy_rejects_limit_up() {
        let (_, a) = setup();
        let mut acct = a.lock().unwrap();
        // 昨收 10 → 涨停 11；以 11 元买 → 拒
        assert!(matches!(buy_direct(&mut acct, 11.0, 100), Err(RejectReason::LimitUp(_))));
    }

    #[test]
    fn buy_rejects_insufficient_funds() {
        let (_, a) = setup();
        let mut acct = a.lock().unwrap();
        let _ = acct.get_or_init_account(200000.0).unwrap();
        acct.set_account_cash(500.0).unwrap();
        assert!(matches!(buy_direct(&mut acct, 10.5, 100), Err(RejectReason::InsufficientFunds(_, _))));
    }

    #[test]
    fn buy_rejects_max_positions() {
        let (_, a) = setup();
        let mut acct = a.lock().unwrap();
        for i in 1..=3 {
            acct.upsert_position(&Position {
                thscode: format!("60000{i}.SH"),
                name: "x".into(),
                quantity: 100,
                avg_cost: 1.0,
            })
            .unwrap();
        }
        assert!(matches!(buy_direct(&mut acct, 10.5, 100), Err(RejectReason::MaxPositions(_))));
    }

    #[test]
    fn sell_happy_path_and_t1() {
        let (_, a) = setup();
        let mut acct = a.lock().unwrap();
        buy_direct(&mut acct, 10.5, 100).unwrap();
        // T+1：今日买入不可卖
        let s = OrderIntent {
            thscode: "600519.SH".into(),
            name: "贵州茅台".into(),
            side: OrderSide::Sell,
            quantity: 100,
            decision_id: None,
        };
        assert!(matches!(sell(&mut acct, &s, 10.5, Some(10.0), 200000.0), Err(RejectReason::TPlusOne(_, 0))));
        // 回拨到昨日
        acct.backdate_buys("600519.SH").unwrap();
        let e = sell(&mut acct, &s, 10.5, Some(10.0), 200000.0).unwrap();
        assert!((e.fee - 5.54).abs() < 1e-9, "fee={}", e.fee);
        assert!(acct.position("600519.SH").unwrap().is_none());
    }

    #[test]
    fn sell_rejects_limit_down() {
        let (_, a) = setup();
        let mut acct = a.lock().unwrap();
        buy_direct(&mut acct, 10.5, 100).unwrap();
        acct.backdate_buys("600519.SH").unwrap();
        let s = OrderIntent {
            thscode: "600519.SH".into(),
            name: "贵州茅台".into(),
            side: OrderSide::Sell,
            quantity: 100,
            decision_id: None,
        };
        // 昨收 10 → 跌停 9；以 9 元卖 → 拒
        assert!(matches!(sell(&mut acct, &s, 9.0, Some(10.0), 200000.0), Err(RejectReason::LimitDown(_))));
    }

    #[test]
    fn fees_calc() {
        assert!((fees(10000.0, OrderSide::Buy) - 5.1).abs() < 1e-9);
        assert!((fees(100000.0, OrderSide::Buy) - 26.0).abs() < 1e-9);
        assert!((fees(100000.0, OrderSide::Sell) - 76.0).abs() < 1e-9);
    }
}
