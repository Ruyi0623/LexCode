use crate::error::{LexError, Result};
use crate::tools::{Todo, TodoStatus, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::Value;

/// 会话待办清单:整体替换式写入(与主流编码 agent 的 todo 语义一致)。
/// 只操作会话内存状态,不改用户文件系统,因此视为只读、免确认、可并发。
pub struct TodoWrite;

fn format_todos(todos: &[Todo]) -> String {
    if todos.is_empty() {
        return "(待办清单已清空)".into();
    }
    let mut out = Vec::with_capacity(todos.len());
    for t in todos {
        out.push(format!("{} [{}] {}", t.status.checkbox(), t.status.as_str(), t.content));
    }
    out.join("\n")
}

#[async_trait]
impl Tool for TodoWrite {
    fn name(&self) -> &str {
        "todo_write"
    }
    fn description(&self) -> &str {
        "更新当前任务的待办清单(整体替换)。任务涉及多个步骤时应先用它列计划,完成后逐项标记状态。参数:todos 数组,每项含 content(待办内容)与 status(pending / in_progress / completed)"
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "完整的新待办列表",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": {"type": "string"},
                            "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}
                        },
                        "required": ["content", "status"]
                    }
                }
            },
            "required": ["todos"]
        })
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<String> {
        let arr = input
            .get("todos")
            .and_then(Value::as_array)
            .ok_or_else(|| LexError::Tool("todo_write 缺少必填参数 \"todos\"(数组)".into()))?;

        let mut todos: Vec<Todo> = Vec::with_capacity(arr.len());
        for (i, item) in arr.iter().enumerate() {
            let content = item
                .get("content")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| LexError::Tool(format!("todos[{i}] 缺少非空 content")))?;
            let status_raw = item
                .get("status")
                .and_then(Value::as_str)
                .ok_or_else(|| LexError::Tool(format!("todos[{i}] 缺少 status(pending/in_progress/completed)")))?;
            let status = TodoStatus::parse(status_raw)
                .ok_or_else(|| LexError::Tool(format!("todos[{i}] status \"{status_raw}\" 无效:只支持 pending/in_progress/completed")))?;
            todos.push(Todo { content: content.to_string(), status });
        }

        let mut guard = ctx.todos.lock().map_err(|_| LexError::Tool("待办清单状态被污染".into()))?;
        *guard = todos;
        Ok(format_todos(&guard))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolContext;
    use serde_json::json;
    use std::path::PathBuf;

    fn ctx() -> ToolContext {
        ToolContext { cwd: PathBuf::from("."), shell: None, todos: Default::default(), spawner: None }
    }

    #[tokio::test]
    async fn writes_and_renders_checklist() {
        let c = ctx();
        let out = TodoWrite
            .execute(
                json!({"todos": [
                    {"content": "读文件", "status": "completed"},
                    {"content": "改代码", "status": "in_progress"},
                    {"content": "跑测试", "status": "pending"}
                ]}),
                &c,
            )
            .await
            .unwrap();
        assert!(out.contains("☑ [completed] 读文件"), "实际: {out}");
        assert!(out.contains("◐ [in_progress] 改代码"), "实际: {out}");
        assert!(out.contains("☐ [pending] 跑测试"), "实际: {out}");
    }

    #[tokio::test]
    async fn state_persists_in_context() {
        let c = ctx();
        TodoWrite
            .execute(json!({"todos": [{"content": "唯一一步", "status": "in_progress"}]}), &c)
            .await
            .unwrap();
        let guard = c.todos.lock().unwrap();
        assert_eq!(guard.len(), 1);
        assert_eq!(guard[0].content, "唯一一步");
        assert_eq!(guard[0].status, TodoStatus::InProgress);
    }

    #[tokio::test]
    async fn whole_list_is_replaced() {
        let c = ctx();
        TodoWrite.execute(json!({"todos": [{"content": "a", "status": "pending"}]}), &c).await.unwrap();
        TodoWrite.execute(json!({"todos": [{"content": "b", "status": "pending"}]}), &c).await.unwrap();
        let guard = c.todos.lock().unwrap();
        assert_eq!(guard.len(), 1);
        assert_eq!(guard[0].content, "b");
    }

    #[tokio::test]
    async fn invalid_status_is_error() {
        let err = TodoWrite
            .execute(json!({"todos": [{"content": "x", "status": "done"}]}), &ctx())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("status"), "实际: {err}");
    }

    #[tokio::test]
    async fn missing_todos_is_error() {
        let err = TodoWrite.execute(json!({}), &ctx()).await.unwrap_err();
        assert!(err.to_string().contains("todos"), "实际: {err}");
    }

    #[tokio::test]
    async fn empty_list_clears() {
        let c = ctx();
        TodoWrite.execute(json!({"todos": [{"content": "a", "status": "pending"}]}), &c).await.unwrap();
        let out = TodoWrite.execute(json!({"todos": []}), &c).await.unwrap();
        assert!(out.contains("已清空"), "实际: {out}");
        assert!(c.todos.lock().unwrap().is_empty());
    }

    #[test]
    fn is_read_only() {
        assert!(TodoWrite.read_only());
    }
}
