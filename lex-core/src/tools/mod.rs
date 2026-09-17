pub mod bash_exec;
pub mod file_edit;
pub mod file_read;
pub mod grep_search;
pub mod spawn_subagent;
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

/// 子 agent 派生请求(spawn_subagent 工具输入的规范化形态)
#[derive(Debug, Clone)]
pub struct SubagentRequest {
    pub task: String,
    pub allowed_tools: Vec<String>,
    /// 父级传入的必要上下文片段(并入子 agent 首条任务消息)
    pub context: Option<String>,
    /// 子 agent 上下文 token 上限(缺省继承父级)
    pub context_budget: Option<u32>,
    /// 是否允许子 agent 再派生下一层(默认 false;深度硬上限见 agent/subagent.rs)
    pub allow_nested: bool,
}

/// 子 agent 派生器抽象:由 agent 层实现,工具层只面向该抽象(保持 agent → tools 单向依赖)。
#[async_trait::async_trait]
pub trait SubagentSpawner: Send + Sync {
    /// 执行子任务,返回子 agent 的结构化摘要(绝不返回子 agent 的完整消息历史)。
    async fn spawn(&self, req: SubagentRequest) -> crate::error::Result<String>;
}

#[derive(Clone, Default)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub shell: Option<ShellCommand>,
    /// 会话级待办清单(todo_write 的存储;不落盘,生命周期同 AgentLoop)
    pub todos: Arc<Mutex<Vec<Todo>>>,
    /// 子 agent 派生器;None = 当前环境不允许派生(深度达上限/未装配)
    pub spawner: Option<Arc<dyn SubagentSpawner>>,
}

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn schema(&self) -> Value;
    fn read_only(&self) -> bool;
    /// 是否可与同轮其他工具并发执行。默认与 read_only 一致;
    /// spawn_subagent 覆写为 true(子任务在独立上下文内执行,多个独立子任务允许并发派生)。
    fn parallel_safe(&self) -> bool {
        self.read_only()
    }
    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<String>;
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        ToolRegistry { tools: Vec::new() }
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(Arc::from(tool));
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.iter().find(|t| t.name() == name).map(|t| t.as_ref())
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.iter().map(|t| t.name().to_string()).collect()
    }

    /// 按 allowed 工具名过滤出子注册表(保持插入顺序;工具实例经 Arc 共享,无重复构建)
    pub fn subset(&self, allowed: &[String]) -> ToolRegistry {
        ToolRegistry {
            tools: self
                .tools
                .iter()
                .filter(|t| allowed.iter().any(|a| a == t.name()))
                .cloned()
                .collect(),
        }
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
    fn parallel_safe_defaults_to_read_only() {
        assert!(Dummy.parallel_safe());          // Dummy read_only = true
        assert!(!super::file_edit::FileEdit.parallel_safe()); // FileEdit read_only = false
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

    #[test]
    fn subset_filters_by_name_and_keeps_order() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(Dummy));
        reg.register(Box::new(FileRead));
        let sub = reg.subset(&["file_read".to_string()]);
        let names = sub.names();
        assert_eq!(names, vec!["file_read".to_string()]);
        assert!(sub.get("dummy").is_none());
        // subset 与原注册表互不影响
        assert!(reg.get("dummy").is_some());
    }

    #[test]
    fn names_lists_all_in_insertion_order() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(Dummy));
        reg.register(Box::new(FileRead));
        assert_eq!(reg.names(), vec!["dummy".to_string(), "file_read".to_string()]);
    }

    struct NullSpawner;
    #[async_trait::async_trait]
    impl SubagentSpawner for NullSpawner {
        async fn spawn(&self, _req: SubagentRequest) -> crate::error::Result<String> {
            Ok("子任务摘要".into())
        }
    }

    #[tokio::test]
    async fn tool_context_spawner_defaults_to_none_and_is_callable() {
        let ctx: ToolContext = ToolContext::default();
        assert!(ctx.spawner.is_none());
        let ctx2 = ToolContext { cwd: PathBuf::from("."), shell: None, todos: Default::default(), spawner: Some(std::sync::Arc::new(NullSpawner)) };
        let s = ctx2.spawner.as_ref().unwrap().spawn(SubagentRequest {
            task: "t".into(), allowed_tools: vec![], context: None, context_budget: None, allow_nested: false,
        }).await.unwrap();
        assert_eq!(s, "子任务摘要");
    }
}
