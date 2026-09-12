use crate::message::{Block, Message};
use crate::provider::RequestContext;
use crate::tools::ToolDefinition;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct AnthropicRequest {
    pub model: String,
    pub max_tokens: u32,
    pub system: String,
    pub tools: Vec<AnthropicTool>,
    pub messages: Vec<AnthropicMessage>,
    pub stream: bool,
}

#[derive(Debug, Serialize, Clone)]
pub struct AnthropicTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Serialize, Clone)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: Vec<AnthropicBlock>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicBlock {
    Text { text: String },
    Thinking { thinking: String, signature: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
}

fn map_block(b: &Block) -> Option<AnthropicBlock> {
    match b {
        Block::Text { text } => Some(AnthropicBlock::Text { text: text.clone() }),
        // DeepSeek 的 Anthropic 兼容端点要求推理历史把 thinking 块原样回传,
        // 否则报 "thinking must be passed back";空 signature 可被其接受。
        Block::Thinking { reasoning_content } => Some(AnthropicBlock::Thinking {
            thinking: reasoning_content.clone(),
            signature: String::new(),
        }),
        Block::ToolUse { id, name, input } => Some(AnthropicBlock::ToolUse {
            id: id.clone(),
            name: name.clone(),
            input: input.clone(),
        }),
        Block::ToolResult { tool_use_id, content, is_error } => Some(AnthropicBlock::ToolResult {
            tool_use_id: tool_use_id.clone(),
            content: content.clone(),
            is_error: *is_error,
        }),
    }
}

fn map_message(m: &Message) -> Option<AnthropicMessage> {
    let content: Vec<AnthropicBlock> = m.content.iter().filter_map(map_block).collect();
    if content.is_empty() {
        return None;
    }
    let role = match m.role {
        crate::message::Role::User => "user",
        crate::message::Role::Assistant => "assistant",
    };
    Some(AnthropicMessage { role: role.to_string(), content })
}

pub fn build_request(ctx: &RequestContext, model: &str, max_tokens: u32) -> AnthropicRequest {
    AnthropicRequest {
        model: model.to_string(),
        max_tokens,
        system: ctx.system.clone(),
        tools: ctx
            .tools
            .iter()
            .map(|t: &ToolDefinition| AnthropicTool {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.input_schema.clone(),
            })
            .collect(),
        messages: ctx.messages.iter().filter_map(map_message).collect(),
        stream: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Block, Message};

    #[test]
    fn request_field_order_is_exact() {
        let req = AnthropicRequest {
            model: "m".into(),
            max_tokens: 8,
            system: "sys".into(),
            tools: vec![],
            messages: vec![],
            stream: true,
        };
        let s = serde_json::to_string(&req).unwrap();
        assert_eq!(
            s,
            r#"{"model":"m","max_tokens":8,"system":"sys","tools":[],"messages":[],"stream":true}"#
        );
    }

    #[test]
    fn build_request_maps_blocks() {
        let ctx = RequestContext {
            system: "s".into(),
            tools: vec![],
            messages: vec![
                Message::user_text("read it"),
                Message::assistant(vec![
                    Block::Text { text: "ok".into() },
                    Block::ToolUse { id: "t1".into(), name: "file_read".into(), input: serde_json::json!({"path":"a"}) },
                ]),
                Message::tool_results(vec![("t1", "content".into(), false)]),
            ],
        };
        let req = build_request(&ctx, "model-x", 100);
        assert_eq!(req.messages.len(), 3);
        assert_eq!(req.messages[0].role, "user");
        assert_eq!(req.messages[1].role, "assistant");
        assert_eq!(req.messages[2].role, "user");
        let s = serde_json::to_string(&req.messages[1].content).unwrap();
        assert!(s.contains(r#""type":"tool_use""#));
        // DeepSeek 兼容端点要求 thinking 原样回传,必须进入 payload
        let ctx2 = RequestContext {
            system: String::new(),
            tools: vec![],
            messages: vec![Message::assistant(vec![Block::Thinking { reasoning_content: "r".into() }])],
        };
        let req2 = build_request(&ctx2, "m", 1);
        assert_eq!(req2.messages.len(), 1);
        let s2 = serde_json::to_string(&req2.messages[0].content).unwrap();
        assert_eq!(s2, r#"[{"type":"thinking","thinking":"r","signature":""}]"#);
    }
}
