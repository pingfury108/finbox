//! 调度器：单进程多账户。
//!
//! - 采集任务（全局唯一）：盘前同步日K/复权 → 盘中采集快照（写 market 库）
//! - 账户任务（每账户一个）：盘中风控监控 → 收盘决策/执行/复盘/快照（写各自账户库）
//!
//! 账户间完全独立：各自持仓/资金/决策/风控状态，互不干扰。
//! 全局配置（key 等）与账户配置（资金/自选池）均存库 meta，每次读取 → 页面改配置即时生效。

use std::time::Duration;

use chrono::{Datelike, Local, Timelike};
use finbox_collector::Collector;
use finbox_decision::{DecisionEngine, LlmConfig};
use finbox_store::SharedDb;
use finbox_trader::{Broker, RiskConfig, RiskManager, SimBroker};
use hithink_sdk::Client;

use crate::accounts;
use crate::config::Config;

/// 单个账户的运行上下文。
struct AccountCtx {
    cfg: Config,
    name: String,
    /// 今日是否已收盘处理
    closed_today: bool,
    market: SharedDb,
    acct: SharedDb,
    broker: SimBroker,
    decision: DecisionEngine,
    risk: RiskManager,
}

/// 账户配置（从账户库 meta 读取，热更新）。
#[derive(Debug, Clone)]
struct AcctConf {
    initial_capital: f64,
    watchlist: Vec<String>,
    candidate_count: usize,
    risk: RiskConfig,
}

pub struct Scheduler {
    cfg: Config,
    market: SharedDb,
    collector: Collector,
    /// 上次采集时刻（分钟）
    last_collect: i64,
    /// 今日盘前同步标记
    pre_open_done: bool,
}

impl Scheduler {
    pub fn new(cfg: Config) -> anyhow::Result<Self> {
        let market = accounts::open_market(&cfg.data_dir)?;
        let client = Client::new(cfg.hithink_api_key.clone())?;
        let collector = Collector::new(client, market.clone());
        Ok(Self { cfg, market, collector, last_collect: 0, pre_open_done: false })
    }

    /// 主循环：采集任务 + 每个账户独立任务 + Web 界面，同进程并行。
    pub async fn run(self) -> anyhow::Result<()> {
        let mut handles = Vec::new();

        // Web 界面（同进程，端口用环境变量 FINBOX_BIND，默认 0.0.0.0:8000）
        let cfg_web = self.cfg.clone();
        handles.push(tokio::spawn(async move {
            let bind = std::env::var("FINBOX_BIND").unwrap_or_else(|_| "0.0.0.0:8000".into());
            crate::web::serve(&cfg_web, &bind).await.map_err(|e| anyhow::anyhow!("Web: {e}"))
        }));

        // 账户任务
        let accounts = accounts::list_accounts(&self.cfg.data_dir)?;
        if accounts.is_empty() {
            log::warn!("没有任何账户，先创建：finbox account create <name>");
        }
        for info in accounts {
            let cfg = self.cfg.clone();
            let handle = tokio::spawn(async move {
                let market = accounts::open_market(&cfg.data_dir)?;
                let acct = accounts::open_account(&cfg.data_dir, &info.name)?;
                let mut ctx = build_account_ctx(&cfg, &info.name, market, acct);
                ctx.run_account().await
            });
            handles.push(handle);
        }

        // 采集任务（本进程内，写 market 库）
        let mut s2 = self;
        let collect_handle = tokio::spawn(async move { s2.run_collector().await });

        // 等待所有任务（账户任务异常退出会让整个进程退出；采集任务常驻）
        for h in handles {
            if let Err(e) = h.await {
                log::error!("任务失败: {e}");
            }
        }
        let _ = collect_handle.await;
        Ok(())
    }

    /// 采集任务：盘前同步 + 盘中采集快照。每次从库读取同花顺 key（热生效）。
    async fn run_collector(&mut self) -> anyhow::Result<()> {
        loop {
            let now = Local::now();
            let today = now.format("%Y%m%d").to_string();
            let weekday_iso = now.weekday().num_days_from_monday() + 1;
            let minute = now.hour() * 60 + now.minute();

            // 工作日才可能交易；周末直接跳过
            if weekday_iso < 6 {
                // 盘前同步：工作日 9:00-9:15 先刷新交易日历（即使旧日历不含今天，
                // 也先拉新日历，避免“日历旧→判定非交易日→不刷新”的死循环）
                if minute >= 9 * 60 && minute < 9 * 60 + 15 && !self.pre_open_done {
                    self.refresh_collector_key()?;
                    let days = self.collector.client.trading_days().await?;
                    self.collector.upsert_trading_days(&days).await?;
                    self.collector.sync_daily_bars(std::path::Path::new("data/dumps"), &days).await?;
                    self.collector.import_adjustment_factors(std::path::Path::new("data/dumps")).await?;
                    log::info!("[采集][盘前] 同步完成（{} 个交易日）", days.item.len());
                    self.pre_open_done = true;
                }

                // 用最新日历判断今天是否交易日
                let trading_day = self.market.lock().unwrap().is_trading_day(&today)?;
                if trading_day && minute >= 9 * 60 + 30 && minute < 15 * 60 {
                    // 盘中采集
                    let min = now.timestamp() / 60;
                    if min - self.last_collect >= self.cfg.collect_interval_seconds as i64 / 60 {
                        self.refresh_collector_key()?;
                        let n = self.collector.collect_market_snapshot().await?;
                        log::info!("[采集][盘中] 快照 {n} 只");
                        self.last_collect = min;
                    }
                }

                if minute < 9 * 60 {
                    self.pre_open_done = false;
                }
            }

            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    }

    /// 从 market 库 meta 刷新同花顺 key（页面配置即时生效）。
    fn refresh_collector_key(&mut self) -> anyhow::Result<()> {
        let key = self
            .market
            .lock()
            .unwrap()
            .meta_get("hithink_api_key")?
            .unwrap_or_else(|| self.cfg.hithink_api_key.clone());
        if !key.is_empty() && key != self.cfg.hithink_api_key {
            self.collector.client = Client::new(key)?;
        }
        Ok(())
    }
}

/// 构建某账户的决策引擎（手动 decide 用）。
pub fn build_decision_engine(
    cfg: &Config,
    market: SharedDb,
    acct: SharedDb,
) -> DecisionEngine {
    let conf = read_acct_conf(cfg, &acct);
    DecisionEngine::new(
        market,
        acct,
        LlmConfig {
            base_url: cfg.llm_base_url.clone(),
            api_key: cfg.llm_api_key.clone(),
            model: cfg.llm_model.clone(),
        },
        conf.watchlist,
    )
}

fn build_account_ctx(
    cfg: &Config,
    name: &str,
    market: SharedDb,
    acct: SharedDb,
) -> AccountCtx {
    let conf = read_acct_conf(cfg, &acct);
    let decision = DecisionEngine::new(
        market.clone(),
        acct.clone(),
        LlmConfig {
            base_url: cfg.llm_base_url.clone(),
            api_key: cfg.llm_api_key.clone(),
            model: cfg.llm_model.clone(),
        },
        conf.watchlist.clone(),
    );
    let broker = SimBroker::new(market.clone(), acct.clone(), conf.initial_capital);
    let risk = RiskManager::new(market.clone(), acct.clone(), conf.risk);
    AccountCtx { cfg: cfg.clone(), name: name.into(), market, acct, broker, decision, risk, closed_today: false }
}

/// 读取账户配置（meta 优先，.env 兜底），每次调用都读 → 热更新。
fn read_acct_conf(cfg: &Config, acct: &SharedDb) -> AcctConf {
    let db = acct.lock().unwrap();
    let get = |key: &str, def: f64| -> f64 {
        db.meta_get(key)
            .ok()
            .flatten()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(def)
    };
    let watch = db
        .meta_get("watchlist")
        .ok()
        .flatten()
        .unwrap_or_default();
    let count = get("candidate_count", cfg.candidate_count as f64) as usize;
    AcctConf {
        initial_capital: get("initial_capital", cfg.initial_capital),
        watchlist: watch.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        candidate_count: count.max(1),
        risk: RiskConfig::default(),
    }
}

impl AccountCtx {
    /// 账户任务主循环：盘中风控 + 收盘决策。
    /// 交易日历由采集任务盘前刷新；这里按“工作日 + 时段”运行，避免旧日历误判。
    async fn run_account(&mut self) -> anyhow::Result<()> {
        log::info!("[{}] 账户任务启动，开始监控", self.name);
        loop {
            let now = Local::now();
            let weekday_iso = now.weekday().num_days_from_monday() + 1;
            let minute = now.hour() * 60 + now.minute();

            if weekday_iso < 6 {
                // 盘中：风控监控（止损/止盈/超期）
                if minute >= 9 * 60 + 30 && minute < 15 * 60 {
                    self.intraday_risk().await?;
                }
                // 收盘：风控 + 决策 + 复盘 + 快照
                if minute >= 15 * 60 + 5 && !self.closed_today {
                    self.close_process().await?;
                    self.closed_today = true;
                }
                if minute < 9 * 60 {
                    self.closed_today = false;
                }
            }

            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    }

    async fn intraday_risk(&self) -> anyhow::Result<()> {
        let report = self.risk.evaluate()?;
        if !report.forced_sells.is_empty() {
            log::info!("[{}][盘中风控] 强制卖出 {} 笔", self.name, report.forced_sells.len());
            self.execute_sells(&report.forced_sells).await?;
        }
        Ok(())
    }

    /// 收盘流程：① 风控强制卖出 ② 每日决策买入 ③ 复盘 ④ 账户快照。
    async fn close_process(&self) -> anyhow::Result<()> {
        log::info!("[{}][收盘] 开始每日结算", self.name);
        let conf = read_acct_conf(&self.cfg, &self.acct);
        let _ = conf;

        let report = self.risk.evaluate()?;
        self.execute_sells(&report.forced_sells).await?;
        log::info!(
            "[{}][风控] 市场{} 目标仓位{} 可买入={} 强制卖出{}笔",
            self.name, report.regime, report.max_total_pct, report.can_buy, report.forced_sells.len()
        );

        if report.can_buy && report.max_total_pct > 0.0 {
            self.daily_decision(&report).await?;
        }

        self.review_old_decisions().await?;
        self.snapshot_account().await?;
        Ok(())
    }

    async fn daily_decision(&self, report: &finbox_trader::RiskReport) -> anyhow::Result<()> {
        let conf = read_acct_conf(&self.cfg, &self.acct);
        // 热加载 LLM 配置（页面改 key 即时生效）
        let defaults = LlmConfig {
            base_url: self.cfg.llm_base_url.clone(),
            api_key: self.cfg.llm_api_key.clone(),
            model: self.cfg.llm_model.clone(),
        };
        self.decision.reload_llm(&self.market, &defaults);
        let result = self.decision.decide(conf.candidate_count).await?;
        log::info!("[{}][决策] 状态 {} 意图 {} 条", self.name, result.status, result.intents.len());
        for intent in &result.intents {
            if intent.side != finbox_core::OrderSide::Buy {
                continue;
            }
            let qty = self.position_size(intent, report.max_total_pct).await;
            if qty < 100 {
                log::info!("[{}][决策] {} 仓位不足一手，跳过", self.name, intent.thscode);
                continue;
            }
            let mut i = intent.clone();
            i.quantity = qty;
            match self.broker.submit(i.clone()).await {
                Ok(e) => log::info!("[{}][成交] {} {} {}股 @ {:.2}", self.name, e.intent.side.as_str(), e.intent.thscode, e.intent.quantity, e.price),
                Err(e) => log::info!("[{}][拒单] {}: {e}", self.name, i.thscode),
            }
        }
        Ok(())
    }

    async fn position_size(&self, intent: &finbox_core::OrderIntent, max_total_pct: f64) -> u32 {
        let acct = self.acct.lock().unwrap();
        let conf = read_acct_conf(&self.cfg, &self.acct);
        let account = match acct.get_or_init_account(conf.initial_capital) {
            Ok(a) => a,
            Err(_) => return 0,
        };
        let total_est = acct.total_asset_estimate(&account).unwrap_or(account.cash);
        let price = self.market.lock().unwrap().latest_snapshot_price(&intent.thscode).ok().flatten().unwrap_or(0.0);
        if price <= 0.0 {
            return 0;
        }
        let positions = acct.positions().unwrap_or_default();
        if positions.len() >= 3 && !positions.iter().any(|p| p.thscode == intent.thscode) {
            return 0;
        }
        let single_max = total_est * 0.20;
        let total_room = (max_total_pct * total_est).max(0.0);
        let budget = single_max.min(total_room).min(account.cash);
        (budget / price / 100.0).floor() as u32 * 100
    }

    async fn execute_sells(&self, sells: &[finbox_core::OrderIntent]) -> anyhow::Result<()> {
        for s in sells {
            match self.broker.submit(s.clone()).await {
                Ok(e) => log::info!("[{}][风控卖出] {} {} {}股 @ {:.2}", self.name, e.intent.side.as_str(), e.intent.thscode, e.intent.quantity, e.price),
                Err(e) => log::info!("[{}][风控卖出拒] {}: {e}", self.name, s.thscode),
            }
        }
        Ok(())
    }

    async fn review_old_decisions(&self) -> anyhow::Result<()> {
        let decisions = self.acct.lock().unwrap().recent_decision_logs(50)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        for d in decisions {
            for days_after in [1u32, 5, 10] {
                let db = self.acct.lock().unwrap();
                if db.review_exists(d.id, days_after)? {
                    continue;
                }
                let age_days = (now_ms - d.ts_ms) / 86_400_000;
                if age_days < days_after as i64 {
                    continue;
                }
                let trades = db.trades_for_decision(d.id)?;
                if trades.is_empty() {
                    continue;
                }
                drop(db);
                let mut lines = Vec::new();
                let mut total_pnl = 0.0;
                for t in &trades {
                    let cur = self
                        .market
                        .lock()
                        .unwrap()
                        .latest_snapshot_price(&t.thscode)?
                        .unwrap_or(t.price);
                    let diff = (cur - t.price) * t.quantity as f64;
                    let diff = if t.side == finbox_core::OrderSide::Sell { -diff } else { diff };
                    total_pnl += diff;
                    let verdict = if (t.side == finbox_core::OrderSide::Buy) == (diff >= 0.0) { "对" } else { "错" };
                    lines.push(format!(
                        "{} {} @ {:.2} → 现价 {:.2} 浮动 {diff:+.0}元 判断【{verdict}】",
                        t.side.as_str(), t.thscode, t.price, cur
                    ));
                }
                self.acct.lock().unwrap().insert_review(d.id, days_after, &lines.join("; "), total_pnl)?;
                log::info!("[{}][复盘] 决策#{} {}天后 盈亏 {total_pnl:+.0}元", self.name, d.id, days_after);
            }
        }
        Ok(())
    }

    async fn snapshot_account(&self) -> anyhow::Result<()> {
        let acct = self.acct.lock().unwrap();
        let conf = read_acct_conf(&self.cfg, &self.acct);
        let account = acct.get_or_init_account(conf.initial_capital)?;
        let total = acct.total_asset_estimate(&account)?;
        drop(acct);
        let ts = chrono::Utc::now().timestamp_millis();
        self.acct
            .lock()
            .unwrap()
            .insert_account_snapshot(ts, account.cash, total - account.cash, total)?;
        log::info!("[{}][收盘快照] 现金 {:.2} 市值 {:.2} 总资产 {:.2}", self.name, account.cash, total - account.cash, total);
        Ok(())
    }
}
