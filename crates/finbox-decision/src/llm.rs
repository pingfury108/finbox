//! LLM 调用（pi-ai 统一多 Provider 层）与输出解析。
//!
//! 用 pi-ai 替代手写 HTTP：内置指数退避重试、流式、多 Provider 可切换、
//! token 用量统计。决策输出仍是 JSON 文本（由提示词约束 + parse 容错）。

use finbox_core::{OrderIntent, OrderSide};
use serde::{Deserialize, Serialize};

use crate::{DecisionError, LlmConfig};

/// LLM 动作条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub action: String,
    pub symbol: String,
    #[serde(default)]
    pub quantity: Option<u32>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// LLM 解析结果。
#[derive(Debug, Clone)]
pub struct ParsedOutput {
    pub actions: Vec<Action>,
    pub actions_json: String,
    pub comment: String,
}

/// 调用 chat completion（pi-ai）。`system` 为系统提示词（按账户档位选择）。
pub async fn chat(config: &LlmConfig, user: &str, system: &str) -> Result<String, DecisionError> {
    use futures::StreamExt;
    use pi_ai::{AssistantContent, AssistantMessageEvent, Context, LlmApi, Message, MessageContent, Model, Modality, OpenAICompletionsApi, StreamOptions, UserMessage};

    let api = OpenAICompletionsApi::with_http(
        reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .map_err(|e| DecisionError::Llm(e.to_string()))?,
    );
    // pi-ai 的 URL = {base_url}/chat/completions；主流兼容端点在 /v1 下，
    // 配置里没写 /v1 的自动补上（写了的尊重配置）
    let base = config.base_url.trim_end_matches('/').to_string();
    let base_url = if base.ends_with("/v1") { base } else { format!("{base}/v1") };

    let model = Model {
        id: config.model.clone(),
        name: config.model.clone(),
        api: "openai-completions".into(),
        provider: "custom".into(),
        base_url,
        reasoning: false,
        input: vec![Modality::Text],
        cost: Default::default(),
        context_window: 128_000,
        max_tokens: 8_192,
        sampling_params: None,
        headers: None,
        compat: None,
    };
    let ctx = Context {
        system_prompt: Some(system.to_string()),
        messages: vec![Message::User(UserMessage::new(MessageContent::text(user)))],
        tools: None,
    };
    let opts = StreamOptions {
        api_key: Some(config.api_key.clone()),
        temperature: Some(0.2),
        ..Default::default()
    };

    let mut stream = api.stream(&model, &ctx, opts);
    let mut text = String::new();
    let mut final_text = String::new();
    let mut err_msg: Option<String> = None;
    while let Some(ev) = stream.next().await {
        match ev {
            AssistantMessageEvent::TextDelta { delta, .. } => text.push_str(&delta),
            AssistantMessageEvent::Done { message, .. } => {
                for c in &message.content {
                    if let AssistantContent::Text(t) = c {
                        final_text.push_str(&t.text);
                    }
                }
            }
            AssistantMessageEvent::Error { error, .. } => {
                err_msg = Some(error.error_message.clone().unwrap_or_else(|| "LLM 流错误".into()));
            }
            _ => {}
        }
    }
    if let Some(e) = err_msg {
        return Err(DecisionError::Llm(e));
    }
    // 少数 Provider 全文只在 Done 里：TextDelta 为空时回退
    let out = if text.is_empty() { final_text } else { text };
    if out.trim().is_empty() {
        return Err(DecisionError::Llm("空回复".into()));
    }
    Ok(out)
}

/// 解析 LLM 原始输出（容忍 ```json 包裹）。
pub fn parse(raw: &str) -> Result<ParsedOutput, DecisionError> {
    let text = raw.trim();
    let text = text
        .strip_prefix("```")
        .map(|t| t.strip_suffix("```").unwrap_or(t))
        .unwrap_or(text)
        .trim();
    let text = text.strip_prefix("json").map(str::trim).unwrap_or(text);

    #[derive(Deserialize)]
    struct Outer {
        #[serde(default)]
        actions: Vec<Action>,
        #[serde(default)]
        comment: String,
    }
    let v: Outer = serde_json::from_str(text).map_err(|e| DecisionError::Parse(e.to_string()))?;
    let actions_json = serde_json::to_string(&v.actions).unwrap_or_else(|_| "[]".into());
    Ok(ParsedOutput { actions: v.actions, actions_json, comment: v.comment })
}

/// 动作 → 委托意图（过滤 hold）。thscode 统一规范化（LLM 可能给纯代码）。
pub fn to_intents(actions: &[Action]) -> Vec<OrderIntent> {
    let mut out = Vec::new();
    for a in actions {
        let side = match a.action.as_str() {
            "buy" => OrderSide::Buy,
            "sell" => OrderSide::Sell,
            _ => continue,
        };
        let qty = a.quantity.unwrap_or(0);
        if qty == 0 {
            continue;
        }
        out.push(OrderIntent {
            thscode: finbox_core::rules::normalize_thscode(&a.symbol),
            name: String::new(),
            side,
            quantity: qty,
            decision_id: None,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json() {
        let raw = r#"{"actions":[{"action":"buy","symbol":"600519.SH","quantity":100,"reason":"x"},{"action":"hold","symbol":"000001.SZ","quantity":0}],"comment":"观望"}"#;
        let p = parse(raw).unwrap();
        assert_eq!(p.actions.len(), 2);
        assert_eq!(p.comment, "观望");
        let intents = to_intents(&p.actions);
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].thscode, "600519.SH");
        assert_eq!(intents[0].side, OrderSide::Buy);
        assert_eq!(intents[0].quantity, 100);
    }

    #[test]
    fn parse_with_code_fence() {
        let raw = "```json\n{\"actions\": [], \"comment\": \"空仓等待\"}\n```";
        let p = parse(raw).unwrap();
        assert!(p.actions.is_empty());
    }
}
