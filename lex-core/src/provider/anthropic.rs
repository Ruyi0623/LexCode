use super::anthropic_types::{build_request, AnthropicRequest};
use super::sse;
use super::{post_stream_with_retry, Provider, ProviderEvent, RequestContext, StreamResult};
use crate::error::{LexError, Result};
use crate::message::Usage;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

pub struct AnthropicProvider {
    http: reqwest::Client,
    base_url: String,
    model: String,
    max_tokens: u32,
    api_key: String,
}

const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Deserialize)]
struct SseFrame {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    index: Option<u64>,
    #[serde(default)]
    delta: Option<Value>,
    #[serde(default)]
    content_block: Option<Value>,
    #[serde(default)]
    message: Option<Value>,
    #[serde(default)]
    usage: Option<Value>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Default)]
struct ToolAcc {
    id: String,
    name: String,
    json: String,
}

impl AnthropicProvider {
    pub fn new(http: reqwest::Client, base_url: String, model: String, max_tokens: u32, api_key: String) -> Self {
        AnthropicProvider { http, base_url, model, max_tokens, api_key }
    }

    fn endpoint(&self) -> String {
        format!("{}/v1/messages", self.base_url.trim_end_matches('/'))
    }

    /// 便捷构造:内部建 reqwest::Client(lex-cli 不直接依赖 reqwest)。
    pub fn with_defaults(base_url: String, model: String, max_tokens: u32, api_key: String) -> Result<Self> {
        let http = reqwest::Client::builder().build()?;
        Ok(AnthropicProvider::new(http, base_url, model, max_tokens, api_key))
    }
}

#[async_trait::async_trait]
impl Provider for AnthropicProvider {
    async fn send(&self, ctx: RequestContext) -> Result<StreamResult> {
        let req: AnthropicRequest = build_request(&ctx, &self.model, self.max_tokens);
        // 预序列化:每次重试发出的字节完全一致(前缀缓存确定性)
        let payload = serde_json::to_vec(&req)?;
        let url = self.endpoint();
        let http = self.http.clone();
        let api_key = self.api_key.clone();

        let send = {
            let http = http.clone();
            let url = url.clone();
            let api_key = api_key.clone();
            move || {
                http.post(&url)
                    .header("x-api-key", api_key.clone())
                    .header("anthropic-version", ANTHROPIC_VERSION)
                    .header("Content-Type", "application/json")
                    .body(payload.clone())
                    .send()
            }
        };
        let resp = post_stream_with_retry(send).await?;

        let stream = async_stream::stream! {
            let mut bytes = resp.bytes_stream();
            let mut pending: Vec<u8> = Vec::new();
            let mut usage = Usage::default();
            let mut tools: BTreeMap<u64, ToolAcc> = BTreeMap::new();

            loop {
                let chunk = match bytes.next().await {
                    Some(Ok(c)) => c,
                    Some(Err(e)) => { yield Err(LexError::Http(e)); return; }
                    None => break,
                };
                pending.extend_from_slice(&chunk);
                // 增量 UTF-8 解码:已解码部分是 pending[..valid_len],
                // 只有这段交给 SSE 解析;不完整的多字节尾字节(pending[valid_len..])留给下一个 chunk
                let valid_len = match std::str::from_utf8(&pending) {
                    Ok(_) => pending.len(),
                    Err(e) => e.valid_up_to(),
                };
                let text = std::str::from_utf8(&pending[..valid_len]).unwrap_or("");
                let (events, remainder) = sse::parse(text);
                // 新 pending = SSE 未终结残余(在已解码文本尾部)+ 不完整 UTF-8 尾字节,顺序不能颠倒
                let mut new_pending = remainder.into_bytes();
                new_pending.extend_from_slice(&pending[valid_len..]);
                pending = new_pending;

                for ev in events {
                    let data = &ev.data;
                    if data.is_empty() { continue; }
                    let frame: SseFrame = match serde_json::from_str(data) {
                        Ok(f) => f,
                        Err(e) => { yield Err(LexError::Provider(format!("无法解析 SSE 数据: {e}: {data}"))); return; }
                    };
                    match frame.kind.as_str() {
                        "message_start" => {
                            if let Some(u) = frame.message.as_ref().and_then(|m| m.get("usage")) {
                                usage.input_tokens = u.get("input_tokens").and_then(Value::as_u64).unwrap_or(usage.input_tokens);
                                usage.cache_hit_tokens = u
                                    .get("cache_read_input_tokens")
                                    .and_then(Value::as_u64)
                                    .unwrap_or(usage.cache_hit_tokens);
                            }
                        }
                        "content_block_start" => {
                            let idx = frame.index.unwrap_or(0);
                            if let Some(cb) = frame.content_block {
                                let ty = cb.get("type").and_then(Value::as_str).unwrap_or("");
                                if ty == "tool_use" {
                                    let id = cb.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                                    let name = cb.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                                    tools.insert(idx, ToolAcc { id: id.clone(), name: name.clone(), json: String::new() });
                                    yield Ok(ProviderEvent::ToolUseStart { id, name });
                                }
                            }
                        }
                        "content_block_delta" => {
                            let idx = frame.index.unwrap_or(0);
                            if let Some(d) = frame.delta {
                                match d.get("type").and_then(Value::as_str).unwrap_or("") {
                                    "text_delta" => {
                                        if let Some(t) = d.get("text").and_then(Value::as_str) {
                                            yield Ok(ProviderEvent::TextDelta(t.to_string()));
                                        }
                                    }
                                    "thinking_delta" => {
                                        if let Some(t) = d.get("thinking").and_then(Value::as_str) {
                                            yield Ok(ProviderEvent::ThinkingDelta(t.to_string()));
                                        }
                                    }
                                    "input_json_delta" => {
                                        if let Some(p) = d.get("partial_json").and_then(Value::as_str) {
                                            if let Some(acc) = tools.get_mut(&idx) {
                                                acc.json.push_str(p);
                                                yield Ok(ProviderEvent::ToolUseDelta { id: acc.id.clone(), partial_json: p.to_string() });
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        "content_block_stop" => {
                            let idx = frame.index.unwrap_or(0);
                            if let Some(acc) = tools.remove(&idx) {
                                let input: Value = if acc.json.trim().is_empty() {
                                    Value::Object(serde_json::Map::new())
                                } else {
                                    match serde_json::from_str(&acc.json) {
                                        Ok(v) => v,
                                        Err(e) => { yield Err(LexError::Provider(format!("tool_use JSON 不完整: {e}"))); return; }
                                    }
                                };
                                yield Ok(ProviderEvent::ToolUseComplete { id: acc.id.clone(), name: acc.name.clone(), input });
                            }
                        }
                        "message_delta" => {
                            if let Some(u) = frame.usage.as_ref() {
                                usage.output_tokens = u.get("output_tokens").and_then(Value::as_u64).unwrap_or(usage.output_tokens);
                            }
                        }
                        "message_stop" => {}
                        "ping" => {}
                        "error" => {
                            let msg = frame.error
                                .and_then(|e| e.get("message").and_then(Value::as_str).map(|s| s.to_string()))
                                .unwrap_or_else(|| "未知 provider 错误".into());
                            yield Err(LexError::Provider(msg));
                            return;
                        }
                        _ => {}
                    }
                }
            }
            if !pending.is_empty() {
                tracing::warn!("SSE 流结束时仍有 {} 字节未解析残余", pending.len());
            }
            yield Ok(ProviderEvent::Completed { usage });
        };

        Ok(Box::pin(stream))
    }
}

