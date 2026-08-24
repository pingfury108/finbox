//! finbox-decision：AI 决策引擎。
//!
//! 流程：全市场初筛（从本地快照/日K取候选）→ 构建上下文（账户/持仓/候选/趋势/复盘）
//! → LLM（OpenAI 兼容）→ 解析为 `Vec<OrderIntent>` → 留痕。
//!
//! 决策只产出意图，不触碰执行；执行交由 `finbox-trader::Broker`。

mod context;
mod llm;
mod screen;

use finbox_core::OrderIntent;
use finbox_store::{DecisionLog, SharedDb};
use log::{info, warn};
use thiserror::Error;

pub use screen::Candidate;

/// LLM 配置。
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

/// 决策结果。
#[derive(Debug, Clone)]
pub struct DecisionResult {
    /// 解析出的委托意图（含 hold 之外的 buy/sell）
    pub intents: Vec<OrderIntent>,
    /// 决策状态：executed / hold / rejected / error
    pub status: String,
    /// 备注（拒单原因、提名处理、comment 等）
    pub note: String,
    /// 决策日志 ID
    pub log_id: i64,
    /// LLM 原始输出
    pub raw_response: String,
}

#[derive(Debug, Error)]
pub enum DecisionError {
    #[error("存储错误: {0}")]
    Store(#[from] finbox_store::StoreError),
    #[error("LLM 调用失败: {0}")]
    Llm(String),
    #[error("LLM 输出解析失败: {0}")]
    Parse(String),
    #[error("未配置 LLM_API_KEY")]
    MissingApiKey,
}

/// 决策引擎。
pub struct DecisionEngine {
    market: SharedDb,
    acct: SharedDb,
    config: std::sync::Mutex<LlmConfig>,
    /// 自选池（可为空）
    watchlist: Vec<String>,
}

impl DecisionEngine {
    pub fn new(market: SharedDb, acct: SharedDb, config: LlmConfig, watchlist: Vec<String>) -> Self {
        Self { market, acct, config: std::sync::Mutex::new(config), watchlist }
    }

    /// 执行一轮决策：初筛 → 上下文 → LLM → 意图。
    /// `candidate_count` 为初筛输出候选数（少而精，建议 3-5）。
    pub async fn decide(&self, candidate_count: usize) -> Result<DecisionResult, DecisionError> {
        let candidates = {
            let m = self.market.lock().unwrap();
            screen::screen(&m, candidate_count)?
        };
        info!("初筛完成：{} 只候选", candidates.len());
        for c in &candidates {
            info!("  候选 {} {} 现价{:.2} 涨幅{:.2}% 评分{:.2}", c.thscode, c.name, c.price, c.pct, c.score);
        }
        let ctx = {
            let (m, a) = (self.market.clone(), self.acct.clone());
            context::build_context(&m, &a, &self.watchlist, &candidates)?
        };

        if self.config.lock().unwrap().api_key.is_empty() {
            let log = self.log_decision("", "", "[]", "rejected", "未配置 LLM_API_KEY，跳过");
            return Ok(DecisionResult {
                intents: vec![],
                status: "rejected".into(),
                note: "未配置 LLM_API_KEY".into(),
                log_id: log,
                raw_response: String::new(),
            });
        }

        // 复制配置再释放锁，避免跨 await 持锁
        let llm_cfg = self.config.lock().unwrap().clone();
        info!("调用 LLM: {} 模型 {}", llm_cfg.base_url, llm_cfg.model);
        let raw = match llm::chat(&llm_cfg, &ctx).await {
            Ok(r) => r,
            Err(e) => {
                let note = format!("LLM 调用失败: {e}");
                warn!("{note}");
                let log = self.log_decision(&ctx, "", "[]", "error", &note);
                return Ok(DecisionResult {
                    intents: vec![],
                    status: "error".into(),
                    note,
                    log_id: log,
                    raw_response: String::new(),
                });
            }
        };
        info!("LLM 返回 {} 字符", raw.len());

        let parsed = match llm::parse(&raw) {
            Ok(p) => p,
            Err(e) => {
                let note = format!("LLM 输出解析失败: {e}");
                let log = self.log_decision(&ctx, &raw, "[]", "error", &note);
                return Ok(DecisionResult {
                    intents: vec![],
                    status: "error".into(),
                    note,
                    log_id: log,
                    raw_response: raw,
                });
            }
        };

        let mut intents = llm::to_intents(&parsed.actions);
        let log = self.log_decision(&ctx, &raw, &parsed.actions_json, "parsed", &parsed.comment);
        // 决策与成交关联：意图带上决策日志 id
        for i in intents.iter_mut() {
            i.decision_id = Some(log);
        }
        info!("AI 决策完成: 状态={} 意图{}条 comment={}", "parsed", intents.len(), parsed.comment);
        for i in &intents {
            info!("  意图 {} {} {}股", i.side.as_str(), i.thscode, i.quantity);
        }
        Ok(DecisionResult {
            intents,
            status: "parsed".into(),
            note: format!("comment: {}", parsed.comment),
            log_id: log,
            raw_response: raw,
        })
    }

    /// 从行情库 meta 热加载 LLM 配置（页面改 key 即时生效）。
    /// 返回是否变化。
    pub fn reload_llm(&self, market: &SharedDb, defaults: &LlmConfig) -> bool {
        let m = market.lock().unwrap();
        let get = |k: &str| m.meta_get(k).ok().flatten();
        let base = get("llm_base_url").unwrap_or_else(|| defaults.base_url.clone());
        let key = get("llm_api_key").unwrap_or_else(|| defaults.api_key.clone());
        let model = get("llm_model").unwrap_or_else(|| defaults.model.clone());
        let mut cfg = self.config.lock().unwrap();
        if base != cfg.base_url || key != cfg.api_key || model != cfg.model {
            cfg.base_url = base;
            cfg.api_key = key;
            cfg.model = model;
            return true;
        }
        false
    }

    /// 记录一次未调 LLM 的轮次（风控拦截/无 LLM key），用于决策留痕完整性。
    pub fn log_skip(&self, status: &str, note: &str) -> i64 {
        self.log_decision("", "", "[]", status, note)
    }

    fn log_decision(&self, ctx: &str, raw: &str, actions: &str, status: &str, note: &str) -> i64 {        let ts = chrono::Utc::now().timestamp_millis();
        self.acct
            .lock()
            .unwrap()
            .insert_decision_log(&DecisionLog {
                id: 0,
                ts_ms: ts,
                model: self.config.lock().unwrap().model.clone(),
                context: ctx.to_string(),
                raw_response: raw.to_string(),
                actions: actions.to_string(),
                status: status.to_string(),
                note: note.to_string(),
            })
            .unwrap_or(0)
    }
}
