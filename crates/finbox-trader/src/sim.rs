//! 模拟盘券商：真实行情价成交，严格遵循 A 股规则。
//!
//! 规则（与旧版 Python 引擎一致）：
//! - 交易时段：工作日 9:30-11:30 / 13:00-15:00
//! - 整手 100 股；T+1（当日买入次日可卖）
//! - 涨跌停：主板 10% / 创业科创 20% / 北交所 30%，涨停禁买、跌停禁卖
//! - 费用：佣金万 2.5（最低 5 元，双边）+ 印花税 0.05%（卖出）+ 过户费 0.001%（双边）
//! - 硬护栏：单票 ≤40% 总资产，持股 ≤5 只
//!
//! 成交价 = 最新行情快照价（盘中），无快照回退昨收价。钱是假的，价格是真的。

use chrono::{Datelike, Local, Timelike};
use finbox_core::rules::{
    is_trading_time, limit_down_price, limit_up_price, round_price, COMMISSION_MIN,
    COMMISSION_RATE, LOT_SIZE, MAX_POSITIONS, MAX_POSITION_PCT, STAMP_RATE, TRANSFER_RATE,
};
use finbox_core::{Account, Execution, OrderIntent, OrderSide, Position, RejectReason, Trade};
use finbox_store::{Db, SharedDb};

use crate::{Broker, BrokerError};

/// 模拟盘券商。内部用共享句柄的 `Mutex` 串行化 DuckDB 写操作（单写多读约束）。
pub struct SimBroker {
    db: SharedDb,
    initial_capital: f64,
}

impl SimBroker {
    pub fn new(db: SharedDb, initial_capital: f64) -> Self {
        Self { db, initial_capital }
    }
}

#[async_trait::async_trait]
impl Broker for SimBroker {
    async fn submit(&self, intent: OrderIntent) -> Result<Execution, BrokerError> {
        let mut db = self.db.lock().unwrap();
        let exec = match intent.side {
            OrderSide::Buy => buy(&mut db, self.initial_capital, &intent),
            OrderSide::Sell => sell(&mut db, self.initial_capital, &intent),
        }?;
        Ok(exec)
    }

    async fn account(&self) -> Result<Account, BrokerError> {
        let db = self.db.lock().unwrap();
        Ok(db.get_or_init_account(self.initial_capital)?)
    }

    async fn positions(&self) -> Result<Vec<Position>, BrokerError> {
        let db = self.db.lock().unwrap();
        Ok(db.positions()?)
    }
}

/// 当前时刻的 (周几ISO 1-7, 当天分钟数)。非交易时段校验用。
fn now_weekday_minute() -> (u32, u32) {
    let now = Local::now();
    (now.weekday().num_days_from_monday() + 1, now.hour() * 60 + now.minute())
}

/// 仅测试编译：置 true 时跳过交易时段校验（生产构建不存在此开关）。
#[cfg(test)]
thread_local! {
    static FORCE_TRADING_TIME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn force_trading_time() -> bool {
    FORCE_TRADING_TIME.with(|f| f.get())
}

#[cfg(not(test))]
fn force_trading_time() -> bool {
    false
}

fn check_trading_time() -> Result<(), RejectReason> {
    if force_trading_time() {
        return Ok(());
    }
    let (wd, m) = now_weekday_minute();
    if is_trading_time(wd, m) {
        Ok(())
    } else {
        Err(RejectReason::NotTradingTime)
    }
}

/// 取成交价：最新快照价优先，无快照用昨收。
fn fill_price(db: &Db, thscode: &str) -> Result<f64, RejectReason> {
    let price = db
        .latest_snapshot_price(thscode)
        .map_err(|e| RejectReason::Other(e.to_string()))?
        .or(db
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

fn buy(db: &mut Db, initial_capital: f64, intent: &OrderIntent) -> Result<Execution, RejectReason> {
    check_trading_time()?;
    if intent.quantity <= 0 || intent.quantity % LOT_SIZE != 0 {
        return Err(RejectReason::LotSize(LOT_SIZE));
    }

    let prev_close = db.prev_close(&intent.thscode).map_err(|e| RejectReason::Other(e.to_string()))?;
    let price = fill_price(db, &intent.thscode)?;
    if let Some(prev) = prev_close {
        let limit_up = limit_up_price(prev, &intent.thscode);
        if price >= limit_up {
            return Err(RejectReason::LimitUp(intent.thscode.clone()));
        }
    }

    let amount = round_price(price * intent.quantity as f64);
    let fee = fees(amount, OrderSide::Buy);
    let account = db
        .get_or_init_account(initial_capital)
        .map_err(|e| RejectReason::Other(e.to_string()))?;
    if amount + fee > account.cash {
        return Err(RejectReason::InsufficientFunds(amount + fee, account.cash));
    }

    // 硬护栏：单票仓位 ≤40% 总资产，持股 ≤5 只
    let total = db.total_asset(&account).map_err(|e| RejectReason::Other(e.to_string()))?;
    let position = db.position(&intent.thscode).map_err(|e| RejectReason::Other(e.to_string()))?;
    let post_mv = (position.as_ref().map(|p| p.quantity as f64 * price).unwrap_or(0.0)) + amount;
    if post_mv > total * MAX_POSITION_PCT {
        return Err(RejectReason::PositionLimit(MAX_POSITION_PCT * 100.0));
    }
    if position.is_none() {
        let held = db.positions().map_err(|e| RejectReason::Other(e.to_string()))?.len();
        if held >= MAX_POSITIONS {
            return Err(RejectReason::MaxPositions(MAX_POSITIONS));
        }
    }

    // 记账
    let cash_after = round_price(account.cash - amount - fee);
    db.set_account_cash(cash_after).map_err(|e| RejectReason::Other(e.to_string()))?;
    let name = if intent.name.is_empty() {
        db.ticker_name(&intent.thscode).map_err(|e| RejectReason::Other(e.to_string()))?
    } else {
        intent.name.clone()
    };
    if let Some(p) = position {
        let new_qty = p.quantity + intent.quantity;
        let avg_cost = round_price((p.avg_cost * p.quantity as f64 + amount) / new_qty as f64);
        db.upsert_position(&Position {
            thscode: p.thscode,
            name: p.name,
            quantity: new_qty,
            avg_cost,
        })
        .map_err(|e| RejectReason::Other(e.to_string()))?;
    } else {
        db.upsert_position(&Position {
            thscode: intent.thscode.clone(),
            name: name.clone(),
            quantity: intent.quantity,
            avg_cost: price,
        })
        .map_err(|e| RejectReason::Other(e.to_string()))?;
    }

    db.insert_trade(&Trade {
        thscode: intent.thscode.clone(),
        name: name.clone(),
        side: OrderSide::Buy,
        price,
        quantity: intent.quantity,
        amount,
        fee,
        decision_id: intent.decision_id,
    })
    .map_err(|e| RejectReason::Other(e.to_string()))?;

    Ok(Execution {
        intent: intent.clone(),
        price,
        amount,
        fee,
        cash_after: Some(cash_after),
    })
}

fn sell(db: &mut Db, initial_capital: f64, intent: &OrderIntent) -> Result<Execution, RejectReason> {
    check_trading_time()?;
    if intent.quantity <= 0 || intent.quantity % LOT_SIZE != 0 {
        return Err(RejectReason::LotSize(LOT_SIZE));
    }

    let position = db
        .position(&intent.thscode)
        .map_err(|e| RejectReason::Other(e.to_string()))?
        .ok_or_else(|| RejectReason::InsufficientPosition(intent.thscode.clone(), 0))?;
    if position.quantity < intent.quantity {
        return Err(RejectReason::InsufficientPosition(intent.thscode.clone(), position.quantity));
    }

    // T+1：当日买入部分不可卖
    let (start_ms, end_ms) = today_range_ms();
    let today_bought = db
        .bought_between(&intent.thscode, start_ms, end_ms)
        .map_err(|e| RejectReason::Other(e.to_string()))?;
    let sellable = position.quantity.saturating_sub(today_bought);
    if intent.quantity > sellable {
        return Err(RejectReason::TPlusOne(intent.thscode.clone(), sellable));
    }

    let prev_close = db.prev_close(&intent.thscode).map_err(|e| RejectReason::Other(e.to_string()))?;
    let price = fill_price(db, &intent.thscode)?;
    if let Some(prev) = prev_close {
        let limit_down = limit_down_price(prev, &intent.thscode);
        if price <= limit_down {
            return Err(RejectReason::LimitDown(intent.thscode.clone()));
        }
    }

    let amount = round_price(price * intent.quantity as f64);
    let fee = fees(amount, OrderSide::Sell);
    let account = db
        .get_or_init_account(initial_capital)
        .map_err(|e| RejectReason::Other(e.to_string()))?;
    let cash_after = round_price(account.cash + amount - fee);
    db.set_account_cash(cash_after).map_err(|e| RejectReason::Other(e.to_string()))?;

    let new_qty = position.quantity - intent.quantity;
    if new_qty == 0 {
        db.delete_position(&intent.thscode).map_err(|e| RejectReason::Other(e.to_string()))?;
    } else {
        db.upsert_position(&Position {
            thscode: position.thscode,
            name: position.name,
            quantity: new_qty,
            avg_cost: position.avg_cost,
        })
        .map_err(|e| RejectReason::Other(e.to_string()))?;
    }

    db.insert_trade(&Trade {
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

    Ok(Execution {
        intent: intent.clone(),
        price,
        amount,
        fee,
        cash_after: Some(cash_after),
    })
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

    /// 内存库 + 预置：昨收 10 元、最新快照 10.5 元、初始资金 200000
    fn setup_db() -> Db {
        force_trading_time_set(true);
        let db = Db::open(":memory:").unwrap();
        let bar = DailyBarRow {
            thscode: "600519.SH".into(),
            date_ms: 1,
            date: "2026-08-21".into(),
            open: 10.0,
            high: 10.8,
            low: 9.9,
            close: 10.0,
            volume: 0.0,
            turnover: 0.0,
        };
        db.insert_daily_bars(&[bar]).unwrap();
        db.insert_snapshots(
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
        db
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

    #[test]
    fn buy_happy_path() {
        let mut db = setup_db();
        let i = intent(100);
        let e = buy(&mut db, 200000.0, &i).unwrap();
        assert!((e.price - 10.5).abs() < 1e-9); // 成交价 = 快照价
        assert!((e.amount - 1050.0).abs() < 1e-9);
        // 费用：佣金 max(1050*0.00025, 5)=5 + 过户费 0.0105 => 5.01
        assert!((e.fee - 5.01).abs() < 1e-9, "fee={}", e.fee);
        assert!((e.cash_after.unwrap() - (200000.0 - 1050.0 - 5.01)).abs() < 1e-9);
        let p = db.position("600519.SH").unwrap().unwrap();
        assert_eq!(p.quantity, 100);
        assert!((p.avg_cost - 10.5).abs() < 1e-9);
    }

    #[test]
    fn buy_rejects_lot_size() {
        let mut db = setup_db();
        assert!(matches!(buy(&mut db, 200000.0, &intent(150)), Err(RejectReason::LotSize(_))));
    }

    #[test]
    fn buy_rejects_limit_up() {
        let mut db = setup_db();
        // 昨收 10 主板涨停 11.0；快照价 10.5 未涨停
        // 造一个涨停价：把快照价改成 11.0
        db.insert_snapshots(
            200,
            &[SnapshotRow {
                thscode: "600519.SH".into(),
                last_price: 11.0,
                price_change: 1.0,
                price_change_ratio_pct: 10.0,
                open_price: 10.0,
                high_price: 11.0,
                low_price: 10.0,
                prev_price: 10.0,
                volume: 1000.0,
                turnover: 11000.0,
            }],
        )
        .unwrap();
        assert!(matches!(buy(&mut db, 200000.0, &intent(100)), Err(RejectReason::LimitUp(_))));
    }

    #[test]
    fn buy_rejects_insufficient_funds() {
        let mut db = setup_db();
        // 先初始化账户（cash=200000），再改成 500
        let _ = db.get_or_init_account(200000.0).unwrap();
        db.set_account_cash(500.0).unwrap();
        // 100 股 * 10.5 = 1050 > 500
        assert!(matches!(buy(&mut db, 200000.0, &intent(100)), Err(RejectReason::InsufficientFunds(_, _))));
    }

    #[test]
    fn buy_rejects_position_limit() {
        let mut db = setup_db();
        // 单票仓位 40% 上限：总资产 20 万 → 单票市值 ≤ 8 万
        // 一次买 10000 股 * 10.5 = 10.5 万 > 8 万
        assert!(matches!(buy(&mut db, 200000.0, &intent(10000)), Err(RejectReason::PositionLimit(_))));
    }

    #[test]
    fn buy_rejects_max_positions() {
        let mut db = setup_db();
        // 已持有 5 只 → 拒绝新开仓
        for i in 1..=5 {
            db.upsert_position(&Position {
                thscode: format!("60000{i}.SH"),
                name: "x".into(),
                quantity: 100,
                avg_cost: 1.0,
            })
            .unwrap();
        }
        let r = buy(&mut db, 200000.0, &intent(100));
        assert!(matches!(r, Err(RejectReason::MaxPositions(_))));
    }

    #[test]
    fn sell_happy_path_and_t1() {
        let mut db = setup_db();
        // 先买 100 股（写入今日 BUY 记录 + 持仓）
        let b = OrderIntent {
            thscode: "600519.SH".into(),
            name: "贵州茅台".into(),
            side: OrderSide::Buy,
            quantity: 100,
            decision_id: None,
        };
        buy(&mut db, 200000.0, &b).unwrap();

        // T+1：今日买入 100 股，可卖 0 → 卖出被拒
        let s = OrderIntent {
            thscode: "600519.SH".into(),
            name: "贵州茅台".into(),
            side: OrderSide::Sell,
            quantity: 100,
            decision_id: None,
        };
        assert!(matches!(sell(&mut db, 200000.0, &s), Err(RejectReason::TPlusOne(_, 0))));

        // 模拟昨日买入：把 BUY 记录 ts 改成昨日，即可卖出
        db.backdate_buys("600519.SH").unwrap();
        let e = sell(&mut db, 200000.0, &s).unwrap();
        assert!((e.price - 10.5).abs() < 1e-9);
        // 卖出费用：佣金5 + 印花税 0.525 + 过户费 0.0105 = 5.54
        assert!((e.fee - 5.54).abs() < 1e-9, "fee={}", e.fee);
        assert!(db.position("600519.SH").unwrap().is_none());
    }

    #[test]
    fn sell_rejects_limit_down() {
        let mut db = setup_db();
        // 昨日买入
        let b = OrderIntent {
            thscode: "600519.SH".into(),
            name: "贵州茅台".into(),
            side: OrderSide::Buy,
            quantity: 100,
            decision_id: None,
        };
        buy(&mut db, 200000.0, &b).unwrap();
        db.backdate_buys("600519.SH").unwrap();
        // 快照价改 9.0 = 跌停
        db.insert_snapshots(
            300,
            &[SnapshotRow {
                thscode: "600519.SH".into(),
                last_price: 9.0,
                price_change: -1.0,
                price_change_ratio_pct: -10.0,
                open_price: 9.0,
                high_price: 9.5,
                low_price: 8.9,
                prev_price: 10.0,
                volume: 1000.0,
                turnover: 9000.0,
            }],
        )
        .unwrap();
        let s = OrderIntent {
            thscode: "600519.SH".into(),
            name: "贵州茅台".into(),
            side: OrderSide::Sell,
            quantity: 100,
            decision_id: None,
        };
        assert!(matches!(sell(&mut db, 200000.0, &s), Err(RejectReason::LimitDown(_))));
    }

    #[test]
    fn fees_calc() {
        // 买入 10000 元：佣金万2.5=2.5 不足最低5 → 5；过户费 0.1
        assert!((fees(10000.0, OrderSide::Buy) - 5.1).abs() < 1e-9);
        // 买入 100000 元：佣金 25 + 过户费 1 = 26
        assert!((fees(100000.0, OrderSide::Buy) - 26.0).abs() < 1e-9);
        // 卖出 100000 元：佣金 25 + 印花税 50 + 过户费 1 = 76
        assert!((fees(100000.0, OrderSide::Sell) - 76.0).abs() < 1e-9);
    }
}
