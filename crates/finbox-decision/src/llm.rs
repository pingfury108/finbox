//! LLM 调用（OpenAI 兼容接口）与输出解析。

use finbox_core::{OrderIntent, OrderSide};
use serde::{Deserialize, Serialize};

use crate::{DecisionError, LlmConfig};

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<Message<'a>>,
    temperature: f64,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: String,
}

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

/// 调用 OpenAI 兼容 chat completion。
pub async fn chat(config: &LlmConfig, user: &str) -> Result<String, DecisionError> {
    let url = format!("{}/v1/chat/completions", config.base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| DecisionError::Llm(e.to_string()))?;

    let body = ChatRequest {
        model: &config.model,
        messages: vec![
            Message { role: "system", content: crate::context::system_prompt() },
            Message { role: "user", content: user },
        ],
        temperature: 0.2,
    };

    let resp = client
        .post(&url)
        .bearer_auth(&config.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| DecisionError::Llm(e.to_string()))?
        .error_for_status()
        .map_err(|e| DecisionError::Llm(e.to_string()))?;

    let parsed: ChatResponse = resp
        .json()
        .await
        .map_err(|e| DecisionError::Parse(e.to_string()))?;
    parsed
        .choices
        .first()
        .map(|c| c.message.content.clone())
        .ok_or_else(|| DecisionError::Parse("无 choices".into()))
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

/// 动作 → 委托意图（过滤 hold）。
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
            thscode: a.symbol.clone(),
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
