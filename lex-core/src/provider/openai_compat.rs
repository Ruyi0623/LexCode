use super::openai_types::{build_request, OpenAiParams};
use super::sse;
use super::{post_stream_with_retry, Provider, ProviderEvent, RequestContext, StreamResult};
use crate::error::{LexError, Result};
use crate::message::Usage;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

pub struct OpenAiCompatProvider {
    http: reqwest::Client,
    base_url: String,
    model: String,
    params: OpenAiParams,
    api_key: String,
}

#[derive(Deserialize)]
struct OpenAiChunk {
    #[serde(default)]
    choices: Vec<OpenAiChoice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    #[serde(default)]
    delta: Value,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    /// DeepSeek 上下文硬盘缓存统计(默认开启,无需显式标记)
    #[serde(default)]
    prompt_cache_hit_tokens: u64,
    #[serde(default)]
    prompt_cache_miss_tokens: u64,
    /// 兼容 OpenAI 风格字段:与 prompt_cache_hit_tokens 同值
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Deserialize)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: u64,
}

#[derive(Default)]
struct ToolAcc {
    id: String,
    name: String,
    json: String,
}

impl OpenAiCompatProvider {
    pub fn new(http: reqwest::Client, base_url: String, model: String, params: OpenAiParams, api_key: String) -> Self {
        OpenAiCompatProvider { http, base_url, model, params, api_key }
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// 便捷构造:内部建 reqwest::Client(lex-cli 不直接依赖 reqwest)。
    /// 只设连接超时:TCP 连不上时快速失败;不设读取超时,SSE 长流不能被总超时截断。
    pub fn with_defaults(base_url: String, model: String, params: OpenAiParams, api_key: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(super::CONNECT_TIMEOUT_SECS))
            .build()?;
        Ok(OpenAiCompatProvider::new(http, base_url, model, params, api_key))
    }
}

#[async_trait::async_trait]
impl Provider for OpenAiCompatProvider {
    async fn send(&self, ctx: RequestContext) -> Result<StreamResult> {
        let req = build_request(&ctx, &self.model, &self.params);
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
                    .header("Authorization", format!("Bearer {api_key}"))
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
            let mut done = false;

            loop {
                let chunk = match bytes.next().await {
                    Some(Ok(c)) => c,
                    Some(Err(e)) => { yield Err(LexError::Http(e)); return; }
                    None => break,
                };
                pending.extend_from_slice(&chunk);
                // 增量 UTF-8 解码:已解码部分是 pending[..valid_len],
                // 只有这段交给 SSE 解析;不完整的多字节尾字节留给下一个 chunk
                let valid_len = match std::str::from_utf8(&pending) {
                    Ok(_) => pending.len(),
                    Err(e) => e.valid_up_to(),
                };
                let text = std::str::from_utf8(&pending[..valid_len]).unwrap_or("");
                let (events, remainder) = sse::parse(text);
                // 新 pending = SSE 未终结残余 + 不完整 UTF-8 尾字节,顺序不能颠倒
                let mut new_pending = remainder.into_bytes();
                new_pending.extend_from_slice(&pending[valid_len..]);
                pending = new_pending;

                for ev in events {
                    let data = ev.data.trim();
                    if data.is_empty() { continue; }
                    if data == "[DONE]" { done = true; break; }
                    let chunk: OpenAiChunk = match serde_json::from_str(data) {
                        Ok(c) => c,
                        Err(e) => { yield Err(LexError::Provider(format!("无法解析 SSE 数据: {e}: {data}"))); return; }
                    };
                    if let Some(err) = chunk.error {
                        let msg = err.get("message").and_then(Value::as_str).map(|s| s.to_string())
                            .unwrap_or_else(|| format!("{err}"));
                        yield Err(LexError::Provider(msg));
                        return;
                    }
                    if let Some(u) = chunk.usage {
                        usage.input_tokens = u.prompt_tokens;
                        usage.output_tokens = u.completion_tokens;
                        usage.cache_hit_tokens = if u.prompt_cache_hit_tokens > 0 {
                            u.prompt_cache_hit_tokens
                        } else {
                            u.prompt_tokens_details.map(|d| d.cached_tokens).unwrap_or(0)
                        };
                        usage.cache_miss_tokens = u.prompt_cache_miss_tokens;
                        tracing::info!(
                            input = usage.input_tokens,
                            output = usage.output_tokens,
                            cache_hit = usage.cache_hit_tokens,
                            cache_miss = usage.cache_miss_tokens,
                            "usage 与上下文缓存命中统计"
                        );
                    }
                    for choice in &chunk.choices {
                        let delta = &choice.delta;
                        if let Some(rc) = delta.get("reasoning_content").and_then(Value::as_str) {
                            if !rc.is_empty() {
                                yield Ok(ProviderEvent::ThinkingDelta(rc.to_string()));
                            }
                        }
                        if let Some(t) = delta.get("content").and_then(Value::as_str) {
                            if !t.is_empty() {
                                yield Ok(ProviderEvent::TextDelta(t.to_string()));
                            }
                        }
                        if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) {
                            for tc in tcs {
                                let idx = tc.get("index").and_then(Value::as_u64).unwrap_or(0);
                                if let Some(id) = tc.get("id").and_then(Value::as_str) {
                                    let name = tc.get("function").and_then(|f| f.get("name")).and_then(Value::as_str).unwrap_or("").to_string();
                                    tools.entry(idx).or_insert_with(|| ToolAcc { id: id.to_string(), name: name.clone(), json: String::new() });
                                    yield Ok(ProviderEvent::ToolUseStart { id: id.to_string(), name: name.clone() });
                                }
                                if let Some(f) = tc.get("function") {
                                    if let Some(n) = f.get("name").and_then(Value::as_str) {
                                        if let Some(acc) = tools.get_mut(&idx) {
                                            acc.name = n.to_string();
                                        }
                                    }
                                    if let Some(a) = f.get("arguments").and_then(Value::as_str) {
                                        if !a.is_empty() {
                                            if let Some(acc) = tools.get_mut(&idx) {
                                                acc.json.push_str(a);
                                                yield Ok(ProviderEvent::ToolUseDelta { id: acc.id.clone(), partial_json: a.to_string() });
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(f) = &choice.finish_reason {
                            if f != "stop" && f != "tool_calls" {
                                // 文档取值:content_filter / insufficient_system_resource / aborted 等,
                                // 均属非正常收尾,提醒上层但不中断(工具结果回填路径仍可用)
                                tracing::warn!(finish_reason = f, "流式响应异常收尾");
                            }
                        }
                        if choice.finish_reason.is_some() && !tools.is_empty() {
                            // finish_reason 到达:按 index 顺序落盘所有已累积 tool_call
                            for (_, acc) in std::mem::take(&mut tools) {
                                let input: Value = if acc.json.trim().is_empty() {
                                    Value::Object(serde_json::Map::new())
                                } else {
                                    match serde_json::from_str(&acc.json) {
                                        Ok(v) => v,
                                        Err(e) => { yield Err(LexError::Provider(format!("tool_call JSON 不完整: {e}"))); return; }
                                    }
                                };
                                yield Ok(ProviderEvent::ToolUseComplete { id: acc.id.clone(), name: acc.name.clone(), input });
                            }
                        }
                    }
                }
                if done { break; }
            }
            if !pending.is_empty() {
                tracing::warn!("SSE 流结束时仍有 {} 字节未解析残余", pending.len());
            }
            yield Ok(ProviderEvent::Completed { usage });
        };

        Ok(Box::pin(stream))
    }
}
