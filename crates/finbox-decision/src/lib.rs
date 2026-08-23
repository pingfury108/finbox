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
    db: SharedDb,
    config: LlmConfig,
    /// 自选池（可为空）
    watchlist: Vec<String>,
}

impl DecisionEngine {
    pub fn new(db: SharedDb, config: LlmConfig, watchlist: Vec<String>) -> Self {
        Self { db, config, watchlist }
    }

    /// 执行一轮决策：初筛 → 上下文 → LLM → 意图。
    /// `is_trading_time` 由调用方（调度器）告知，非交易时段用昨日候选并禁止下单由 Broker 兜底。
    pub async fn decide(&self, screen_top_n: u32) -> Result<DecisionResult, DecisionError> {
        let (_, ctx) = {
            let db = self.db.lock().unwrap();
            let candidates = screen::screen(&db, screen_top_n)?;
            let ctx = context::build_context(&db, &self.watchlist, &candidates)?;
            (candidates, ctx)
        };

        if self.config.api_key.is_empty() {
            let log = self.log_decision("", "", "[]", "rejected", "未配置 LLM_API_KEY，跳过");
            return Ok(DecisionResult {
                intents: vec![],
                status: "rejected".into(),
                note: "未配置 LLM_API_KEY".into(),
                log_id: log,
                raw_response: String::new(),
            });
        }

        let raw = match llm::chat(&self.config, &ctx).await {
            Ok(r) => r,
            Err(e) => {
                let note = format!("LLM 调用失败: {e}");
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
        Ok(DecisionResult {
            intents,
            status: "parsed".into(),
            note: format!("comment: {}", parsed.comment),
            log_id: log,
            raw_response: raw,
        })
    }

    fn log_decision(&self, ctx: &str, raw: &str, actions: &str, status: &str, note: &str) -> i64 {
        let ts = chrono::Utc::now().timestamp_millis();
        self.db
            .lock()
            .unwrap()
            .insert_decision_log(&DecisionLog {
                id: 0,
                ts_ms: ts,
                model: self.config.model.clone(),
                context: ctx.to_string(),
                raw_response: raw.to_string(),
                actions: actions.to_string(),
                status: status.to_string(),
                note: note.to_string(),
            })
            .unwrap_or(0)
    }
}
