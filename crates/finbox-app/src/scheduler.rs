//! 调度器：交易时段感知的采集 / 决策 / 执行循环。

use std::time::Duration;

use chrono::{Datelike, Local, Timelike};
use finbox_collector::Collector;
use finbox_decision::{DecisionEngine, LlmConfig};
use finbox_store::{open_shared, SharedDb};
use finbox_trader::{Broker, SimBroker};
use hithink_sdk::Client;

use crate::config::Config;

/// 常驻调度服务。
pub struct Scheduler {
    cfg: Config,
    db: SharedDb,
    collector: Collector,
    decision: DecisionEngine,
    broker: SimBroker,
    /// 上次采集时间（分钟级）
    last_collect_minute: i64,
    /// 上次决策时间
    last_decision_minute: i64,
    /// 今日是否已收盘补采
    closed_today: bool,
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
        Ok(Self {
            cfg,
            db,
            collector,
            decision,
            broker,
            last_collect_minute: 0,
            last_decision_minute: 0,
            closed_today: false,
        })
    }

    /// 主循环：每 20 秒 tick 一次。
    pub async fn run(&mut self) -> anyhow::Result<()> {
        loop {
            let now = Local::now();
            let today = now.format("%Y%m%d").to_string();
            let weekday_iso = now.weekday().num_days_from_monday() + 1;
            let minute = now.hour() * 60 + now.minute();

            // 是否交易日（交易日历）
            let trading_day = self.db.lock().unwrap().is_trading_day(&today)?;

            if trading_day {
                if weekday_iso >= 6 {
                    continue; // 日历有误，防御
                }
                // 盘前 9:15：补齐日K + 复权（若今天还没补）
                if minute >= 9 * 60 + 15 && minute < 9 * 60 + 30 {
                    self.pre_open_sync().await?;
                }
                // 盘中采集（交易时段内按间隔）
                if is_market_open(minute) {
                    if now.timestamp() / 60 - self.last_collect_minute >= self.cfg.collect_interval_seconds as i64 / 60 {
                        self.collect_now().await?;
                        self.last_collect_minute = now.timestamp() / 60;
                    }
                    if now.timestamp() / 60 - self.last_decision_minute
                        >= self.cfg.ai_decision_interval_minutes as i64 * 60
                    {
                        self.decide_and_execute().await?;
                        self.last_decision_minute = now.timestamp() / 60;
                    }
                    self.closed_today = false;
                } else if minute >= 15 * 60 + 5 && !self.closed_today {
                    // 收盘后补一次收盘快照 + 每日账户快照
                    self.collect_now().await?;
                    self.snapshot_account().await?;
                    self.closed_today = true;
                }
            }

            tokio::time::sleep(Duration::from_secs(20)).await;
        }
    }

    /// 盘前同步：交易日历 + 日K增量 + 复权事件。
    async fn pre_open_sync(&self) -> anyhow::Result<()> {
        let days = self.collector.client.trading_days().await?;
        self.collector.upsert_trading_days(&days).await?;
        self.collector.sync_daily_bars(std::path::Path::new("data/dumps"), &days).await?;
        self.collector.import_adjustment_factors(std::path::Path::new("data/dumps")).await?;
        Ok(())
    }

    /// 采一次全市场快照。
    async fn collect_now(&self) -> anyhow::Result<()> {
        let n = self.collector.collect_market_snapshot().await?;
        log::info!("采集快照 {n} 只");
        Ok(())
    }

    /// 一轮决策并执行：决策 → 意图 → Broker 下单。
    async fn decide_and_execute(&self) -> anyhow::Result<()> {
        let result = self.decision.decide(self.cfg.screen_top_n).await?;
        log::info!("决策状态 {}，意图 {} 条", result.status, result.intents.len());
        for intent in &result.intents {
            match self.broker.submit(intent.clone()).await {
                Ok(e) => log::info!(
                    "成交 {} {} {}股 @ {:.2} 费 {:.2}",
                    e.intent.side.as_str(),
                    e.intent.thscode,
                    e.intent.quantity,
                    e.price,
                    e.fee
                ),
                Err(e) => log::info!("拒单 {}: {}", intent.thscode, e),
            }
        }
        Ok(())
    }

    /// 每日收盘账户快照（复盘曲线用）。
    async fn snapshot_account(&self) -> anyhow::Result<()> {
        let acct = self.broker.account().await?;
        let total = self.db.lock().unwrap().total_asset(&acct)?;
        let mv = total - acct.cash;
        let ts = chrono::Utc::now().timestamp_millis();
        self.db
            .lock()
            .unwrap()
            .insert_account_snapshot(ts, acct.cash, mv, total)?;
        log::info!("收盘快照已落库: 现金 {:.2} 市值 {:.2} 总资产 {:.2}", acct.cash, mv, total);
        Ok(())
    }
}

/// 是否处于盘中交易时段（9:30-11:30 / 13:00-15:00）。
fn is_market_open(minute: u32) -> bool {
    (9 * 60 + 30 <= minute && minute < 11 * 60 + 30)
        || (13 * 60 <= minute && minute < 15 * 60)
}
