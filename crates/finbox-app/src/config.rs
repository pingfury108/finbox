//! finbox-app 配置：从环境变量读取。

use anyhow::Result;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Config {
    /// 数据根目录（market.duckdb 与 accounts/ 都在其下）
    pub data_dir: String,
    /// 初始资金
    pub initial_capital: f64,
    /// 自选池（逗号分隔 thscode）
    pub watchlist: Vec<String>,
    /// 初筛输出候选数（少而精）
    pub candidate_count: usize,
    /// 行情采集间隔（秒）
    pub collect_interval_seconds: u64,
    /// AI 决策间隔（分钟）
    pub ai_decision_interval_minutes: u64,
    /// LLM 配置
    pub llm_base_url: String,
    pub llm_api_key: String,
    pub llm_model: String,
    /// 同花顺 API Key
    pub hithink_api_key: String,
    /// 管理口令（只从环境变量/参数读取，不设则不启用保护；Web 不可改）
    pub admin_key: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: "data".into(),
            initial_capital: 200_000.0,
            watchlist: vec![],
            candidate_count: 5,
            collect_interval_seconds: 60,
            ai_decision_interval_minutes: 30,
            llm_base_url: "https://api.deepseek.com".into(),
            llm_api_key: String::new(),
            llm_model: "deepseek-chat".into(),
            hithink_api_key: String::new(),
            admin_key: String::new(),
        }
    }
}

impl Config {
    /// 从环境变量加载（配合 .env）。
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            data_dir: env_str("FINBOX_DATA", &Self::default().data_dir),
            initial_capital: env_float("INITIAL_CAPITAL", Self::default().initial_capital),
            watchlist: env_str("WATCHLIST", "")
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            candidate_count: env_u64("CANDIDATE_COUNT", Self::default().candidate_count as u64) as usize,
            collect_interval_seconds: env_u64("COLLECT_INTERVAL_SECONDS", Self::default().collect_interval_seconds),
            ai_decision_interval_minutes: env_u64("AI_DECISION_INTERVAL_MINUTES", Self::default().ai_decision_interval_minutes),
            llm_base_url: env_str("LLM_BASE_URL", &Self::default().llm_base_url),
            llm_api_key: env_str("LLM_API_KEY", &Self::default().llm_api_key),
            llm_model: env_str("LLM_MODEL", &Self::default().llm_model),
            hithink_api_key: env_str("HITHINK_FINANCE_API_KEY", &Self::default().hithink_api_key),
            admin_key: env_str("ADMIN_KEY", &Self::default().admin_key),
        })
    }
}

fn env_str(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_float(key: &str, default: f64) -> f64 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}
