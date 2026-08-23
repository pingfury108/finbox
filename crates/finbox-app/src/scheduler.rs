//! 调度器：日线策略节奏。
//!
//! - 盘前（交易日 9:00）：补齐日K/复权，刷新交易日历
//! - 盘中（每 60s）：采集快照 + 只跑风控（止损/止盈/超期监控）
//! - 收盘（交易日 15:05）：① 风控评估并执行强制卖出 ② 每日 AI 决策（候选→LLM→买入）
//!   ③ 收盘账户快照
//!
//! 决策频率从"每30分钟"降为"每天一次"：目标 5% 的稳健策略，交易越少，手续费侵蚀越少。

use std::time::Duration;

use chrono::{Datelike, Local, Timelike};
use finbox_collector::Collector;
use finbox_decision::{DecisionEngine, LlmConfig};
use finbox_store::{open_shared, SharedDb};
use finbox_trader::{Broker, RiskConfig, RiskManager, SimBroker};
use hithink_sdk::Client;

use crate::config::Config;

pub struct Scheduler {
    cfg: Config,
    db: SharedDb,
    collector: Collector,
    decision: DecisionEngine,
    broker: SimBroker,
    risk: RiskManager,
    /// 上次采集时刻（分钟）
    last_collect: i64,
    /// 今日是否已收盘处理
    closed_today: bool,
    /// 今日是否已盘前同步
    pre_open_done: bool,
}

impl Scheduler {
    pub fn new(cfg: Config) -> anyhow::Result<Self> {
        let db = open_shared(&cfg.db_path)?;
        let client = Client::new(cfg.hithink_api_key.clone())?;
        let collector = Collector::new(client, db.clone());
        let decision = DecisionEngine::new(
            db.clone(),
            LlmConfig {
                base_url: cfg.llm_base_url.clone(),
                api_key: cfg.llm_api_key.clone(),
                model: cfg.llm_model.clone(),
            },
            cfg.watchlist.clone(),
        );
        let broker = SimBroker::new(db.clone(), cfg.initial_capital);
        let risk = RiskManager::new(db.clone(), RiskConfig::default());
        Ok(Self {
            cfg,
            db,
            collector,
            decision,
            broker,
            risk,
            last_collect: 0,
            closed_today: false,
            pre_open_done: false,
        })
    }

    /// 主循环：每 30 秒 tick。
    pub async fn run(&mut self) -> anyhow::Result<()> {
        loop {
            let now = Local::now();
            let today = now.format("%Y%m%d").to_string();
            let weekday_iso = now.weekday().num_days_from_monday() + 1;
            let minute = now.hour() * 60 + now.minute();
            let trading_day = self.db.lock().unwrap().is_trading_day(&today)?;

            if trading_day && weekday_iso < 6 {
                // 盘前同步（9:00-9:15）
                if minute >= 9 * 60 && minute < 9 * 60 + 15 && !self.pre_open_done {
                    self.pre_open_sync().await?;
                    self.pre_open_done = true;
                }
                // 盘中：采集 + 风控监控（止损等实时触发）
                if minute >= 9 * 60 + 30 && minute < 15 * 60 {
                    self.intraday(now).await?;
                }
                // 收盘：风控 + 决策 + 快照（15:05 后）
                if minute >= 15 * 60 + 5 && !self.closed_today {
                    self.close_process().await?;
                    self.closed_today = true;
                }
                // 跨日重置标记
                if minute < 9 * 60 {
                    self.pre_open_done = false;
                    self.closed_today = false;
                }
            }

            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    }

    /// 盘前同步：交易日历 + 日K增量 + 复权事件。
    async fn pre_open_sync(&self) -> anyhow::Result<()> {
        log::info!("[盘前] 同步交易日历/日K/复权");
        let days = self.collector.client.trading_days().await?;
        self.collector.upsert_trading_days(&days).await?;
        self.collector.sync_daily_bars(std::path::Path::new("data/dumps"), &days).await?;
        self.collector.import_adjustment_factors(std::path::Path::new("data/dumps")).await?;
        Ok(())
    }

    /// 盘中：按间隔采集快照 + 风控实时监控。
    async fn intraday(&mut self, now: chrono::DateTime<Local>) -> anyhow::Result<()> {
        let min = now.timestamp() / 60;
        // 采集（默认每 60s）
        if min - self.last_collect >= self.cfg.collect_interval_seconds as i64 / 60 {
            let n = self.collector.collect_market_snapshot().await?;
            log::info!("[盘中] 采集快照 {n} 只");
            self.last_collect = min;
        }
        // 风控监控：止损/止盈/超期，触发生成卖出并执行
        self.run_risk_sells().await?;
        Ok(())
    }

    /// 收盘流程：① 风控强制卖出 ② 每日决策买入 ③ 账户快照。
    async fn close_process(&self) -> anyhow::Result<()> {
        log::info!("[收盘] 开始每日结算");
        // ① 风控评估 + 执行强制卖出（用收盘价）
        let report = self.risk.evaluate()?;
        self.execute_sells(&report.forced_sells).await?;
        log::info!(
            "[风控] 市场{} 目标总仓位{} 可买入={} 强制卖出{}笔 {}",
            report.regime,
            report.max_total_pct,
            report.can_buy,
            report.forced_sells.len(),
            report.note
        );

        // ② 若可买入（未熔断 + 市场允许），每日决策
        if report.can_buy && report.max_total_pct > 0.0 {
            self.daily_decision(&report).await?;
        } else if !report.can_buy {
            log::info!("[决策] 熔断/回撤中，今日不买入");
        } else {
            log::info!("[决策] 市场 {}，只减不买", report.regime);
        }

        // ③ 复盘：验证 1/5/10 天前的决策
        self.review_old_decisions().await?;

        // ④ 收盘账户快照
        self.snapshot_account().await?;
        Ok(())
    }

    /// 复盘：对 1/5/10 天前产生过交易的决策，用最新价验证对错并写入 review 表。
    async fn review_old_decisions(&self) -> anyhow::Result<()> {
        let db = self.db.lock().unwrap();
        let decisions = db.recent_decision_logs(50)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        for d in decisions {
            for days_after in [1u32, 5, 10] {
                // 已复盘过则跳过
                if db.review_exists(d.id, days_after)? {
                    continue;
                }
                let age_days = (now_ms - d.ts_ms) / 86_400_000;
                if age_days < days_after as i64 {
                    continue; // 还没到复盘日
                }
                // 该决策的成交
                let trades = db.trades_for_decision(d.id)?;
                if trades.is_empty() {
                    continue;
                }
                let mut lines = Vec::new();
                let mut total_pnl = 0.0;
                for t in &trades {
                    let cur = db
                        .latest_snapshot_price(&t.thscode)?
                        .or(db.prev_close(&t.thscode)?)
                        .unwrap_or(t.price);
                    let diff = (cur - t.price) * t.quantity as f64;
                    let diff = if t.side == finbox_core::OrderSide::Sell { -diff } else { diff };
                    total_pnl += diff;
                    let verdict = if (t.side == finbox_core::OrderSide::Buy) == (diff >= 0.0) {
                        "对"
                    } else {
                        "错"
                    };
                    lines.push(format!(
                        "{} {} @ {:.2} → 现价 {:.2} 浮动 {diff:+.0}元 判断【{verdict}】",
                        t.side.as_str(), t.thscode, t.price, cur
                    ));
                }
                db.insert_review(d.id, days_after, &lines.join("; "), total_pnl)?;
                log::info!("[复盘] 决策#{} {}天后 总盈亏 {:+.0} 元", d.id, days_after, total_pnl);
            }
        }
        Ok(())
    }

    /// 每日一次 AI 决策：候选 → LLM → 按风控上限执行买入。
    async fn daily_decision(&self, report: &finbox_trader::RiskReport) -> anyhow::Result<()> {
        let result = self.decision.decide(self.cfg.candidate_count).await?;
        log::info!("[决策] 状态 {} 意图 {} 条", result.status, result.intents.len());
        for intent in &result.intents {
            if intent.side != finbox_core::OrderSide::Buy {
                continue; // 买入由收盘决策处理，卖出由风控处理
            }
            // 数量由风控约束：单票 ≤20% 总资产、总仓位 ≤ max_total_pct、持仓 ≤3
            let qty = self.position_size(intent, report.max_total_pct).await;
            if qty < 100 {
                log::info!("[决策] {} 仓位计算后不足一手，跳过", intent.thscode);
                continue;
            }
            let mut i = intent.clone();
            i.quantity = qty;
            match self.broker.submit(i.clone()).await {
                Ok(e) => log::info!("[成交] {} {} {}股 @ {:.2} 费 {:.2}", e.intent.side.as_str(), e.intent.thscode, e.intent.quantity, e.price, e.fee),
                Err(e) => log::info!("[拒单] {}: {e}", i.thscode),
            }
        }
        Ok(())
    }

    /// 计算买入数量：单票 ≤20% 总资产，且总市值 ≤ max_total_pct × 总资产，持仓 ≤3。
    async fn position_size(&self, intent: &finbox_core::OrderIntent, max_total_pct: f64) -> u32 {
        let db = self.db.lock().unwrap();
        let acct = match db.get_or_init_account(self.cfg.initial_capital) {
            Ok(a) => a,
            Err(_) => return 0,
        };
        let total = db.total_asset(&acct).unwrap_or(acct.cash);
        let price = db.latest_snapshot_price(&intent.thscode).unwrap_or(None);
        let price = match price {
            Some(p) if p > 0.0 => p,
            _ => db.prev_close(&intent.thscode).unwrap_or(None).unwrap_or(0.0),
        };
        if price <= 0.0 {
            return 0;
        }
        let positions = db.positions().unwrap_or_default();
        if positions.len() >= 3 && !positions.iter().any(|p| p.thscode == intent.thscode) {
            return 0; // 已持有 3 只且非加仓
        }
        let current_mv: f64 = positions
            .iter()
            .filter(|p| p.thscode != intent.thscode)
            .map(|p| p.quantity as f64 * db.latest_snapshot_price(&p.thscode).unwrap_or(None).unwrap_or(p.avg_cost))
            .sum();
        // 单票上限：20% 总资产
        let single_max = total * 0.20;
        // 总仓位约束：剩余可买 = max_total_pct × total - current_mv
        let total_room = (max_total_pct * total - current_mv).max(0.0);
        let budget = single_max.min(total_room).min(acct.cash);
        let qty = (budget / price / 100.0).floor() as u32 * 100;
        qty
    }

    /// 执行风控强制卖出。
    async fn execute_sells(&self, sells: &[finbox_core::OrderIntent]) -> anyhow::Result<()> {
        for s in sells {
            match self.broker.submit(s.clone()).await {
                Ok(e) => log::info!("[风控卖出] {} {} {}股 @ {:.2} 费 {:.2}", e.intent.side.as_str(), e.intent.thscode, e.intent.quantity, e.price, e.fee),
                Err(e) => log::info!("[风控卖出拒单] {}: {e}", s.thscode),
            }
        }
        Ok(())
    }

    /// 盘中风控：只处理强制卖出（止损等），不买入。
    async fn run_risk_sells(&self) -> anyhow::Result<()> {
        let report = self.risk.evaluate()?;
        if !report.forced_sells.is_empty() {
            self.execute_sells(&report.forced_sells).await?;
        }
        Ok(())
    }

    /// 收盘账户快照。
    async fn snapshot_account(&self) -> anyhow::Result<()> {
        let acct = self.broker.account().await?;
        let total = self.db.lock().unwrap().total_asset(&acct)?;
        let mv = total - acct.cash;
        let ts = chrono::Utc::now().timestamp_millis();
        self.db.lock().unwrap().insert_account_snapshot(ts, acct.cash, mv, total)?;
        log::info!("[收盘快照] 现金 {:.2} 市值 {:.2} 总资产 {:.2}", acct.cash, mv, total);
        Ok(())
    }
}
