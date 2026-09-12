use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text { text: String },
    Thinking { reasoning_content: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String, is_error: bool },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Block>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Message { role: Role::User, content: vec![Block::Text { text: text.into() }] }
    }

    pub fn assistant(content: Vec<Block>) -> Self {
        Message { role: Role::Assistant, content }
    }

    pub fn tool_results(results: Vec<(&str, String, bool)>) -> Self {
        Message {
            role: Role::User,
            content: results
                .into_iter()
                .map(|(id, content, is_error)| Block::ToolResult { tool_use_id: id.to_string(), content, is_error })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_use_block_serializes_tagged() {
        let b = Block::ToolUse { id: "t1".into(), name: "file_read".into(), input: json!({"path":"a.rs"}) };
        let v = serde_json::to_value(&b).unwrap();
        assert_eq!(v["type"], "tool_use");
        assert_eq!(v["name"], "file_read");
    }

    #[test]
    fn message_field_order_is_role_then_content() {
        let m = Message::user_text("hi");
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.starts_with(r#"{"role":"user","content":"#), "实际: {s}");
    }

    #[test]
    fn tool_results_groups_multiple_blocks() {
        let m = Message::tool_results(vec![
            ("t1", "ok".to_string(), false),
            ("t2", "bad".to_string(), true),
        ]);
        assert_eq!(m.role, Role::User);
        assert_eq!(m.content.len(), 2);
        match &m.content[1] {
            Block::ToolResult { tool_use_id, is_error, .. } => {
                assert_eq!(tool_use_id, "t2");
                assert!(is_error);
            }
            other => panic!("期望 ToolResult,实际 {other:?}"),
        }
    }
}
