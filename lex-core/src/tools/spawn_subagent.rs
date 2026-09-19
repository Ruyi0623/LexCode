use crate::error::{LexError, Result};
use crate::tools::{SubagentRequest, Tool, ToolContext};
use serde_json::{json, Value};

/// spawn_subagent:把独立、边界清晰的子任务派发给独立上下文的子 agent。
/// 返回值只有子 agent 的结构化摘要,不带回子 agent 的完整消息历史(控制主上下文体积的关键)。
///
/// 每轮派生数量有上限(来自 `[agent] max_children_per_turn`,0 表示禁止派生),
/// 该上限写进 description 供模型自知,避免反复撞墙。
pub struct SpawnSubagent {
    description: String,
}

impl SpawnSubagent {
    pub fn new(max_children_per_turn: u32) -> Self {
        SpawnSubagent {
            description: format!(
                "把一个独立、边界清晰的多步骤子任务派发给拥有独立上下文的子 agent 执行,只返回结构化摘要。\
                 每轮(同一轮对话内)最多派生 {max_children_per_turn} 个子 agent,超出会被拒绝——请优先把相近的排查合并成一个子任务。\
                 仅当子任务的执行长度/探索成本明显高于污染主上下文的代价时使用;琐碎单步操作不要派生。"
            ),
        }
    }
}

#[async_trait::async_trait]
impl Tool for SpawnSubagent {
    fn name(&self) -> &str {
        "spawn_subagent"
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["task", "allowed_tools"],
            "properties": {
                "task": {"type": "string", "description": "子任务描述,需自包含(子 agent 看不到主对话)"},
                "allowed_tools": {"type": "array", "items": {"type": "string"}, "description": "允许子 agent 使用的工具名列表"},
                "context": {"type": "string", "description": "子任务需要的必要上下文片段(可选)"},
                "context_budget": {"type": "integer", "description": "子 agent 上下文 token 上限(可选;缺省或 0 表示继承父级,超过父级上限时按父级上限夹取)"},
                "allow_nested": {"type": "boolean", "description": "是否允许子 agent 再派生下一层(默认 false,硬上限共 2 层)"}
            }
        })
    }
    fn read_only(&self) -> bool {
        false
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<String> {
        let spawner = ctx.spawner.as_ref().ok_or_else(|| {
            LexError::Tool("当前环境不允许派生子 agent(已达派生深度上限或未启用)".into())
        })?;
        let task = crate::tools::file_read::require_str(&input, "task")?;
        let allowed_tools: Vec<String> = input
            .get("allowed_tools")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        if allowed_tools.is_empty() {
            return Err(LexError::Tool("allowed_tools 不能为空:请列出子 agent 可用的工具名".into()));
        }
        let context = input.get("context").and_then(Value::as_str).map(str::to_string);
        // 原样透传模型给的值(0 亦不在此处改写):归一化与「夹取到父级上限」在派生侧完成,
        // 因为只有那里知道父级上限(`agent/subagent.rs::effective_context_limit`)。
        let context_budget = input.get("context_budget").and_then(Value::as_u64).map(|v| u32::try_from(v).unwrap_or(u32::MAX));
        let allow_nested = input.get("allow_nested").and_then(Value::as_bool).unwrap_or(false);
        spawner
            .spawn(SubagentRequest { task, allowed_tools, context, context_budget, allow_nested })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::SubagentSpawner;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Recorder {
        requests: Mutex<Vec<SubagentRequest>>,
        reply: String,
        err: Option<String>,
    }
    #[async_trait::async_trait]
    impl SubagentSpawner for Recorder {
        async fn spawn(&self, req: SubagentRequest) -> Result<String> {
            self.requests.lock().unwrap_or_else(|p| p.into_inner()).push(req);
            if let Some(e) = &self.err {
                return Err(LexError::Tool(e.clone()));
            }
            Ok(self.reply.clone())
        }
    }

    fn ctx_with(rec: Arc<Recorder>) -> ToolContext {
        ToolContext { cwd: std::path::PathBuf::from("."), shell: None, todos: Default::default(), spawner: Some(rec) }
    }

    #[tokio::test]
    async fn parses_input_and_returns_summary() {
        let rec = Arc::new(Recorder { reply: "## 子任务摘要\n- **做了什么**:x".into(), ..Default::default() });
        let out = SpawnSubagent::new(4).execute(
            json!({"task":"排查失败","allowed_tools":["file_read"],"context":"模块在 src/","context_budget":16000,"allow_nested":true}),
            &ctx_with(rec.clone()),
        ).await.unwrap();
        assert!(out.starts_with("## 子任务摘要"));
        let reqs = rec.requests.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(reqs[0].task, "排查失败");
        assert_eq!(reqs[0].allowed_tools, vec!["file_read".to_string()]);
        assert_eq!(reqs[0].context.as_deref(), Some("模块在 src/"));
        assert_eq!(reqs[0].context_budget, Some(16_000));
        assert!(reqs[0].allow_nested);
    }

    #[tokio::test]
    async fn spawner_error_propagates() {
        // 派生器返回 Err(如 allowed_tools 含未知工具 / 达深度上限)时,工具必须把该错误原样上抛,
        // 而不是降级成空摘要或 Ok —— 否则主 agent 会把「派生失败」当成「子任务做完了」。
        let rec = Arc::new(Recorder { err: Some("allowed_tools 含未知工具: ghost".into()), ..Default::default() });
        let err = SpawnSubagent::new(4)
            .execute(json!({"task":"排查失败","allowed_tools":["file_read"]}), &ctx_with(rec.clone()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("未知工具"), "派生器的错误须原样透出,实际: {err}");
        // 请求确实已交到派生器(证明错误来自派生器而非前置校验)
        assert_eq!(rec.requests.lock().unwrap_or_else(|p| p.into_inner()).len(), 1);
    }

    #[tokio::test]
    async fn missing_spawner_is_error() {
        let ctx = ToolContext { cwd: std::path::PathBuf::from("."), shell: None, todos: Default::default(), spawner: None };
        let err = SpawnSubagent::new(4).execute(json!({"task":"t","allowed_tools":["file_read"]}), &ctx).await.unwrap_err();
        assert!(err.to_string().contains("不允许派生"), "实际: {err}");
    }

    #[tokio::test]
    async fn empty_allowed_tools_is_error() {
        let rec = Arc::new(Recorder::default());
        let err = SpawnSubagent::new(4).execute(json!({"task":"t","allowed_tools":[]}), &ctx_with(rec)).await.unwrap_err();
        assert!(err.to_string().contains("allowed_tools"), "实际: {err}");
    }

    #[test]
    fn description_states_the_configured_per_turn_cap() {
        // 模型需要知道上限,否则会反复撞墙而不自知。
        // 写进工具 description —— 模型挑工具时读的就是它。
        let d7 = SpawnSubagent::new(7).description().to_string();
        assert!(d7.contains('7'), "描述应含配置的上限值: {d7}");
        assert!(d7.contains("每轮"), "应说明是按轮的: {d7}");

        // 上限为 0(禁止派生)时也要如实告知
        let d0 = SpawnSubagent::new(0).description().to_string();
        assert!(d0.contains('0'), "描述应如实反映 0: {d0}");
    }

    #[test]
    fn tool_metadata_unchanged_by_construction() {
        let t = SpawnSubagent::new(4);
        assert_eq!(t.name(), "spawn_subagent");
        assert!(!t.read_only());
        assert!(t.parallel_safe());
    }

    #[test]
    fn tool_metadata() {
        let t = SpawnSubagent::new(4);
        assert_eq!(t.name(), "spawn_subagent");
        assert!(!t.read_only());
        assert!(t.parallel_safe());
    }
}
