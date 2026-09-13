pub mod anthropic;
pub mod anthropic_types;
pub mod openai_compat;
pub mod openai_types;
pub mod sse;

use crate::error::Result;
use crate::message::{Message, Usage};
use crate::tools::ToolDefinition;
use futures::stream::BoxStream;
use std::future::Future;

pub type StreamResult = BoxStream<'static, Result<ProviderEvent>>;

#[derive(Debug, Clone)]
pub struct RequestContext {
    pub system: String,
    pub tools: Vec<ToolDefinition>,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProviderEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolUseStart { id: String, name: String },
    ToolUseDelta { id: String, partial_json: String },
    ToolUseComplete { id: String, name: String, input: serde_json::Value },
    Completed { usage: Usage },
}

/// DeepSeek 官方错误码语义(Anthropic 端点的 4xx/5xx 语义兼容):
/// 429/500/503 可重试,400/401/422 为请求问题,402 为账户问题。
fn is_retryable_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 503)
}

fn status_hint(status: u16) -> &'static str {
    match status {
        400 => "请求体格式错误,请检查配置或参数",
        401 => "认证失败:API Key 无效或缺失",
        402 => "账户余额不足,请充值后重试",
        422 => "请求参数错误",
        429 => "请求速率达到上限(TPM/RPM)",
        500 => "服务端内部故障",
        503 => "服务端繁忙,请稍后重试",
        _ => "",
    }
}

/// 从错误体提取干净信息:DeepSeek/Anthropic 均为 {"error":{"message":...}}。
pub(crate) fn provider_error_message(status: u16, body: &str) -> String {
    let hint = status_hint(status);
    let msg = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error")?.get("message")?.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| truncate_body(body).to_string());
    if hint.is_empty() {
        format!("HTTP {status}: {msg}")
    } else {
        format!("HTTP {status}({hint}): {msg}")
    }
}

pub(crate) fn truncate_body(s: &str) -> &str {
    match s.char_indices().nth(500) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

const MAX_RETRIES: u32 = 2;

/// 发送 chat/completions 请求并对可重试错误(429/500/503)自动退避重试。
/// 重试只发生在流开始之前;payload 由调用方预序列化,保证每次重试字节一致。
/// `send` 每次调用都会重新构造请求(闭包内 clone 连接参数)。
pub(crate) async fn post_stream_with_retry<F, Fut>(mut send: F) -> Result<reqwest::Response>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = reqwest::Result<reqwest::Response>>,
{
    let mut attempt: u32 = 0;
    loop {
        let resp = send().await.map_err(crate::error::LexError::Http)?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(resp);
        }
        let body = resp.text().await.unwrap_or_default();
        if is_retryable_status(status) && attempt < MAX_RETRIES {
            attempt += 1;
            let delay = std::time::Duration::from_secs(if attempt == 1 { 1 } else { 3 });
            tracing::warn!(status, ?delay, attempt, "服务端可重试错误,自动退避重试");
            tokio::time::sleep(delay).await;
            continue;
        }
        return Err(crate::error::LexError::Provider(provider_error_message(status, &body)));
    }
}

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    async fn send(&self, ctx: RequestContext) -> Result<StreamResult>;
}

#[cfg(test)]
mod tests {
    // ProviderEvent 必须可 Debug/Clone,占位断言在 anthropic_types 内
}
