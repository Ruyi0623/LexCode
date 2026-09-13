pub mod bash_exec;
pub mod file_edit;
pub mod file_read;
pub mod grep_search;
pub mod todo_write;

use crate::error::Result;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct ShellCommand {
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    pub fn parse(s: &str) -> Option<TodoStatus> {
        match s {
            "pending" => Some(TodoStatus::Pending),
            "in_progress" => Some(TodoStatus::InProgress),
            "completed" => Some(TodoStatus::Completed),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Completed => "completed",
        }
    }

    fn checkbox(&self) -> char {
        match self {
            TodoStatus::Pending => '☐',
            TodoStatus::InProgress => '◐',
            TodoStatus::Completed => '☑',
        }
    }
}

#[derive(Debug, Clone)]
pub struct Todo {
    pub content: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Default)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub shell: Option<ShellCommand>,
    /// 会话级待办清单(todo_write 的存储;不落盘,生命周期同 AgentLoop)
    pub todos: Arc<Mutex<Vec<Todo>>>,
}

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn schema(&self) -> Value;
    fn read_only(&self) -> bool;
    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<String>;
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        ToolRegistry { tools: Vec::new() }
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.iter().find(|t| t.name() == name).map(|t| t.as_ref())
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .map(|t| ToolDefinition { name: t.name().to_string(), description: t.description().to_string(), input_schema: t.schema() })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::file_read::FileRead;
    use super::*;
    use serde_json::json;

    struct Dummy;
    #[async_trait::async_trait]
    impl Tool for Dummy {
        fn name(&self) -> &str { "dummy" }
        fn description(&self) -> &str { "d" }
        fn schema(&self) -> serde_json::Value { json!({"type":"object"}) }
        fn read_only(&self) -> bool { true }
        async fn execute(&self, _input: serde_json::Value, _ctx: &ToolContext) -> crate::error::Result<String> { Ok("ok".into()) }
    }

    #[test]
    fn registry_definitions_preserve_insertion_order() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(Dummy));
        reg.register(Box::new(FileRead));
        let defs = reg.definitions();
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].name, "dummy");
        assert_eq!(defs[1].name, "file_read");
        assert!(reg.get("file_read").is_some());
        assert!(reg.get("nope").is_none());
    }
}
