use crate::message::{Block, Message};
use crate::provider::RequestContext;
use crate::tools::ToolDefinition;
use serde::Serialize;

/// OpenAI 兼容 chat/completions 请求 payload。
/// 字段顺序 = derive 声明顺序(任务书硬性约束,保证前缀缓存可预测)。
#[derive(Debug, Serialize)]
pub struct OpenAiRequest {
    pub model: String,
    pub messages: Vec<OpenAiMessage>,
    pub tools: Vec<OpenAiTool>,
    pub stream: bool,
    pub stream_options: StreamOptions,
    pub max_tokens: u32,
}

#[derive(Debug, Serialize, Clone)]
pub struct StreamOptions {
    pub include_usage: bool,
}

#[derive(Debug, Serialize, Clone)]
pub struct OpenAiTool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: OpenAiFunction,
}

#[derive(Debug, Serialize, Clone)]
pub struct OpenAiFunction {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// 一条 OpenAI 兼容消息。role 决定哪些可选字段合法:
/// - system/user/assistant:content;assistant 另有 reasoning_content / tool_calls
/// - tool:content + tool_call_id
#[derive(Debug, Serialize, Clone)]
pub struct OpenAiMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct OpenAiToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: OpenAiToolCallFn,
}

#[derive(Debug, Serialize, Clone)]
pub struct OpenAiToolCallFn {
    pub name: String,
    /// JSON 字符串(OpenAI 格式为文本,非结构化对象)
    pub arguments: String,
}

/// ToolResult 的失败标记:OpenAI 兼容格式没有 is_error 字段,
/// 用确定性前缀把失败信息带给模型(Anthropic adapter 有原生 flag,不受影响)。
const ERROR_PREFIX: &str = "[工具执行失败] ";

fn map_tool_call(b: &Block) -> Option<OpenAiToolCall> {
    match b {
        Block::ToolUse { id, name, input } => Some(OpenAiToolCall {
            id: id.clone(),
            kind: "function".into(),
            function: OpenAiToolCallFn {
                name: name.clone(),
                arguments: serde_json::to_string(input).unwrap_or_else(|_| "{}".into()),
            },
        }),
        _ => None,
    }
}

fn map_assistant(m: &Message) -> Option<OpenAiMessage> {
    let mut reasoning: Vec<String> = Vec::new();
    let mut text: Vec<String> = Vec::new();
    let mut tool_calls: Vec<OpenAiToolCall> = Vec::new();
    for b in &m.content {
        match b {
            Block::Thinking { reasoning_content } => reasoning.push(reasoning_content.clone()),
            Block::Text { text: t } => text.push(t.clone()),
            Block::ToolUse { .. } => {
                if let Some(tc) = map_tool_call(b) {
                    tool_calls.push(tc);
                }
            }
            // ToolResult 不该出现在 assistant 消息里,忽略
            Block::ToolResult { .. } => {}
        }
    }
    if reasoning.is_empty() && text.is_empty() && tool_calls.is_empty() {
        return None;
    }
    Some(OpenAiMessage {
        role: "assistant".into(),
        content: if text.is_empty() { None } else { Some(text.join("")) },
        // DeepSeek 硬性要求:历史中的 reasoning_content 必须原样回传,缺失会被 400 拒绝
        reasoning_content: if reasoning.is_empty() { None } else { Some(reasoning.join("")) },
        tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
        tool_call_id: None,
    })
}

fn map_user(m: &Message) -> Vec<OpenAiMessage> {
    let mut out: Vec<OpenAiMessage> = Vec::new();
    let mut text: Vec<String> = Vec::new();
    let mut flush_text = |out: &mut Vec<OpenAiMessage>, text: &mut Vec<String>| {
        if !text.is_empty() {
            out.push(OpenAiMessage {
                role: "user".into(),
                content: Some(text.join("")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            });
            text.clear();
        }
    };
    for b in &m.content {
        match b {
            Block::Text { text: t } => text.push(t.clone()),
            Block::ToolResult { tool_use_id, content, is_error } => {
                // tool 消息与 user 文本不能合在一条消息里:先落盘已积累文本
                flush_text(&mut out, &mut text);
                let body = if *is_error { format!("{ERROR_PREFIX}{content}") } else { content.clone() };
                out.push(OpenAiMessage {
                    role: "tool".into(),
                    content: Some(body),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: Some(tool_use_id.clone()),
                });
            }
            // user 消息里的 Thinking/ToolUse 不合法,忽略
            _ => {}
        }
    }
    flush_text(&mut out, &mut text);
    out
}

pub fn build_request(ctx: &RequestContext, model: &str, max_tokens: u32) -> OpenAiRequest {
    let mut messages: Vec<OpenAiMessage> = Vec::new();
    if !ctx.system.is_empty() {
        messages.push(OpenAiMessage {
            role: "system".into(),
            content: Some(ctx.system.clone()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        });
    }
    for m in &ctx.messages {
        match m.role {
            crate::message::Role::User => messages.extend(map_user(m)),
            crate::message::Role::Assistant => {
                if let Some(am) = map_assistant(m) {
                    messages.push(am);
                }
            }
        }
    }
    OpenAiRequest {
        model: model.to_string(),
        messages,
        tools: ctx
            .tools
            .iter()
            .map(|t: &ToolDefinition| OpenAiTool {
                kind: "function".into(),
                function: OpenAiFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.input_schema.clone(),
                },
            })
            .collect(),
        stream: true,
        stream_options: StreamOptions { include_usage: true },
        max_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Role;

    fn ctx_with(messages: Vec<Message>) -> RequestContext {
        RequestContext { system: "sys".into(), tools: vec![], messages }
    }

    #[test]
    fn request_field_order_is_exact() {
        let req = build_request(&ctx_with(vec![Message::user_text("hi")]), "m", 8);
        let s = serde_json::to_string(&req).unwrap();
        assert_eq!(
            s,
            r#"{"model":"m","messages":[{"role":"system","content":"sys"},{"role":"user","content":"hi"}],"tools":[],"stream":true,"stream_options":{"include_usage":true},"max_tokens":8}"#
        );
    }

    #[test]
    fn tools_map_to_function_format() {
        let ctx = RequestContext {
            system: String::new(),
            tools: vec![ToolDefinition {
                name: "file_read".into(),
                description: "读文件".into(),
                input_schema: serde_json::json!({"type":"object"}),
            }],
            messages: vec![],
        };
        let req = build_request(&ctx, "m", 1);
        let s = serde_json::to_string(&req.tools[0]).unwrap();
        assert_eq!(
            s,
            r#"{"type":"function","function":{"name":"file_read","description":"读文件","parameters":{"type":"object"}}}"#
        );
    }

    /// 任务书要求:一旦历史中出现 tool call,对应 assistant 消息的完整
    /// reasoning_content 必须原样回传,缺失会被 DeepSeek 拒绝(400)。
    #[test]
    fn reasoning_content_is_passed_back_with_tool_calls() {
        let ctx = ctx_with(vec![
            Message::user_text("read it"),
            Message::assistant(vec![
                Block::Thinking { reasoning_content: "需要先读文件".into() },
                Block::Text { text: "好的".into() },
                Block::ToolUse { id: "t1".into(), name: "file_read".into(), input: serde_json::json!({"path":"a.rs"}) },
            ]),
            Message::tool_results(vec![("t1", "content".into(), false)]),
        ]);
        let req = build_request(&ctx, "m", 1);
        assert_eq!(req.messages.len(), 4);
        // [0] 是 system;assistant 消息:reasoning_content + content + tool_calls 三者齐全且顺序稳定
        let s = serde_json::to_string(&req.messages[2]).unwrap();
        assert_eq!(
            s,
            r#"{"role":"assistant","content":"好的","reasoning_content":"需要先读文件","tool_calls":[{"id":"t1","type":"function","function":{"name":"file_read","arguments":"{\"path\":\"a.rs\"}"}}]}"#
        );
        // tool 结果回传为 role:"tool" + tool_call_id
        let s2 = serde_json::to_string(&req.messages[3]).unwrap();
        assert_eq!(s2, r#"{"role":"tool","content":"content","tool_call_id":"t1"}"#);
    }

    #[test]
    fn tool_result_error_is_marked_in_content() {
        let ctx = ctx_with(vec![Message {
            role: Role::User,
            content: vec![Block::ToolResult { tool_use_id: "t1".into(), content: "boom".into(), is_error: true }],
        }]);
        let req = build_request(&ctx, "m", 1);
        let s = serde_json::to_string(&req.messages[1]).unwrap();
        assert_eq!(s, r#"{"role":"tool","content":"[工具执行失败] boom","tool_call_id":"t1"}"#);
    }

    #[test]
    fn mixed_user_message_splits_tool_and_text() {
        let ctx = ctx_with(vec![Message {
            role: Role::User,
            content: vec![
                Block::ToolResult { tool_use_id: "t1".into(), content: "r".into(), is_error: false },
                Block::Text { text: "继续".into() },
            ],
        }]);
        let req = build_request(&ctx, "m", 1);
        assert_eq!(req.messages.len(), 3);
        assert_eq!(req.messages[1].role, "tool");
        assert_eq!(req.messages[2].role, "user");
        assert_eq!(req.messages[2].content.as_deref(), Some("继续"));
    }

    #[test]
    fn empty_system_is_skipped() {
        let ctx = RequestContext { system: String::new(), tools: vec![], messages: vec![Message::user_text("hi")] };
        let req = build_request(&ctx, "m", 1);
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
    }
}
