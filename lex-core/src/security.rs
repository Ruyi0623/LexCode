use crate::error::Result;
use crate::message::Block;
use crate::tools::{ToolContext, ToolRegistry};
use async_trait::async_trait;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct PendingAction {
    pub tool_name: String,
    pub summary: String,
}

#[async_trait]
pub trait PermissionHandler: Send + Sync {
    /// 返回 true = 放行;false = 用户拒绝。
    async fn confirm(&self, action: &PendingAction) -> Result<bool>;
}

#[async_trait]
impl<T: PermissionHandler + ?Sized> PermissionHandler for std::sync::Arc<T> {
    async fn confirm(&self, action: &PendingAction) -> Result<bool> {
        self.as_ref().confirm(action).await
    }
}

pub fn describe_call(name: &str, input: &Value) -> String {
    match name {
        "bash_exec" => {
            let cmd = input.get("command").and_then(Value::as_str).unwrap_or("<未知命令>");
            format!("执行命令: {cmd}")
        }
        "file_read" => {
            let p = input.get("path").and_then(Value::as_str).unwrap_or("<未知路径>");
            format!("读取文件: {p}")
        }
        "file_edit" => {
            let p = input.get("path").and_then(Value::as_str).unwrap_or("<未知路径>");
            let old = input.get("old_string").and_then(Value::as_str).unwrap_or("");
            let new = input.get("new_string").and_then(Value::as_str).unwrap_or("");
            format!("编辑文件: {p}\n  - 替换前: {old}\n  - 替换后: {new}")
        }
        _ => format!("调用 {name}: {}", serde_json::to_string(input).unwrap_or_else(|_| "<无法序列化>".into())),
    }
}

pub async fn execute_tool_call(
    registry: &ToolRegistry,
    handler: &dyn PermissionHandler,
    ctx: &ToolContext,
    call_id: &str,
    tool_name: &str,
    input: Value,
) -> Block {
    let tool = match registry.get(tool_name) {
        Some(t) => t,
        None => {
            return Block::ToolResult {
                tool_use_id: call_id.to_string(),
                content: format!("未知工具: {tool_name}"),
                is_error: true,
            }
        }
    };

    let action = PendingAction { tool_name: tool_name.to_string(), summary: describe_call(tool_name, &input) };
    match handler.confirm(&action).await {
        Ok(true) => {}
        Ok(false) => {
            return Block::ToolResult {
                tool_use_id: call_id.to_string(),
                content: "用户拒绝执行该操作".into(),
                is_error: true,
            }
        }
        Err(e) => {
            return Block::ToolResult {
                tool_use_id: call_id.to_string(),
                content: format!("权限确认失败: {e}"),
                is_error: true,
            }
        }
    }

    match tool.execute(input, ctx).await {
        Ok(content) => Block::ToolResult { tool_use_id: call_id.to_string(), content, is_error: false },
        Err(e) => Block::ToolResult { tool_use_id: call_id.to_string(), content: format!("{e}"), is_error: true },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Block;
    use crate::tools::{Tool, ToolContext, ToolRegistry};
    use serde_json::json;
    use std::path::PathBuf;

    struct Always;
    #[async_trait::async_trait]
    impl PermissionHandler for Always {
        async fn confirm(&self, _a: &PendingAction) -> crate::error::Result<bool> { Ok(true) }
    }
    struct Never;
    #[async_trait::async_trait]
    impl PermissionHandler for Never {
        async fn confirm(&self, _a: &PendingAction) -> crate::error::Result<bool> { Ok(false) }
    }

    struct Echo;
    #[async_trait::async_trait]
    impl Tool for Echo {
        fn name(&self) -> &str { "echo" }
        fn description(&self) -> &str { "e" }
        fn schema(&self) -> serde_json::Value { json!({"type":"object"}) }
        fn read_only(&self) -> bool { false }
        async fn execute(&self, input: Value, _ctx: &ToolContext) -> crate::error::Result<String> {
            Ok(format!("echo:{}", input.get("x").and_then(Value::as_str).unwrap_or("")))
        }
    }
    struct Boom;
    #[async_trait::async_trait]
    impl Tool for Boom {
        fn name(&self) -> &str { "boom" }
        fn description(&self) -> &str { "b" }
        fn schema(&self) -> serde_json::Value { json!({"type":"object"}) }
        fn read_only(&self) -> bool { false }
        async fn execute(&self, _i: Value, _c: &ToolContext) -> crate::error::Result<String> {
            Err(crate::error::LexError::Tool("炸了".into()))
        }
    }

    fn ctx() -> ToolContext { ToolContext { cwd: PathBuf::from("."), shell: None } }

    #[tokio::test]
    async fn approved_call_returns_tool_result() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(Echo));
        let block = execute_tool_call(&reg, &Always, &ctx(), "t1", "echo", json!({"x":"hi"})).await;
        match block {
            Block::ToolResult { tool_use_id, content, is_error } => {
                assert_eq!(tool_use_id, "t1");
                assert_eq!(content, "echo:hi");
                assert!(!is_error);
            }
            o => panic!("{o:?}"),
        }
    }

    #[tokio::test]
    async fn denied_call_becomes_error_result_not_abort() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(Echo));
        let block = execute_tool_call(&reg, &Never, &ctx(), "t2", "echo", json!({"x":"hi"})).await;
        match block {
            Block::ToolResult { content, is_error, .. } => {
                assert!(is_error);
                assert!(content.contains("拒绝"));
            }
            o => panic!("{o:?}"),
        }
    }

    #[tokio::test]
    async fn tool_failure_becomes_error_result() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(Boom));
        let block = execute_tool_call(&reg, &Always, &ctx(), "t3", "boom", json!({})).await;
        match block {
            Block::ToolResult { content, is_error, .. } => {
                assert!(is_error);
                assert!(content.contains("炸了"));
            }
            o => panic!("{o:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_tool_is_error_result() {
        let reg = ToolRegistry::new();
        let block = execute_tool_call(&reg, &Always, &ctx(), "t4", "ghost", json!({})).await;
        match block {
            Block::ToolResult { content, is_error, .. } => {
                assert!(is_error);
                assert!(content.contains("ghost"));
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn describe_call_shows_concrete_content() {
        let s = describe_call("bash_exec", &json!({"command":"cargo test"}));
        assert!(s.contains("cargo test"));
        let s2 = describe_call("file_edit", &json!({"path":"a.rs","old_string":"x","new_string":"y"}));
        assert!(s2.contains("a.rs") && s2.contains("x") && s2.contains("y"));
        let s3 = describe_call("mystery", &json!({"k":1}));
        assert!(s3.contains("k"));
    }
}
