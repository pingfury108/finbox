//! 硬性风控层：独立于 LLM，不可绕过。
//!
//! - 单票止损：亏损 ≥ 5% 强制清仓
//! - 分批止盈：盈利 ≥ 15% 减半仓
//! - 持仓超期：超过 N 天且无起色（现价 < 成本）强制清仓
//! - 账户熔断：总资产回撤 ≥ 5% 时停止买入（熔断期）
//!
//! 双库架构：行情（价格/涨跌家数）读 market 库，账户（持仓/熔断状态）读写 account 库。
//! 铁律：**不同时持有两把锁** —— 先读持仓（acct）解锁，再读价格（market）解锁，最后写状态（acct）。

use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use chrono::Utc;
use finbox_core::{OrderIntent, OrderSide, Position};
use finbox_store::SharedDb;

/// 风控参数。
#[derive(Debug, Clone)]
pub struct RiskConfig {
    /// 单票止损阈值（亏损比例）
    pub stop_loss_pct: f64,
    /// 止盈阈值（盈利比例，达到则减半）
    pub take_profit_pct: f64,
    /// 持仓超期天数（超过且无起色则清仓）
    pub max_holding_days: u32,
    /// 账户熔断回撤阈值
    pub fuse_drawdown_pct: f64,
    /// 熔断持续天数
    pub fuse_days: u32,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            stop_loss_pct: 0.05,
            take_profit_pct: 0.15,
            max_holding_days: 20,
            fuse_drawdown_pct: 0.05,
            fuse_days: 5,
        }
    }
}

/// 风控评估结果。
#[derive(Debug, Default)]
pub struct RiskReport {
    /// 强制卖出的委托意图（止损/止盈/超期）
    pub forced_sells: Vec<OrderIntent>,
    /// 是否可买入（未熔断）
    pub can_buy: bool,
    /// 目标总仓位上限（随市场状态浮动）
    pub max_total_pct: f64,
    /// 市场状态：risk-on / neutral / risk-off
    pub regime: String,
    /// 备注（熔断状态等）
    pub note: String,
}

/// 市场状态 → 目标总仓位上限。
fn regime_max_total(breadth_ratio: f64) -> (&'static str, f64) {
    if breadth_ratio >= 0.6 {
        ("risk-on", 0.60)
    } else if breadth_ratio >= 0.4 {
        ("neutral", 0.40)
    } else {
        ("risk-off", 0.0)
    }
}

pub struct RiskManager {
    pub market: SharedDb,
    pub acct: finbox_store::SharedAccountDb,
    pub config: RiskConfig,
    /// 进程内缓存的历史总资产峰值
    peak_cache: AtomicU64,
}

impl RiskManager {
    pub fn new(market: SharedDb, acct: finbox_store::SharedAccountDb, config: RiskConfig) -> Self {
        Self { market, acct, config, peak_cache: AtomicU64::new(0) }
    }

    /// 运行一轮风控评估。
    pub fn evaluate(&self) -> finbox_store::Result<RiskReport> {
        let mut report = RiskReport::default();

        // 1. 市场状态（涨跌家数，读 market）
        let (up, total) = self.market.lock().unwrap().market_breadth()?;
        let ratio = if total > 0 { up as f64 / total as f64 } else { 0.5 };
        let (regime, max_total) = regime_max_total(ratio);
        report.regime = regime.into();
        report.max_total_pct = max_total;

        // 2. 账户峰值与熔断（读 acct；总资产 = 现金 + 持仓市值[读 market 价格]）
        let total_asset = self.total_asset()?;
        let (peak, fuse_until_ms) = {
            let db = self.acct.lock().unwrap();
            let peak = self.current_peak_from(&db);
            let fuse = db.meta_get("fuse_until_ms")?.and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            (peak, fuse)
        };
        if total_asset > peak {
            self.update_peak(total_asset);
        }
        let peak = self.current_peak();

        let now_ms = Utc::now().timestamp_millis();
        if peak > 0.0 && (peak - total_asset) / peak >= self.config.fuse_drawdown_pct {
            if fuse_until_ms > now_ms {
                report.can_buy = false;
                let mins_left = (fuse_until_ms - now_ms) / 60000;
                report.note = format!("账户回撤 {:.1}% 熔断中，剩余约 {mins_left} 分钟", self.config.fuse_drawdown_pct * 100.0);
            } else {
                let until = now_ms + self.config.fuse_days as i64 * 86_400_000;
                self.acct.lock().unwrap().meta_set("fuse_until_ms", &until.to_string())?;
                report.can_buy = false;
                report.note = format!("触发熔断：回撤 {:.1}%，停止买入 {} 天", self.config.fuse_drawdown_pct * 100.0, self.config.fuse_days);
            }
        } else {
            report.can_buy = true;
        }

        // 3. 持仓风控（止损/止盈/超期）
        let positions = self.acct.lock().unwrap().positions()?;
        for p in &positions {
            if let Some(sell) = self.check_position(p)? {
                report.forced_sells.push(sell);
            }
        }
        Ok(report)
    }

    /// 总资产 = 现金 + 持仓市值（按最新行情价，读 market）。
    fn total_asset(&self) -> finbox_store::Result<f64> {
        let acct = self.acct.lock().unwrap();
        let account = acct.get_or_init_account(0.0)?;
        let positions = acct.positions()?;
        drop(acct); // 释放 acct 锁，再读 market
        let mut mv = 0.0;
        {
            let m = self.market.lock().unwrap();
            for p in &positions {
                let price = m.latest_snapshot_price(&p.thscode)?.unwrap_or(p.avg_cost);
                mv += price * p.quantity as f64;
            }
        }
        Ok(account.cash + mv)
    }

    /// 对单只持仓判断是否需要卖出。
    fn check_position(&self, p: &Position) -> finbox_store::Result<Option<OrderIntent>> {
        let cur = self
            .market
            .lock()
            .unwrap()
            .latest_snapshot_price(&p.thscode)?
            .unwrap_or(p.avg_cost);
        if cur <= 0.0 {
            return Ok(None);
        }
        let pnl = (cur - p.avg_cost) / p.avg_cost;

        // 止损：亏损 ≥ 阈值 → 全部卖出
        if pnl <= -self.config.stop_loss_pct {
            return Ok(Some(self.sell_intent(p, p.quantity)));
        }
        // 止盈：盈利 ≥ 阈值 → 减半仓（至少一手）
        if pnl >= self.config.take_profit_pct && p.quantity > 200 {
            let qty = (p.quantity / 2 / 100 * 100).max(100);
            return Ok(Some(self.sell_intent(p, qty)));
        }
        // 超期：持仓超天数且无起色（现价 < 成本）→ 全部卖出
        if pnl < 0.0 {
            let bought_ms = self.acct.lock().unwrap().position_bought_at(&p.thscode)?;
            if let Some(bought_ms) = bought_ms {
                let days = (Utc::now().timestamp_millis() - bought_ms) / 86_400_000;
                if days >= self.config.max_holding_days as i64 {
                    return Ok(Some(self.sell_intent(p, p.quantity)));
                }
            }
        }
        Ok(None)
    }

    fn sell_intent(&self, p: &Position, qty: u32) -> OrderIntent {
        OrderIntent {
            thscode: p.thscode.clone(),
            name: p.name.clone(),
            side: OrderSide::Sell,
            quantity: qty,
            decision_id: None,
        }
    }

    fn current_peak(&self) -> f64 {
        let v = self.peak_cache.load(AtomicOrdering::Relaxed);
        if v > 0 {
            return f64::from_bits(v);
        }
        let peak = self.acct.lock().unwrap().meta_get("peak_asset").ok().flatten()
            .and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
        self.peak_cache.store(peak.to_bits(), AtomicOrdering::Relaxed);
        peak
    }

    fn current_peak_from(&self, db: &finbox_store::AccountDb) -> f64 {
        let v = self.peak_cache.load(AtomicOrdering::Relaxed);
        if v > 0 {
            return f64::from_bits(v);
        }
        let peak = db.meta_get("peak_asset").ok().flatten()
            .and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
        self.peak_cache.store(peak.to_bits(), AtomicOrdering::Relaxed);
        peak
    }

    fn update_peak(&self, asset: f64) {
        self.peak_cache.store(asset.to_bits(), AtomicOrdering::Relaxed);
        let _ = self.acct.lock().unwrap().meta_set("peak_asset", &asset.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use finbox_store::{open_account_shared, open_market_shared, SnapshotRow};

    fn setup() -> (SharedDb, finbox_store::SharedAccountDb, RiskManager) {
        let market = open_market_shared(":memory:").unwrap();
        let acct = open_account_shared(":memory:").unwrap();
        let rm = RiskManager::new(market.clone(), acct.clone(), RiskConfig::default());
        (market, acct, rm)
    }

    #[test]
    fn regime_thresholds() {
        assert_eq!(regime_max_total(0.7), ("risk-on", 0.60));
        assert_eq!(regime_max_total(0.5), ("neutral", 0.40));
        assert_eq!(regime_max_total(0.2), ("risk-off", 0.0));
    }

    #[test]
    fn stop_loss_triggers_forced_sell() {
        let (market, acct, rm) = setup();
        {
            let a = acct.lock().unwrap();
            a.get_or_init_account(100000.0).unwrap();
            a.upsert_position(&Position {
                thscode: "600519.SH".into(),
                name: "贵州茅台".into(),
                quantity: 100,
                avg_cost: 10.0,
            })
            .unwrap();
        }
        {
            let m = market.lock().unwrap();
            m.insert_snapshots(
                1,
                &[SnapshotRow {
                    thscode: "600519.SH".into(),
                    last_price: 9.4,
                    price_change: -0.6,
                    price_change_ratio_pct: -6.0,
                    open_price: 9.9,
                    high_price: 10.0,
                    low_price: 9.3,
                    prev_price: 10.0,
                    volume: 1000.0,
                    turnover: 9400.0,
                }],
            )
            .unwrap();
        }
        let report = rm.evaluate().unwrap();
        assert_eq!(report.forced_sells.len(), 1, "亏损 6% 应触发止损卖出");
        assert_eq!(report.forced_sells[0].thscode, "600519.SH");
    }

    #[test]
    fn no_stop_loss_when_profit() {
        let (market, acct, rm) = setup();
        {
            let a = acct.lock().unwrap();
            a.get_or_init_account(100000.0).unwrap();
            a.upsert_position(&Position {
                thscode: "600519.SH".into(),
                name: "贵州茅台".into(),
                quantity: 100,
                avg_cost: 10.0,
            })
            .unwrap();
        }
        {
            let m = market.lock().unwrap();
            m.insert_snapshots(
                1,
                &[SnapshotRow {
                    thscode: "600519.SH".into(),
                    last_price: 10.5,
                    price_change: 0.5,
                    price_change_ratio_pct: 5.0,
                    open_price: 10.2,
                    high_price: 10.6,
                    low_price: 10.1,
                    prev_price: 10.0,
                    volume: 1000.0,
                    turnover: 10500.0,
                }],
            )
            .unwrap();
        }
        let report = rm.evaluate().unwrap();
        assert!(report.forced_sells.is_empty());
    }
}
