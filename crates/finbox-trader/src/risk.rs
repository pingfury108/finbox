//! 硬性风控层：独立于 LLM，不可绕过。
//!
//! - 单票止损：亏损 ≥ 5% 强制清仓
//! - 分批止盈：盈利 ≥ 15% 减半仓
//! - 持仓超期：超过 N 天且无起色（现价 < 成本）强制清仓
//! - 账户熔断：总资产回撤 ≥ 5% 时停止买入（熔断期）
//!
//! 该层是「控制回撤 ≤ 5%」的第一道防线，所有卖出意图直接交给 Broker 执行。

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
    pub db: SharedDb,
    pub config: RiskConfig,
    /// 进程内缓存的历史总资产峰值（毫秒 → 值），避免每次查库
    peak_cache: AtomicU64,
}

impl RiskManager {
    pub fn new(db: SharedDb, config: RiskConfig) -> Self {
        Self { db, config, peak_cache: AtomicU64::new(0) }
    }

    /// 运行一轮风控评估：检查持仓触发止损/止盈/超期，更新熔断状态。
    pub fn evaluate(&self) -> finbox_store::Result<RiskReport> {
        let mut report = RiskReport::default();
        let db = self.db.lock().unwrap();

        // 1. 市场状态（涨跌家数）
        let (up, total) = db.market_breadth()?;
        let ratio = if total > 0 { up as f64 / total as f64 } else { 0.5 };
        let (regime, max_total) = regime_max_total(ratio);
        report.regime = regime.into();
        report.max_total_pct = max_total;

        // 2. 账户峰值与熔断检查（peak 读写直接操作 db，不再重复加锁）
        let acct = db.get_or_init_account(0.0)?;
        let total_asset = db.total_asset(&acct)?;
        let peak = self.current_peak_from(&db);
        if total_asset > peak {
            self.update_peak_from(&db, total_asset);
        }
        let peak = self.current_peak_from(&db);
        if peak > 0.0 && (peak - total_asset) / peak >= self.config.fuse_drawdown_pct {
            let fuse_until_ms = db
                .meta_get("fuse_until_ms")?
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            let now_ms = Utc::now().timestamp_millis();
            if fuse_until_ms > now_ms {
                report.can_buy = false;
                let mins_left = (fuse_until_ms - now_ms) / 60000;
                report.note = format!("账户回撤 {:.0}% 熔断中，剩余约 {mins_left} 分钟", self.config.fuse_drawdown_pct * 100.0);
            } else {
                // 触发熔断：设置熔断期
                let until = now_ms + self.config.fuse_days as i64 * 86_400_000;
                db.meta_set("fuse_until_ms", &until.to_string())?;
                report.can_buy = false;
                report.note = format!("触发熔断：回撤 {:.1}%，停止买入 {} 天", self.config.fuse_drawdown_pct * 100.0, self.config.fuse_days);
            }
        } else {
            report.can_buy = true;
        }

        // 3. 持仓风控（止损/止盈/超期）
        let positions = db.positions()?;
        for p in &positions {
            if let Some(sell) = self.check_position(&db, p)? {
                report.forced_sells.push(sell);
            }
        }
        Ok(report)
    }

    /// 对单只持仓判断是否需要卖出。
    fn check_position(&self, db: &finbox_store::Db, p: &Position) -> finbox_store::Result<Option<OrderIntent>> {
        let cur = db.latest_snapshot_price(&p.thscode)?.unwrap_or(p.avg_cost);
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
            if let Some(bought_ms) = db.position_bought_at(&p.thscode)? {
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

    fn current_peak_from(&self, db: &finbox_store::Db) -> f64 {
        let v = self.peak_cache.load(AtomicOrdering::Relaxed);
        if v > 0 {
            return f64::from_bits(v);
        }
        // 从 meta 读取历史峰值
        let peak = db
            .meta_get("peak_asset")
            .ok()
            .flatten()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        self.peak_cache.store(peak.to_bits(), AtomicOrdering::Relaxed);
        peak
    }

    fn update_peak_from(&self, db: &finbox_store::Db, asset: f64) {
        self.peak_cache.store(asset.to_bits(), AtomicOrdering::Relaxed);
        let _ = db.meta_set("peak_asset", &asset.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use finbox_store::{open_shared, SnapshotRow};

    // 简化：直接在 shared 上构造测试数据
    fn setup() -> (SharedDb, RiskManager) {
        let shared = open_shared(":memory:").unwrap();
        let rm = RiskManager::new(shared.clone(), RiskConfig::default());
        (shared, rm)
    }

    #[test]
    fn regime_thresholds() {
        assert_eq!(regime_max_total(0.7), ("risk-on", 0.60));
        assert_eq!(regime_max_total(0.5), ("neutral", 0.40));
        assert_eq!(regime_max_total(0.2), ("risk-off", 0.0));
    }

    #[test]
    fn stop_loss_triggers_forced_sell() {
        let (shared, rm) = setup();
        {
            let db = shared.lock().unwrap();
            // 账户 + 持仓（成本 10 元）+ 最新价 9.4（-6%）
            db.get_or_init_account(100000.0).unwrap();
            db.upsert_position(&Position {
                thscode: "600519.SH".into(),
                name: "贵州茅台".into(),
                quantity: 100,
                avg_cost: 10.0,
            })
            .unwrap();
            db.insert_snapshots(
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
            db.insert_trade(&finbox_core::Trade {
                thscode: "600519.SH".into(),
                name: "贵州茅台".into(),
                side: finbox_core::OrderSide::Buy,
                price: 10.0,
                quantity: 100,
                amount: 1000.0,
                fee: 5.0,
                decision_id: None,
            })
            .unwrap();
        }
        let report = rm.evaluate().unwrap();
        assert_eq!(report.forced_sells.len(), 1, "亏损 6% 应触发止损卖出");
        assert_eq!(report.forced_sells[0].thscode, "600519.SH");
        assert_eq!(report.forced_sells[0].side, finbox_core::OrderSide::Sell);
    }

    #[test]
    fn no_stop_loss_when_profit() {
        let (shared, rm) = setup();
        {
            let db = shared.lock().unwrap();
            db.get_or_init_account(100000.0).unwrap();
            db.upsert_position(&Position {
                thscode: "600519.SH".into(),
                name: "贵州茅台".into(),
                quantity: 100,
                avg_cost: 10.0,
            })
            .unwrap();
            // 现价 10.5（+5%）不触发止损
            db.insert_snapshots(
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
