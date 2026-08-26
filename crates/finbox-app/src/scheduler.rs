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
    /// 今日收盘后是否已同步日K
    after_close_synced: bool,
}

impl Scheduler {
    pub fn new(cfg: Config) -> anyhow::Result<Self> {
        let market = accounts::open_market(&cfg.data_dir)?;
        let client = Client::new(cfg.hithink_api_key.clone())?;
        let collector = Collector::new(client, market.clone());
        Ok(Self { cfg, market, collector, last_collect: 0, pre_open_done: false, after_close_synced: false })
    }

    /// 主循环：采集任务 + 账户动态发现 + Web 界面，同进程并行。
    pub async fn run(mut self) -> anyhow::Result<()> {
        // 空库自检：tickers 为空说明从未 init，自动执行首次全量建库
        let empty = self.market.lock().unwrap().stats().map(|s| s.tickers == 0).unwrap_or(false);
        if empty {
            log::info!("[初始化] 检测到空库，开始首次全量建库（代码表+日历+10年日K+复权，约3分钟）...");
            self.auto_init().await?;
            log::info!("[初始化] 建库完成，启动系统");
        }

        let mut handles = Vec::new();

        // Web 界面（同进程，端口用环境变量 FINBOX_BIND，默认 0.0.0.0:8000）
        // market 传共享连接：同进程另开 DuckDB 实例与采集端互不可见
        let cfg_web = self.cfg.clone();
        let market_web = self.market.clone();
        handles.push(tokio::spawn(async move {
            let bind = std::env::var("FINBOX_BIND").unwrap_or_else(|_| "0.0.0.0:8000".into());
            crate::web::serve(&cfg_web, &bind, market_web).await.map_err(|e| anyhow::anyhow!("Web: {e}"))
        }));

        // 账户监督任务：定期扫描，动态发现新账户/删除账户
        let cfg_acct = self.cfg.clone();
        handles.push(tokio::spawn(async move {
            account_supervisor(cfg_acct).await
        }));

        // 采集任务（本进程内，写 market 库）
        let mut s2 = self;
        let collect_handle = tokio::spawn(async move { s2.run_collector().await });

        // 等待所有任务
        for h in handles {
            if let Err(e) = h.await {
                log::error!("任务失败: {e}");
            }
        }
        let _ = collect_handle.await;
        Ok(())
    }

    /// 首次全量建库（自动 init）：失败即中止启动（无数据的系统空转无意义）。
    async fn auto_init(&mut self) -> anyhow::Result<()> {
        use anyhow::Context;
        self.refresh_collector_key()?;
        let dump_dir = std::path::Path::new(&self.cfg.data_dir).join("dumps");
        let n = self.collector.sync_tickers().await.context("同步代码表失败")?;
        log::info!("[初始化] 代码表: {n} 只");
        let days = self.collector.client.trading_days().await.context("获取交易日历失败（检查 HITHINK_FINANCE_API_KEY）")?;
        self.collector.upsert_trading_days(&days).await?;
        log::info!("[初始化] 交易日历: {} 天", days.item.len());
        self.collector.sync_daily_bars(&dump_dir, &days).await.context("同步日K失败")?;
        let n = self.collector.import_adjustment_factors(&dump_dir).await.context("导入复权因子失败")?;
        log::info!("[初始化] 复权事件: {n} 行");
        let n = self.collector.sync_index_bars(1200).await.context("同步指数日K失败")?;
        log::info!("[初始化] 指数日K: {n} 根");
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
                        // 指数快照一并采集（盘中实时）
                        if let Err(e) = self.collector.collect_index_snapshot().await {
                            log::warn!("[采集][盘中] 指数快照失败: {e}");
                        }
                        log::info!("[采集][盘中] 快照 {n} 只");
                        self.last_collect = min;
                    }
                }
                // 收盘后 15:30：同步当天日K（盘前只到昨日，当天日K收盘后才产生）
                if trading_day && minute >= 15 * 60 + 30 && !self.after_close_synced {
                    self.refresh_collector_key()?;
                    let days = self.collector.client.trading_days().await?;
                    self.collector.sync_daily_bars(std::path::Path::new("data/dumps"), &days).await?;
                    // 指数日K也补当天
                    let _ = self.collector.sync_index_bars(1200).await;
                    log::info!("[采集][收盘后] 当日日K+指数同步完成");
                    self.after_close_synced = true;
                }

                if minute < 9 * 60 {
                    self.pre_open_done = false;
                    self.after_close_synced = false;
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

/// 账户监督任务：每 60s 扫描账户目录，动态发现新账户（spawn 任务）与删除账户（停止任务）。
/// 使 Web 新建/删除账户即时生效，无需重启主进程。
async fn account_supervisor(cfg: Config) -> anyhow::Result<()> {
    use std::collections::HashMap;
    use tokio::sync::mpsc;

    let mut running: HashMap<String, mpsc::Sender<()>> = HashMap::new();
    loop {
        let accounts = accounts::list_accounts(&cfg.data_dir)?;
        let names: std::collections::HashSet<String> =
            accounts.into_iter().map(|a| a.name).collect();

        // 启动新账户任务
        for name in &names {
            if running.contains_key(name) {
                continue;
            }
            log::info!("[账户] 动态发现新账户「{name}」，启动任务");
            let (tx, mut rx) = mpsc::channel::<()>(1);
            let cfg2 = cfg.clone();
            let name2 = name.clone();
            tokio::spawn(async move {
                let market = accounts::open_market(&cfg2.data_dir);
                let acct = accounts::open_account(&cfg2.data_dir, &name2);
                match (market, acct) {
                    (Ok(market), Ok(acct)) => {
                        let mut ctx = build_account_ctx(&cfg2, &name2, market, acct);
                        // 账户循环运行，直到收到停止信号或账户被删除
                        tokio::select! {
                            r = ctx.run_account() => log::error!("[{}] 账户任务异常退出: {:?}", name2, r.err()),
                            _ = rx.recv() => log::info!("[{}] 账户任务已停止", name2),
                        }
                    }
                    _ => log::error!("[{name2}] 打开账户库失败"),
                }
            });
            running.insert(name.clone(), tx);
        }

        // 停止已删除账户的任务
        let removed: Vec<String> = running.keys().filter(|n| !names.contains(*n)).cloned().collect();
        for n in removed {
            log::info!("[账户] 账户「{n}」已删除，停止任务");
            if let Some(tx) = running.remove(&n) {
                let _ = tx.send(()).await;
            }
        }

        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

impl AccountCtx {    /// 账户任务主循环：盘中持续监控。
    /// - 风控（止损/止盈/超期）：盘中每 60s 实时检查，情况不妙立即卖出
    /// - AI 决策：固定时点（开盘 9:35 / 午间 11:25 / 尾盘 14:55）+ 每 DECISION_INTERVAL 分钟轮询
    /// - 收盘 15:05：复盘 + 账户快照
    async fn run_account(&mut self) -> anyhow::Result<()> {
        log::info!("[{}] 账户任务启动，开始监控", self.name);
        // 启动即应用待处理除权事件（分红/送股）
        self.apply_adjustments();
        let mut last_decision_min = 0i64;
        let mut last_adjust_date = String::new();
        // 固定决策时点（分钟）：开盘/午间/尾盘
        let fixed_points = [9 * 60 + 35, 11 * 60 + 25, 14 * 60 + 55];
        loop {
            let now = Local::now();
            let weekday_iso = now.weekday().num_days_from_monday() + 1;
            let minute = now.hour() * 60 + now.minute();
            let min = now.timestamp() / 60;

            if weekday_iso < 6 {
                // 每日首次进入工作日循环时应用除权（除权日开盘前生效）
                let today = now.format("%Y-%m-%d").to_string();
                if today != last_adjust_date && minute >= 9 * 60 {
                    self.apply_adjustments();
                    last_adjust_date = today;
                }
                // 盘中（9:30-15:00）
                if minute >= 9 * 60 + 30 && minute < 15 * 60 {
                    // ① 风控实时监控（每 60s）：止损/止盈/超期 → 立即卖出
                    self.intraday_risk().await?;

                    // ② AI 决策：固定时点 或 距上次决策超过间隔
                    let interval = self.decision_interval();
                    let is_fixed = fixed_points.contains(&minute);
                    let due = is_fixed || min - last_decision_min >= interval as i64;
                    if due {
                        self.periodic_decision().await?;
                        last_decision_min = min;
                    }
                }
                // 收盘后 15:05：复盘 + 账户快照（每天一次）
                if minute >= 15 * 60 + 5 && !self.closed_today {
                    self.review_old_decisions().await?;
                    self.snapshot_account().await?;
                    self.closed_today = true;
                }
                if minute < 9 * 60 {
                    self.closed_today = false;
                }
            }

            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    }

    /// 决策间隔（分钟）：账户库 meta 可配置，默认 30。
    fn decision_interval(&self) -> u64 {
        let db = self.acct.lock().unwrap();
        db.meta_get("decision_interval_minutes")
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30)
    }

    /// 应用持仓的除权除息事件（分红入账/送股摊薄成本）。
    fn apply_adjustments(&self) {
        let today_ms = chrono::Local::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_local_timezone(chrono::Local)
            .unwrap()
            .timestamp_millis();
        let acct = self.acct.lock().unwrap();
        let m = self.market.lock().unwrap();
        match finbox_trader::apply_pending_adjustments(&acct, &m, today_ms) {
            Ok(0) => {}
            Ok(n) => log::info!("[{}][除权] 应用 {} 个事件", self.name, n),
            Err(e) => log::warn!("[{}][除权] 应用失败: {e}", self.name),
        }
    }

    /// 盘中风控：止损/止盈/超期，触发生成卖出并执行。
    async fn intraday_risk(&self) -> anyhow::Result<()> {
        let report = self.risk.evaluate()?;
        if !report.forced_sells.is_empty() {
            log::info!("[{}][盘中风控] 强制卖出 {} 笔", self.name, report.forced_sells.len());
            self.execute_sells(&report.forced_sells).await?;
        }
        Ok(())
    }

    /// 定期 AI 决策：风控门控 → 初筛+LLM → 买入/卖出意图执行。
    async fn periodic_decision(&self) -> anyhow::Result<()> {
        let report = self.risk.evaluate()?;
        // 账户门槛（熔断）与市场门槛（risk-off 目标仓位）独立判断
        let acct_ok = report.can_buy;
        let mkt_ok = report.max_total_pct > 0.0;
        let npos = self.acct.lock().unwrap().positions().map(|p| p.len()).unwrap_or(0);
        log::info!(
            "[{}][决策] 市场{} 目标仓位{:.0}% 账户可买={} 强制卖出{}笔 当前持仓{}只",
            self.name, report.regime, report.max_total_pct * 100.0, acct_ok, report.forced_sells.len(), npos
        );
        // 风控强制卖出优先执行
        self.execute_sells(&report.forced_sells).await?;

        // 门槛不满足则跳过买入
        if !acct_ok || !mkt_ok {
            let reason = if !acct_ok {
                "账户熔断/回撤中"
            } else if report.regime == "risk-off" {
                "市场走弱(risk-off)，只减不买"
            } else {
                "市场中性，暂不追买"
            };
            log::info!("[{}][决策] 空仓{}，{}，本轮不买入", self.name, if npos == 0 { "(空仓)" } else { "" }, reason);
            // 留痕：被拦截的轮次也写入 AI 建议记录
            self.decision.log_skip("hold", &format!("{}，当前持仓{}只", reason, npos));
            return Ok(());
        }

        let conf = read_acct_conf(&self.cfg, &self.acct);
        let defaults = LlmConfig {
            base_url: self.cfg.llm_base_url.clone(),
            api_key: self.cfg.llm_api_key.clone(),
            model: self.cfg.llm_model.clone(),
        };
        self.decision.reload_llm(&self.market, &defaults);
        let result = self.decision.decide(conf.candidate_count).await?;
        log::info!("[{}][决策] 状态 {} 意图 {} 条", self.name, result.status, result.intents.len());
        // 执行结果汇总：全部成交 → executed；有拒单 → rejected（追加原因）
        let mut filled = 0usize;
        let mut rejects: Vec<String> = Vec::new();
        for intent in &result.intents {
            match intent.side {
                finbox_core::OrderSide::Buy => {
                    if let Some(r) = self.execute_buy(intent, report.max_total_pct).await? {
                        rejects.push(r);
                    } else {
                        filled += 1;
                    }
                }
                finbox_core::OrderSide::Sell => {
                    // AI 主动性卖出（趋势走坏/到目标）
                    match self.broker.submit(intent.clone()).await {
                        Ok(e) => {
                            log::info!("[{}][成交] 卖出 {} {}股 @ {:.2}", self.name, e.intent.thscode, e.intent.quantity, e.price);
                            filled += 1;
                        }
                        Err(e) => {
                            log::info!("[{}][拒单] 卖出 {}: {e}", self.name, intent.thscode);
                            rejects.push(format!("{}: {e}", intent.thscode));
                        }
                    }
                }
            }
        }
        // 回写执行结果到决策日志（建议页可区分"说了做了"vs"说了没做成"）
        if result.log_id > 0 && result.status == "parsed" {
            let (st, note) = if !result.intents.is_empty() && rejects.is_empty() {
                ("executed", Some(format!(" [执行] 成交{}笔", filled)))
            } else if !rejects.is_empty() {
                ("rejected", Some(format!(" [执行] 成交{}笔 拒单{}笔: {}", filled, rejects.len(), rejects.join("; "))))
            } else {
                ("executed", None) // 无意图（观望类）
            };
            if let Err(e) = self.acct.lock().unwrap().update_decision_status(result.log_id, st, note.as_deref()) {
                log::warn!("[{}][决策] 回写执行状态失败: {e}", self.name);
            }
        }
        Ok(())
    }

    /// 执行买入：数量由系统按仓位约束计算。返回 Some(拒单原因) 表示未成交。
    async fn execute_buy(&self, intent: &finbox_core::OrderIntent, max_total_pct: f64) -> anyhow::Result<Option<String>> {
        let qty = self.position_size(intent, max_total_pct).await;
        if qty < 100 {
            log::info!("[{}][决策] {} 仓位不足一手，跳过", self.name, intent.thscode);
            return Ok(Some(format!("{} 仓位不足一手", intent.thscode)));
        }
        let mut i = intent.clone();
        i.quantity = qty;
        match self.broker.submit(i.clone()).await {
            Ok(e) => {
                log::info!("[{}][成交] 买入 {} {}股 @ {:.2}", self.name, e.intent.thscode, e.intent.quantity, e.price);
                Ok(None)
            }
            Err(e) => {
                log::info!("[{}][拒单] 买入 {}: {e}", self.name, i.thscode);
                Ok(Some(format!("{}: {e}", i.thscode)))
            }
        }
    }

    async fn position_size(&self, intent: &finbox_core::OrderIntent, max_total_pct: f64) -> u32 {
        // 先取行情价（market 锁，短），释放后再取账户/持仓（acct 锁），避免双锁嵌套死锁
        let price = self.market.lock().unwrap().latest_snapshot_price(&intent.thscode).ok().flatten().unwrap_or(0.0);
        if price <= 0.0 {
            return 0;
        }
        // read_acct_conf 内部会自行 lock acct，必须在持锁前调用（std Mutex 不可重入，否则自锁死锁）
        let conf = read_acct_conf(&self.cfg, &self.acct);
        let acct = self.acct.lock().unwrap();
        let account = match acct.get_or_init_account(conf.initial_capital) {
            Ok(a) => a,
            Err(_) => return 0,
        };
        let total_est = acct.total_asset_estimate(&account).unwrap_or(account.cash);
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
        // read_acct_conf 内部自行 lock，必须在持锁前调用（防自锁）
        let conf = read_acct_conf(&self.cfg, &self.acct);
        let acct = self.acct.lock().unwrap();
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
