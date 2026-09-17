pub mod rules;

pub use rules::{Decision, SecurityGuard, SecurityRules};

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
        "grep_search" => {
            let p = input.get("pattern").and_then(Value::as_str).unwrap_or("<未知模式>");
            format!("搜索内容: {p}")
        }
        "todo_write" => "更新待办清单".to_string(),
        "spawn_subagent" => {
            let t = input.get("task").and_then(Value::as_str).unwrap_or("<未知任务>");
            format!("派生子 agent 执行子任务: {t}")
        }
        _ => format!("调用 {name}: {}", serde_json::to_string(input).unwrap_or_else(|_| "<无法序列化>".into())),
    }
}

/// 规则匹配的判定主体:bash 取命令、文件类取路径,其余为空。
fn decision_subject(tool_name: &str, input: &Value) -> String {
    match tool_name {
        "bash_exec" => input.get("command").and_then(Value::as_str).unwrap_or("").to_string(),
        "file_read" | "file_edit" => input.get("path").and_then(Value::as_str).unwrap_or("").to_string(),
        _ => String::new(),
    }
}

/// 敏感文件读取记录:file_read / file_edit 都会触碰文件内容。
fn note_read(guard: &SecurityGuard, tool_name: &str, input: &Value) {
    if matches!(tool_name, "file_read" | "file_edit") {
        if let Some(p) = input.get("path").and_then(Value::as_str) {
            guard.note_file_read(p);
        }
    }
}

/// 统一执行入口:权限检查在路径内部,上层不可绕过。
/// 裁决顺序:Forbidden 硬拦截(不询问用户)→ Confirm(询问/白名单)→ Auto(只读直接放行)。
pub async fn execute_tool_call(
    registry: &ToolRegistry,
    handler: &dyn PermissionHandler,
    ctx: &ToolContext,
    guard: &SecurityGuard,
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

    let subject = decision_subject(tool_name, &input);
    match guard.decide(&subject, tool.read_only()) {
        Decision::Forbidden { reason } => {
            tracing::warn!(tool_name, %reason, "工具调用被 Forbidden 规则硬性拦截");
            return Block::ToolResult {
                tool_use_id: call_id.to_string(),
                content: format!("⛔ 操作被安全规则硬性拦截(Forbidden):{reason}。该拦截不接受用户确认,请改用其他方案。"),
                is_error: true,
            };
        }
        Decision::Auto => {}
        Decision::Confirm => {
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
        }
    }
    note_read(guard, tool_name, &input);

    let started = std::time::Instant::now();
    let result = tool.execute(input, ctx).await;
    tracing::info!(
        tool_name,
        duration_ms = started.elapsed().as_millis() as u64,
        is_error = result.is_err(),
        "工具执行完成"
    );
    match result {
        Ok(content) => Block::ToolResult { tool_use_id: call_id.to_string(), content, is_error: false },
        Err(e) => {
            // 工具报错是 agent 常规反馈(如读不存在的文件),降 debug 避免默认级别噪音
            tracing::debug!(tool_name, error = %e, "工具执行失败,已降级为错误 ToolResult 回填模型");
            Block::ToolResult { tool_use_id: call_id.to_string(), content: format!("{e}"), is_error: true }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Block;
    use crate::tools::{Tool, ToolContext, ToolRegistry};
    use rules::{SecurityGuard, SecurityRules};
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

    fn ctx() -> ToolContext { ToolContext { cwd: PathBuf::from("."), shell: None, todos: Default::default(), spawner: None } }
    fn guard() -> SecurityGuard { SecurityGuard::new(SecurityRules::defaults()) }

    #[tokio::test]
    async fn approved_call_returns_tool_result() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(Echo));
        let block = execute_tool_call(&reg, &Always, &ctx(), &guard(), "t1", "echo", json!({"x":"hi"})).await;
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
        let block = execute_tool_call(&reg, &Never, &ctx(), &guard(), "t2", "echo", json!({"x":"hi"})).await;
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
        let block = execute_tool_call(&reg, &Always, &ctx(), &guard(), "t3", "boom", json!({})).await;
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
        let block = execute_tool_call(&reg, &Always, &ctx(), &guard(), "t4", "ghost", json!({})).await;
        match block {
            Block::ToolResult { content, is_error, .. } => {
                assert!(is_error);
                assert!(content.contains("ghost"));
            }
            o => panic!("{o:?}"),
        }
    }

    struct ReadOnly;
    #[async_trait::async_trait]
    impl Tool for ReadOnly {
        fn name(&self) -> &str { "ro" }
        fn description(&self) -> &str { "r" }
        fn schema(&self) -> serde_json::Value { json!({"type":"object"}) }
        fn read_only(&self) -> bool { true }
        async fn execute(&self, _i: Value, _c: &ToolContext) -> crate::error::Result<String> { Ok("只读结果".into()) }
    }

    #[tokio::test]
    async fn read_only_tool_is_auto_even_if_handler_denies() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(ReadOnly));
        let block = execute_tool_call(&reg, &Never, &ctx(), &guard(), "t5", "ro", json!({})).await;
        match block {
            Block::ToolResult { content, is_error, .. } => {
                assert!(!is_error);
                assert_eq!(content, "只读结果");
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
