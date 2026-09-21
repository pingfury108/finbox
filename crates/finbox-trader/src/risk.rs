//! 硬性风控层：独立于 LLM，不可绕过。
//!
//! - 单票止损：**收盘价**亏损 ≥ 5% 强制清仓（盘中插针仅预警，不卖）
//! - 分批止盈：盈利 ≥ 6% 减 1/3，≥ 10% 再减 1/3（落袋为安）
//! - 持仓超期：超过 N 天且无起色（现价 < 成本）强制清仓
//! - 账户熔断：总资产回撤 ≥ 5% 时停止买入（熔断期）
//! - 市场门控：风险偏好决定目标仓位上限，risk-off 时**真减仓**到目标（不是只冻结买入）
//!
//! 双库架构：行情（价格/涨跌家数）读 market 库，账户（持仓/熔断状态）读写 account 库。
//! 铁律：**不同时持有两把锁** —— 先读持仓（acct）解锁，再读价格（market）解锁，最后写状态（acct）。

use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering as AtomicOrdering};

use chrono::Utc;
use finbox_core::{OrderIntent, OrderSide, Position};
use finbox_store::SharedDb;

/// 风控参数。
#[derive(Debug, Clone)]
pub struct RiskConfig {
    /// 单票止损阈值（亏损比例，**收盘价**判定）
    pub stop_loss_pct: f64,
    /// 尾部硬止损：盘中实时价跌破此比例无条件清仓（补收盘价判定的跳空敲口）
    pub hard_stop_loss_pct: f64,
    /// 止盈第一档（减 1/3）
    pub take_profit_pct: f64,
    /// 止盈第二档（再减 1/3）
    pub take_profit_pct2: f64,
    /// 每次止盈减仓比例
    pub trim_ratio: f64,
    /// 持仓超期天数（超过且无起色则清仓）
    pub max_holding_days: u32,
    /// 账户熔断回撤阈值
    pub fuse_drawdown_pct: f64,
    /// 熔断持续天数
    pub fuse_days: u32,
    /// 熔断时降到目标仓位（不只停买，避免满仓挨跌）
    pub fuse_target_position: f64,
    /// 账户收益目标（达到后降仓锁利）
    pub profit_target_pct: f64,
    /// 达标后的仓位上限
    pub profit_target_position: f64,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            stop_loss_pct: 0.05,
            hard_stop_loss_pct: 0.08,
            take_profit_pct: 0.06,
            take_profit_pct2: 0.10,
            trim_ratio: 1.0 / 3.0,
            max_holding_days: 20,
            fuse_drawdown_pct: 0.05,
            fuse_days: 5,
            fuse_target_position: 0.30,
            profit_target_pct: 0.05,
            profit_target_position: 0.40,
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
    /// 是否已达到账户收益目标（降仓锁利中）
    pub profit_reached: bool,
    /// 备注（熔断状态等）
    pub note: String,
}

/// 市场状态 → 目标总仓位上限。
///
/// 调优：risk-on 60%→75%（40% 制度性闲置太重，目标 +5% 需组合涨 8.3%）；
/// neutral 40%→55%；risk-off 0%→30%（降仓而非清仓，避免与 re-buy 反复摩擦）。
fn regime_max_total(breadth_ratio: f64) -> (&'static str, f64) {
    if breadth_ratio >= 0.6 {
        ("risk-on", 0.75)
    } else if breadth_ratio >= 0.4 {
        ("neutral", 0.55)
    } else {
        ("risk-off", 0.30)
    }
}

pub struct RiskManager {
    pub market: SharedDb,
    pub acct: finbox_store::SharedAccountDb,
    /// 风控参数（Mutex 包裹：Web 参数页改后可热生效）
    config: std::sync::Mutex<RiskConfig>,
    /// 进程内缓存的历史总资产峰值
    peak_cache: AtomicU64,
    /// 连续“需减仓”（risk-off / 熔断）的评估次数：过滤 1 分钟级 breadth 抖动
    trim_streak: AtomicU32,
}

/// 减仓前需要的连续确认次数（盘中风控每分钟评一次，3 次 ≈ 3 分钟）。
const TRIM_CONFIRM_EVALS: u32 = 3;

impl RiskManager {
    pub fn new(market: SharedDb, acct: finbox_store::SharedAccountDb, config: RiskConfig) -> Self {
        Self {
            market,
            acct,
            config: std::sync::Mutex::new(config),
            peak_cache: AtomicU64::new(0),
            trim_streak: AtomicU32::new(0),
        }
    }

    /// 更新风控参数（每轮决策前由调度器从账户库 meta 刷新）。
    pub fn set_config(&self, cfg: RiskConfig) {
        *self.config.lock().unwrap() = cfg;
    }

    /// 当前参数快照。
    pub fn config(&self) -> RiskConfig {
        self.config.lock().unwrap().clone()
    }

    /// 运行一轮风控评估。
    pub fn evaluate(&self) -> finbox_store::Result<RiskReport> {
        let mut report = RiskReport::default();
        let cfg = self.config();

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
        let drawdown = if peak > 0.0 { (peak - total_asset) / peak } else { 0.0 };
        // 熔断（账户级回撤）：只停买，不再直接减仓（减仓统一走下方“确认后减仓”）
        if peak > 0.0 && drawdown >= cfg.fuse_drawdown_pct {
            if fuse_until_ms > now_ms {
                report.can_buy = false;
                let mins_left = (fuse_until_ms - now_ms) / 60000;
                report.note = format!("账户回撤 {:.1}% 熔断中，剩余约 {mins_left} 分钟", cfg.fuse_drawdown_pct * 100.0);
            } else {
                let until = now_ms + cfg.fuse_days as i64 * 86_400_000;
                self.acct.lock().unwrap().meta_set("fuse_until_ms", &until.to_string())?;
                report.can_buy = false;
                report.note = format!("触发熔断：回撤 {:.1}%，停止买入 {} 天", cfg.fuse_drawdown_pct * 100.0, cfg.fuse_days);
            }
        } else {
            report.can_buy = true;
        }

        // 2.5 账户收益目标：达到即降仓锁利（目标不是口号，要有动作）
        let initial = self.acct.lock().unwrap().get_or_init_account(0.0)?.initial_capital;
        if initial > 0.0 {
            let ret = total_asset / initial - 1.0;
            if ret >= cfg.profit_target_pct {
                report.profit_reached = true;
                report.max_total_pct = report.max_total_pct.min(cfg.profit_target_position);
                report.note = format!(
                    "{}已达收益目标 {:.1}%（当前 {:+.1}%），仓位上限降至 {:.0}% 锁利",
                    if report.note.is_empty() { "" } else { "；" },
                    cfg.profit_target_pct * 100.0, ret * 100.0,
                    cfg.profit_target_position * 100.0
                );
            }
        }

        // 3. 持仓风控（止损/止盈/超期）
        let positions = self.acct.lock().unwrap().positions()?;
        for p in &positions {
            if let Some(sell) = self.check_position(p)? {
                report.forced_sells.push(sell);
            }
        }

        // 4. 减仓降仓：**只在 risk-off（或熔断）且连续确认后才动手**。
        //    教训（2026-09-21）：原来“任何时点仓位>目标就减仓”，导致开盘头几分钟
        //    涨跌家数抖动（risk-on→neutral→risk-on）被当成真实信号，
        //    买入 60 秒后就被市价砍仓。neutral 恢复为“只限制买入，不动手”。
        let need_trim = report.regime == "risk-off" || !report.can_buy || report.profit_reached;
        let streak = if need_trim {
            self.trim_streak.fetch_add(1, AtomicOrdering::Relaxed) + 1
        } else {
            self.trim_streak.store(0, AtomicOrdering::Relaxed);
            0
        };
        if need_trim {
            if report.regime == "risk-off" {
                report.max_total_pct = report.max_total_pct.min(cfg.fuse_target_position);
            }
            if streak < TRIM_CONFIRM_EVALS {
                report.note = format!(
                    "{}{}待确认减仓（{streak}/{TRIM_CONFIRM_EVALS}），本轮不动手",
                    report.note,
                    if report.note.is_empty() { "" } else { "；" }
                );
            } else {
                let trims = self.trim_to_target(&positions, &report.forced_sells, total_asset, report.max_total_pct)?;
                if !trims.is_empty() {
                    report.note = format!(
                        "{}{}减仓至 {:.0}%（{} 笔）",
                        report.note,
                        if report.note.is_empty() { "" } else { "；" },
                        report.max_total_pct * 100.0,
                        trims.len()
                    );
                    report.forced_sells.extend(trims);
                }
            }
        }
        Ok(report)
    }

    /// 总仓位超过目标上限时，按**浮亏最大优先**减仓（先砍风险敎口）。
    ///
    /// - 当日买入的仓位跳过（T+1 卖不掉，不生成废单）
    /// - 按整手向上取整（不再 floor 截断留 100 股零碎仓）；卖剩不足一手则一把清
    fn trim_to_target(
        &self,
        positions: &[Position],
        existing: &[OrderIntent],
        total_asset: f64,
        target_pct: f64,
    ) -> finbox_store::Result<Vec<OrderIntent>> {
        if total_asset <= 0.0 {
            return Ok(vec![]);
        }
        let day_start = chrono::Local::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_local_timezone(chrono::Local)
            .unwrap()
            .timestamp_millis();
        let done: HashSet<&str> = existing.iter().map(|i| i.thscode.as_str()).collect();
        // (代码, 名称, 数量, 市值, 盈亏比例)
        let mut rows: Vec<(String, String, u32, f64, f64)> = Vec::new();
        let mut mv_total = 0.0;
        for p in positions {
            if done.contains(p.thscode.as_str()) {
                continue;
            }
            // 当日买入（T+1 不可卖）→ 不计入可减仓市值
            let bought_at = self.acct.lock().unwrap().position_bought_at(&p.thscode)?;
            if bought_at.map(|t| t >= day_start).unwrap_or(false) {
                continue;
            }
            let price = self
                .market
                .lock()
                .unwrap()
                .latest_snapshot_price(&p.thscode)?
                .unwrap_or(p.avg_cost);
            let mv = price * p.quantity as f64;
            mv_total += mv;
            let pnl = if p.avg_cost > 0.0 { price / p.avg_cost - 1.0 } else { 0.0 };
            rows.push((p.thscode.clone(), p.name.clone(), p.quantity, mv, pnl));
        }
        // 仓位比例用全量市值（含当日买入），否则会误判为“已达标”
        let cur_pct = self.position_pct(total_asset)?;
        if cur_pct <= target_pct {
            return Ok(vec![]);
        }
        let need = (cur_pct - target_pct) * total_asset;
        rows.sort_by(|a, b| a.4.partial_cmp(&b.4).unwrap_or(std::cmp::Ordering::Equal));
        let mut out = Vec::new();
        let mut sold = 0.0;
        for (code, name, qty, mv, pnl) in rows {
            if sold >= need {
                break;
            }
            let unit = if qty > 0 { mv / qty as f64 } else { 0.0 };
            if unit <= 0.0 {
                continue;
            }
            let want = (need - sold).min(mv);
            // 整手**向上**取整：原来向下取整会留 100 股零碎仓（持仓 4 手砍成 3.9 手）
            let mut q = ((want / unit) / 100.0).ceil() as u32 * 100;
            if q >= qty {
                q = qty; // 整只清掉
            } else if qty - q < 100 {
                q = qty; // 卖剩不足一手，一并清掉（避免零碎仓）
            }
            if q == 0 {
                continue;
            }
            log::info!(
                "[风控] 超仓减仓 {} {}股（浮{:.1}%，仓位 {:.1}% → 目标 {:.0}%）",
                code, q, pnl * 100.0, cur_pct * 100.0, target_pct * 100.0
            );
            out.push(OrderIntent {
                thscode: code,
                name,
                side: OrderSide::Sell,
                quantity: q,
                decision_id: None,
            });
            sold += unit * q as f64;
        }
        Ok(out)
    }

    /// 当前总仓位比例（现金 + 持仓市值口径，与 scheduler 的估值一致）。
    fn position_pct(&self, total_asset: f64) -> finbox_store::Result<f64> {
        if total_asset <= 0.0 {
            return Ok(0.0);
        }
        let positions = self.acct.lock().unwrap().positions()?;
        let mut mv = 0.0;
        {
            let m = self.market.lock().unwrap();
            for p in &positions {
                let price = m.latest_snapshot_price(&p.thscode)?.unwrap_or(p.avg_cost);
                mv += price * p.quantity as f64;
            }
        }
        Ok(mv / total_asset)
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
    ///
    /// 止损/超期用**收盘价**（避开日内插针洗盘），止盈用实时价（落袋为安）。
    fn check_position(&self, p: &Position) -> finbox_store::Result<Option<OrderIntent>> {
        let cfg = self.config();
        let (rt, close) = {
            let m = self.market.lock().unwrap();
            let rt = m.latest_snapshot_price(&p.thscode)?;
            let close = m.latest_raw_close(&p.thscode)?;
            (rt, close)
        };
        let rt = rt.filter(|v| *v > 0.0).unwrap_or(p.avg_cost);
        // 收盘价缺失（新股/停牌）时用实时价兜底
        let close = close.filter(|v| *v > 0.0).unwrap_or(rt);
        if p.avg_cost <= 0.0 {
            return Ok(None);
        }
        let pnl_close = (close - p.avg_cost) / p.avg_cost;
        let pnl_rt = (rt - p.avg_cost) / p.avg_cost;

        // 尾部硬止损（实时价）：补收盘价判定的跳空敲口——盘中崩盘不等到收盘
        if pnl_rt <= -cfg.hard_stop_loss_pct {
            log::warn!(
                "[风控] {} 盘中暴跌 {:.1}%（硬止损线 -{:.0}%）→ 无条件清仓",
                p.thscode, pnl_rt * 100.0, cfg.hard_stop_loss_pct * 100.0
            );
            return Ok(Some(self.sell_intent(p, p.quantity)));
        }

        // 止损（收盘价判定）：盘中插针不算——否则 A 股日内振幅 5% 的常态会把仓位洗在最低点
        if pnl_close <= -cfg.stop_loss_pct {
            log::info!(
                "[风控] {} 收盘价 {:.2} 跌破止损线（{:.1}%）→ 清仓",
                p.thscode, close, pnl_close * 100.0
            );
            return Ok(Some(self.sell_intent(p, p.quantity)));
        }
        if pnl_rt <= -cfg.stop_loss_pct {
            log::warn!(
                "[风控] {} 盘中触及止损线（实时 {:.1}%），但收盘价 {:.2}（{:.1}%）未破线，不卖",
                p.thscode, pnl_rt * 100.0, close, pnl_close * 100.0
            );
        }

        // 分批止盈：+6% 减 1/3，+10% 再减 1/3（仓位太小则一次清）
        let trim_qty = |q: u32| -> u32 {
            if q < 300 {
                0
            } else {
                ((q as f64 * cfg.trim_ratio) as u32 / 100) * 100
            }
        };
        if pnl_rt >= cfg.take_profit_pct2 && !self.tp_done(p, 2)? {
            let q = trim_qty(p.quantity);
            if q < 100 {
                log::info!("[风控] {} 达二档止盈 {:.1}%，仓位过小→清仓", p.thscode, pnl_rt * 100.0);
                return Ok(Some(self.sell_intent(p, p.quantity)));
            }
            self.mark_tp(p, 2)?;
            log::info!("[风控] {} 达二档止盈 +{:.1}%（阈值 +{:.0}%）→ 减 {} 股", p.thscode, pnl_rt * 100.0, cfg.take_profit_pct2 * 100.0, q);
            return Ok(Some(self.sell_intent(p, q)));
        }
        if pnl_rt >= cfg.take_profit_pct && !self.tp_done(p, 1)? {
            let q = trim_qty(p.quantity);
            if q < 100 {
                log::info!("[风控] {} 达一档止盈 {:.1}%，仓位过小→清仓", p.thscode, pnl_rt * 100.0);
                return Ok(Some(self.sell_intent(p, p.quantity)));
            }
            self.mark_tp(p, 1)?;
            log::info!("[风控] {} 达一档止盈 +{:.1}%（阈值 +{:.0}%）→ 减 {} 股", p.thscode, pnl_rt * 100.0, cfg.take_profit_pct * 100.0, q);
            return Ok(Some(self.sell_intent(p, q)));
        }

        // 超期：持仓超天数且无起色（收盘价低于成本）→ 全部卖出
        if pnl_close < 0.0 {
            let bought_ms = self.acct.lock().unwrap().position_bought_at(&p.thscode)?;
            if let Some(bought_ms) = bought_ms {
                let days = (Utc::now().timestamp_millis() - bought_ms) / 86_400_000;
                if days >= cfg.max_holding_days as i64 {
                    log::info!("[风控] {} 持仓 {} 天未起色 → 清仓", p.thscode, days);
                    return Ok(Some(self.sell_intent(p, p.quantity)));
                }
            }
        }
        Ok(None)
    }

    /// 该档止盈是否已执行（防同一档反复减仓）。
    fn tp_done(&self, p: &Position, level: u8) -> finbox_store::Result<bool> {
        Ok(self
            .acct
            .lock()
            .unwrap()
            .meta_get(&format!("tp{level}:{}", p.thscode))?
            .is_some())
    }

    fn mark_tp(&self, p: &Position, level: u8) -> finbox_store::Result<()> {
        self.acct
            .lock()
            .unwrap()
            .meta_set(&format!("tp{level}:{}", p.thscode), &Utc::now().timestamp_millis().to_string())
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
        assert_eq!(regime_max_total(0.7), ("risk-on", 0.75));
        assert_eq!(regime_max_total(0.5), ("neutral", 0.55));
        assert_eq!(regime_max_total(0.2), ("risk-off", 0.30));
    }

    fn put_position(acct: &finbox_store::SharedAccountDb, cash: f64, qty: u32, cost: f64) {
        // 初始资金给得足够大（使“收益目标”默认不触发，专注验证被测逻辑）
        put_position_with(acct, cash, qty, cost, 10_000_000.0);
    }

    fn put_position_with(
        acct: &finbox_store::SharedAccountDb,
        cash: f64,
        qty: u32,
        cost: f64,
        initial: f64,
    ) {
        let a = acct.lock().unwrap();
        a.get_or_init_account(initial).unwrap();
        a.set_account_cash(cash).unwrap();
        a.upsert_position(&Position {
            thscode: "600519.SH".into(),
            name: "贵州茅台".into(),
            quantity: qty,
            avg_cost: cost,
        })
        .unwrap();
    }

    fn put_snapshot(market: &SharedDb, price: f64, pct: f64) {
        market
            .lock()
            .unwrap()
            .insert_snapshots(
                1,
                &[SnapshotRow {
                    thscode: "600519.SH".into(),
                    last_price: price,
                    price_change: price - 10.0,
                    price_change_ratio_pct: pct,
                    open_price: 10.0,
                    high_price: price.max(10.0),
                    low_price: price.min(10.0),
                    prev_price: 10.0,
                    volume: 1000.0,
                    turnover: 0.0,
                }],
            )
            .unwrap();
    }

    #[test]
    fn stop_loss_ignores_intraday_wick() {
        // 盘中一度 -6%（插针），但收盘价 10.0 未破线 → 不卖（避开日内洗盘）
        let (market, acct, rm) = setup();
        put_position(&acct, 100000.0, 100, 10.0);
        {
            let m = market.lock().unwrap();
            m.insert_daily_bars(&[finbox_store::DailyBarRow {
                thscode: "600519.SH".into(),
                date_ms: 1,
                date: "2026-09-10".into(),
                open: 10.0,
                high: 10.2,
                low: 9.3,
                close: 10.0,
                volume: 0.0,
                turnover: 0.0,
            }])
            .unwrap();
        }
        put_snapshot(&market, 9.4, -6.0);
        let report = rm.evaluate().unwrap();
        assert!(report.forced_sells.is_empty(), "盘中插针不应触发止损");
    }

    #[test]
    fn take_profit_trims_third_once_per_level() {
        // +7% → 减 1/3；同一档不重复减
        let (market, acct, rm) = setup();
        put_position(&acct, 100000.0, 900, 10.0);
        put_snapshot(&market, 10.7, 7.0);
        let report = rm.evaluate().unwrap();
        assert_eq!(report.forced_sells.len(), 1, "+7% 应触发第一档止盈");
        assert_eq!(report.forced_sells[0].quantity, 300, "减 1/3");
        let report2 = rm.evaluate().unwrap();
        assert!(report2.forced_sells.is_empty(), "同一档不应重复减仓");
    }

    #[test]
    fn risk_off_trims_to_target_after_confirm() {
        // 涨跌家数 0% → risk-off（目标 30%），持仓占比 60% → 连续确认 3 次后才减仓
        let (market, acct, rm) = setup();
        put_position(&acct, 400000.0, 60000, 10.0);
        put_snapshot(&market, 10.0, -0.5); // 该股微跌，使涨跌家数为 0/1 → risk-off
        // 第一次：确认中，不动手（当日买入也豁免）
        let r1 = rm.evaluate().unwrap();
        assert_eq!(r1.regime, "risk-off");
        assert!(r1.forced_sells.is_empty(), "首次 risk-off 不应立即减仓");
        // 第 2、3 次后确认通过
        let _ = rm.evaluate().unwrap();
        let r3 = rm.evaluate().unwrap();
        assert_eq!(r3.forced_sells.len(), 1, "确认后应减仓");
        // 持仓 60 万 / 总资产 100 万 = 60%；目标 30% → 卖 30 万 = 30000 股
        assert_eq!(r3.forced_sells[0].quantity, 30000);
    }

    #[test]
    fn neutral_does_not_trim() {
        // 涨跌家数 50% → neutral：只限制买入，不动手（2026-09-21 事故回测）
        let (market, acct, rm) = setup();
        put_position(&acct, 400000.0, 60000, 10.0);
        put_snapshot(&market, 10.5, 1.0); // 1/2 上涨 → ratio 0.5 → neutral
        {
            // 再插一只下跌股，使 ratio = 50%
            market
                .lock()
                .unwrap()
                .insert_snapshots(
                    1,
                    &[SnapshotRow {
                        thscode: "600000.SH".into(),
                        last_price: 9.5,
                        price_change: -0.5,
                        price_change_ratio_pct: -5.0,
                        open_price: 10.0,
                        high_price: 10.0,
                        low_price: 9.5,
                        prev_price: 10.0,
                        volume: 1000.0,
                        turnover: 0.0,
                    }],
                )
                .unwrap();
        }
        let mut trimmed = false;
        for _ in 0..5 {
            let r = rm.evaluate().unwrap();
            assert_eq!(r.regime, "neutral");
            if !r.forced_sells.is_empty() {
                trimmed = true;
            }
        }
        assert!(!trimmed, "neutral 不应触发任何减仓");
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
