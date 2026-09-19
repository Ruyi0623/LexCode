use crate::error::{LexError, Result};
use crate::provider::throttle::ThrottledProvider;
use crate::security::{PermissionHandler, SecurityGuard, SecurityRules};
use crate::tools::{ShellCommand, SubagentRequest, SubagentSpawner, ToolContext, ToolRegistry};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// 派生层数硬上限:主循环为第 0 层,最多派生两层(子 = 1,孙 = 2)。
/// 不做成配置项,防止失控递归。
pub const MAX_SPAWN_DEPTH: u32 = 2;

/// depth 层 runtime 派生子 agent 时,子 agent 能否获得派生能力:
/// 共 2 层派生(子=1、孙=2),孙的孙(3)结构性不可达。
fn nested_spawner_allowed(depth: u32, allow_nested: bool) -> bool {
    allow_nested && depth + 2 <= MAX_SPAWN_DEPTH
}

/// 子 agent system prompt:主 prompt 原文作为逐字节前缀(保住隐式前缀缓存),
/// 任务范围限定只追加在末尾,绝不插进固定前缀中间。
pub fn compose_subagent_system(base: &str, suffix: &str) -> String {
    format!("{base}\n\n{suffix}")
}

pub fn subagent_system_suffix(task: &str, allowed_tools: &[String]) -> String {
    format!(
        "# 子任务模式\n你是被主 agent 派生出来的子 agent,只负责下面这一项独立子任务,完成后即结束:\n\n{task}\n\n## 任务范围限定\n- 你只能使用以下工具:{};不要尝试调用列表之外的任何工具。\n- 你的执行过程不会回到主对话,主 agent 只会收到你的最终回复,请让最终回复自包含。\n- 完成任务后,最终回复必须严格使用以下结构化摘要格式:\n\n## 子任务摘要\n- **做了什么**:<步骤概述>\n- **关键结论**:<发现/结果>\n- **修改的文件**:<文件路径列表;无修改写「无」>",
        allowed_tools.join(", ")
    )
}

/// 子 agent 的有效上下文上限。该值由模型经 `spawn_subagent` 的 `context_budget` 控制,
/// 故必须校验,否则模型随手编的数字会静默改变子 agent 的压缩策略:
/// - 缺失或 0 → 继承父级上限(0 不作「零上限」解,否则子 agent 首次机会即压缩);
/// - 有值 → 按父级上限夹取(子 agent 不得获得比父级更大的上下文余量);
///   父级无上限时原样透传。
fn effective_context_limit(budget: Option<u32>, parent_limit: Option<u32>) -> Option<u32> {
    match budget {
        None | Some(0) => parent_limit,
        Some(n) => Some(parent_limit.map_or(n, |p| n.min(p))),
    }
}

/// 子 agent 首条任务消息 = 父级上下文片段 + 任务描述
fn child_task_text(req: &SubagentRequest) -> String {
    match req.context.as_deref() {
        Some(c) if !c.trim().is_empty() => format!("[父级上下文]\n{c}\n\n[子任务]\n{}", req.task),
        _ => req.task.clone(),
    }
}

/// 子 agent 的运行限额(集中传递,避免构造函数参数爆炸)
#[derive(Debug, Clone, Copy)]
pub struct SpawnLimits {
    pub max_turns: u32,
    pub context_limit: Option<u32>,
    /// 每轮最多派生多少个子 agent;0 = 禁止派生
    pub max_children_per_turn: u32,
}

/// 整棵派生树共享的轮次级状态。仅 depth == 0 的 runtime 在 `begin_turn` 时重置它,
/// 故「每轮上限」的口径是**整棵树在根的一轮内**的派生总数,而非每个父级各自计数。
#[derive(Clone, Default)]
pub(crate) struct SpawnState {
    spawned_this_turn: Arc<AtomicU32>,
}

impl SpawnState {
    /// 本轮已派生的子 agent 数。
    /// 仅测试直接读取该值(生产路径只经 `try_admit` 判定),故以 `cfg(test)` 收窄可见性,
    /// 避免 lib 目标出现 `dead_code` 警告。
    #[cfg(test)]
    fn spawned(&self) -> u32 {
        self.spawned_this_turn.load(Ordering::SeqCst)
    }

    /// 清零(新一轮开始;仅根 runtime 调用)
    fn reset(&self) {
        self.spawned_this_turn.store(0, Ordering::SeqCst);
    }

    /// 尝试占用一个派生名额。超上限时不占位(回滚)并返回 false。
    /// 用「先加后判、越限回滚」而非 CAS 循环:并发的两次调用各自的 new 都已包含对方,
    /// 故不会双双越限;回滚保证被拒的派生不消耗配额。
    fn try_admit(&self, max_children_per_turn: u32) -> bool {
        let admitted = self.spawned_this_turn.fetch_add(1, Ordering::SeqCst) + 1;
        if admitted > max_children_per_turn {
            self.spawned_this_turn.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        true
    }
}

/// 子 agent 运行时:持有共享的节流 provider / 主 prompt 前缀 / 父级权限配置 / 基础注册表。
pub struct SubagentRuntime {
    provider: ThrottledProvider,
    base_system: String,
    handler: Arc<dyn PermissionHandler>,
    rules: SecurityRules,
    cwd: PathBuf,
    shell: Option<ShellCommand>,
    base_registry: Arc<ToolRegistry>,
    limits: SpawnLimits,
    on_tool_result: Option<crate::agent::ToolResultHook>,
    depth: u32,
    /// 整棵派生树共享;新树由 `new` 创建,嵌套时 clone 下去
    spawn_state: SpawnState,
}

impl SubagentRuntime {
    pub fn new(
        provider: ThrottledProvider,
        base_system: String,
        handler: Arc<dyn PermissionHandler>,
        rules: SecurityRules,
        cwd: PathBuf,
        shell: Option<ShellCommand>,
        base_registry: Arc<ToolRegistry>,
        limits: SpawnLimits,
        on_tool_result: Option<crate::agent::ToolResultHook>,
    ) -> Self {
        Self::with_depth(provider, base_system, handler, rules, cwd, shell, base_registry, limits, on_tool_result, SpawnState::default(), 0)
    }

    /// 带派生深度与共享轮次状态的构造:主循环走 `new`(depth 0),嵌套派生器由此构造(depth + 1)。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with_depth(
        provider: ThrottledProvider,
        base_system: String,
        handler: Arc<dyn PermissionHandler>,
        rules: SecurityRules,
        cwd: PathBuf,
        shell: Option<ShellCommand>,
        base_registry: Arc<ToolRegistry>,
        limits: SpawnLimits,
        on_tool_result: Option<crate::agent::ToolResultHook>,
        spawn_state: SpawnState,
        depth: u32,
    ) -> Self {
        SubagentRuntime { provider, base_system, handler, rules, cwd, shell, base_registry, limits, on_tool_result, depth, spawn_state }
    }
}

#[async_trait::async_trait]
impl SubagentSpawner for SubagentRuntime {
    async fn spawn(&self, req: SubagentRequest) -> Result<String> {
        // 1. allowed_tools 校验与过滤:spawn_subagent 永不随 allowed_tools 传入(嵌套只由 allow_nested 控制)
        let allowed: Vec<String> = req.allowed_tools.iter().filter(|n| n.as_str() != "spawn_subagent").cloned().collect();
        if allowed.is_empty() {
            return Err(LexError::Tool("allowed_tools 过滤后为空:至少提供一个工具".into()));
        }
        let unknown: Vec<String> = allowed.iter().filter(|n| self.base_registry.get(n).is_none()).cloned().collect();
        if !unknown.is_empty() {
            return Err(LexError::Tool(format!(
                "allowed_tools 含未知工具: {}(可用: {})",
                unknown.join(", "),
                self.base_registry.names().join(", ")
            )));
        }

        // 2. 深度防御 + 嵌套判定:显式 allow_nested + 深度硬上限;
        //    不授予时子 agent 完全拿不到 spawn 工具与派生器(结构性不可绕过)
        if self.depth + 1 > MAX_SPAWN_DEPTH {
            return Err(LexError::Tool(format!("已达派生深度硬上限 {} 层,禁止继续派生", MAX_SPAWN_DEPTH)));
        }

        // 3. 成本护栏:整棵派生树在根的一轮内共享计数,防止单轮扇出失控。
        if !self.spawn_state.try_admit(self.limits.max_children_per_turn) {
            return Err(LexError::Tool(format!(
                "本轮派生子 agent 已达上限 {}(可在 lex-code.toml 的 [agent] max_children_per_turn 调整)",
                self.limits.max_children_per_turn
            )));
        }

        // 子 agent 的有效上限只算一次:孙级派生器与子 AgentLoop 共用同一个值,
        // 否则孙级继承的是根的配置上限、而非其父级请求的预算(模型给的值被跳过)。
        let effective_limit = effective_context_limit(req.context_budget, self.limits.context_limit);
        let nested_spawner: Option<Arc<dyn SubagentSpawner>> = if nested_spawner_allowed(self.depth, req.allow_nested) {
            Some(Arc::new(SubagentRuntime::with_depth(
                self.provider.clone(),
                self.base_system.clone(),
                self.handler.clone(),
                self.rules.clone(),
                self.cwd.clone(),
                self.shell.clone(),
                self.base_registry.clone(),
                SpawnLimits {
                    max_turns: self.limits.max_turns,
                    context_limit: effective_limit,
                    max_children_per_turn: self.limits.max_children_per_turn,
                },
                self.on_tool_result.clone(),
                self.spawn_state.clone(),
                self.depth + 1,
            )))
        } else {
            None
        };

        let mut child_registry = self.base_registry.subset(&allowed);
        if nested_spawner.is_some() {
            child_registry.register(Box::new(crate::tools::spawn_subagent::SpawnSubagent));
        }

        // 4. 独立 AgentLoop:独立历史、独立 todos、独立 SecurityGuard(规则表与父级相同 = 权限不高于父级)
        let mut child = crate::agent::AgentLoop {
            provider: Box::new(self.provider.clone()),
            registry: child_registry,
            handler: Box::new(self.handler.clone()),
            tool_ctx: ToolContext { cwd: self.cwd.clone(), shell: self.shell.clone(), todos: Arc::new(Mutex::new(Vec::new())), spawner: nested_spawner },
            security: SecurityGuard::new(self.rules.clone()),
            system: compose_subagent_system(&self.base_system, &subagent_system_suffix(&req.task, &allowed)),
            history: vec![],
            max_turns: self.limits.max_turns,
            cache_strategy: None,
            context_limit: effective_limit,
            pending_summary: None,
            compress_attempted: false,
            on_tool_result: self.on_tool_result.clone(),
        };

        // 5. 运行子任务,只把最终摘要(结构化文本)交回父级
        let final_text = child.run_turn(&child_task_text(&req), &mut |_e| {}).await?;
        Ok(final_text)
    }

    /// 新一轮开始:仅根 runtime(depth 0)清零当轮计数。
    /// 子 runtime 与父级共享同一个计数器,不设闸的话子 agent 每跑一轮都会清空
    /// 父级的当轮计数,护栏就被静默架空了 —— 这是本设计最易写错处。
    fn begin_turn(&self) {
        if self.depth == 0 {
            self.spawn_state.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{Provider, ProviderEvent, RequestContext, StreamResult};
    use crate::tools::{SubagentRequest as Req, ToolRegistry};
    use futures::StreamExt;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[test]
    fn composed_system_keeps_base_as_byte_prefix() {
        // 缓存关键:子 agent system = 主 prompt 原文前缀,任务限定只追加在末尾
        let base = "# 身份\n你是编程 agent。\n\n# 核心原则\n安全第一。";
        let suffix = subagent_system_suffix("排查测试", &["file_read".into()]);
        let composed = compose_subagent_system(base, &suffix);
        assert!(composed.starts_with(base), "任务限定内容必须追加在固定前缀之后");
        assert!(composed.contains("file_read"));
        assert!(composed.contains("子任务摘要"));
        // 同一任务两次生成应逐字节一致(前缀缓存稳定性)
        assert_eq!(subagent_system_suffix("t", &["a".into()]), subagent_system_suffix("t", &["a".into()]));
    }

    #[test]
    fn nested_depth_boundary() {
        // 语义:depth 层 runtime 派生子 agent 时,子 agent 能否获得派生能力。
        // 共 2 层派生:主(0)→子(1)可授;子(1)→孙(2)不授(孙彻底没有派生工具,结构性封顶)。
        assert!(nested_spawner_allowed(0, true));
        assert!(!nested_spawner_allowed(1, true), "孙 agent 不得再获得派生能力(2 层硬上限)");
        assert!(!nested_spawner_allowed(2, true));
        assert!(!nested_spawner_allowed(0, false), "未显式允许不得嵌套");
    }

    #[test]
    fn child_task_text_merges_context_fragment() {
        let req = SubagentRequest {
            task: "排查失败".into(),
            allowed_tools: vec!["file_read".into()],
            context: Some("模块在 src/foo".into()),
            context_budget: None,
            allow_nested: false,
        };
        let text = child_task_text(&req);
        assert!(text.contains("[父级上下文]"));
        assert!(text.contains("模块在 src/foo"));
        assert!(text.contains("排查失败"));
        let mut req2 = req.clone();
        req2.context = None;
        assert_eq!(child_task_text(&req2), "排查失败");
    }

    #[test]
    fn context_budget_is_normalized_and_clamped_to_parent() {
        // 父级无上限:模型给的值原样透传,缺失仍为 None
        assert_eq!(effective_context_limit(Some(16_000), None), Some(16_000));
        assert_eq!(effective_context_limit(None, None), None);
        // 模型传 0 = 未指定(继承父级),绝不解成「零上限」——否则子 agent 首次机会即压缩
        assert_eq!(effective_context_limit(Some(0), Some(64_000)), Some(64_000));
        assert_eq!(effective_context_limit(Some(0), None), None);
        // 缺失 = 继承父级上限
        assert_eq!(effective_context_limit(None, Some(64_000)), Some(64_000));
        // 超过父级上限 → 夹到父级(子 agent 不得比父级余量更大)
        assert_eq!(effective_context_limit(Some(999_999), Some(64_000)), Some(64_000));
        // 低于父级上限 → 尊重模型给的值
        assert_eq!(effective_context_limit(Some(8_000), Some(64_000)), Some(8_000));
    }

    // —— 以下为 spawn 行为测试:脚本化 Provider,验证权限/深度/过滤不被任务描述绕过 ——

    #[derive(Clone, Default)]
    struct ScriptedProvider {
        scripts: Arc<Mutex<VecDeque<Vec<ProviderEvent>>>>,
    }
    impl ScriptedProvider {
        fn push(&self, events: Vec<ProviderEvent>) {
            self.scripts.lock().unwrap_or_else(|p| p.into_inner()).push_back(events);
        }
    }
    #[async_trait::async_trait]
    impl Provider for ScriptedProvider {
        async fn send(&self, _ctx: RequestContext) -> Result<StreamResult> {
            let script = self
                .scripts
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .pop_front()
                .unwrap_or_default();
            Ok(futures::stream::iter(script.into_iter().map(Ok)).boxed())
        }
    }

    fn completed() -> ProviderEvent {
        ProviderEvent::Completed { usage: crate::message::Usage::default() }
    }
    fn text_reply(s: &str) -> Vec<ProviderEvent> {
        vec![ProviderEvent::TextDelta(s.to_string()), completed()]
    }

    fn runtime_with(
        provider: &ScriptedProvider,
        depth: u32,
        registry: std::sync::Arc<ToolRegistry>,
        state: SpawnState,
        max_children_per_turn: u32,
    ) -> SubagentRuntime {
        let throttled = crate::provider::throttle::ThrottledProvider::new(Arc::new(provider.clone()), 3);
        SubagentRuntime::with_depth(
            throttled,
            "主提示词前缀".into(),
            Arc::new(AllowHandler),
            crate::security::SecurityRules::defaults(),
            std::path::PathBuf::from("."),
            None,
            registry,
            SpawnLimits { max_turns: 10, context_limit: Some(64_000), max_children_per_turn },
            None,
            state,
            depth,
        )
    }

    fn runtime_at_depth(
        provider: &ScriptedProvider,
        depth: u32,
        registry: std::sync::Arc<ToolRegistry>,
    ) -> SubagentRuntime {
        runtime_with(provider, depth, registry, SpawnState::default(), 4)
    }

    struct AllowHandler;
    #[async_trait::async_trait]
    impl crate::security::PermissionHandler for AllowHandler {
        async fn confirm(&self, _: &crate::security::PendingAction) -> crate::error::Result<bool> {
            Ok(true)
        }
    }

    #[tokio::test]
    async fn spawn_returns_child_summary_only() {
        let provider = ScriptedProvider::default();
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(crate::tools::file_read::FileRead));
        registry.register(Box::new(crate::tools::spawn_subagent::SpawnSubagent));
        let registry = Arc::new(registry);
        // 子 agent 一轮:直接产出结构化摘要
        let child_reply = "## 子任务摘要\n- **做了什么**:读了文件\n- **关键结论**:原因 A\n- **修改的文件**:无";
        provider.push(text_reply(child_reply));
        let rt = runtime_at_depth(&provider, 0, registry.clone());
        let summary = rt
            .spawn(Req {
                task: "排查".into(),
                allowed_tools: vec!["file_read".into()],
                context: None,
                context_budget: None,
                allow_nested: false,
            })
            .await
            .unwrap();
        // 父级拿到的只有摘要:与子 agent 最终回复逐字节相同 —— 既不含子历史(任务原文/tool_use/tool_result),
        // 也不含子 system prompt;返回值不是"历史的序列化"而是最终文本本身
        assert_eq!(summary, child_reply, "父级只应拿到子 agent 的最终摘要");
        assert!(!summary.contains("主提示词前缀"), "子 agent 的 system prompt 不得回流父级");
        assert!(!summary.contains("排查"), "父级任务原文不得出现在返回值中(证明返回的不是子历史)");
    }

    #[tokio::test]
    async fn unknown_allowed_tool_is_rejected() {
        let provider = ScriptedProvider::default();
        let registry = Arc::new(ToolRegistry::new());
        let rt = runtime_at_depth(&provider, 0, registry);
        let err = rt
            .spawn(Req { task: "t".into(), allowed_tools: vec!["ghost".into()], context: None, context_budget: None, allow_nested: false })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("未知工具"), "实际: {err}");
    }

    #[tokio::test]
    async fn nested_chain_reaches_two_layers_and_grandchild_cannot_spawn() {
        let provider = ScriptedProvider::default();
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(crate::tools::file_read::FileRead));
        registry.register(Box::new(crate::tools::spawn_subagent::SpawnSubagent));
        let registry = Arc::new(registry);
        let rt = runtime_at_depth(&provider, 0, registry);
        // 子脚本:子 agent 再派生孙 agent(allow_nested=true,depth 1 → 2 合法)
        provider.push(vec![
            ProviderEvent::ToolUseComplete {
                id: "c1".into(),
                name: "spawn_subagent".into(),
                input: serde_json::json!({"task":"孙任务","allowed_tools":["file_read"],"allow_nested":true}),
            },
            completed(),
        ]);
        // 孙脚本:孙 agent(depth 2)只能文本作答——其注册表里没有 spawn_subagent 可调
        provider.push(text_reply("## 子任务摘要\n- **做了什么**:孙任务完成"));
        // 子收尾:汇总
        provider.push(text_reply("## 子任务摘要\n- **做了什么**:委派孙任务完成"));
        let summary = rt
            .spawn(Req { task: "子任务".into(), allowed_tools: vec!["file_read".into()], context: None, context_budget: None, allow_nested: true })
            .await
            .unwrap();
        // 精确串相等:只有"子 agent 真的派生了孙 agent 并消费了孙的回复"这一条路径,父级才会拿到第 3 个脚本。
        // 若嵌套失效(子注册表缺 spawn_subagent),spawn 调用降级为错误 ToolResult、循环继续,
        // 子 agent 会转而吃掉第 2 个脚本「孙任务完成」→ 本断言失败。宽泛的 contains("子任务摘要")
        // 对三个脚本都成立,分不清"嵌套正常"与"嵌套全坏"。
        assert_eq!(summary, "## 子任务摘要\n- **做了什么**:委派孙任务完成", "父级拿到的应是子 agent 自己那份收尾回复");
    }

    #[tokio::test]
    async fn depth_cap_enforced_defensively() {
        // 结构上 depth-2 runtime 不会持有派生器;防御性守卫兜底(即便被错误构造也拒绝)
        let provider = ScriptedProvider::default();
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(crate::tools::file_read::FileRead));
        let registry = Arc::new(registry);
        let rt = runtime_at_depth(&provider, 2, registry);
        let err = rt
            .spawn(Req { task: "t".into(), allowed_tools: vec!["file_read".into()], context: None, context_budget: None, allow_nested: true })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("上限"), "实际: {err}");
        // 结构性证明:子(1)派生孙(2)时不再授予派生能力
        assert!(!nested_spawner_allowed(1, true));
        assert!(nested_spawner_allowed(0, true));
    }

    // —— 以下为成本护栏测试:整棵派生树共享当轮计数,仅根 runtime 重置 ——

    #[test]
    fn try_admit_enforces_limit_without_leaking_slots() {
        let st = SpawnState::default();
        assert!(st.try_admit(2));
        assert!(st.try_admit(2));
        assert!(!st.try_admit(2), "第三次应被拒");
        assert_eq!(st.spawned(), 2, "被拒的派生不得占位(必须回滚)");
        st.reset();
        assert_eq!(st.spawned(), 0);
        assert!(st.try_admit(2), "重置后应重新可派生");
    }

    #[test]
    fn zero_limit_forbids_every_spawn() {
        let st = SpawnState::default();
        assert!(!st.try_admit(0));
        assert_eq!(st.spawned(), 0, "被拒的派生不占位");
    }

    #[test]
    fn clones_share_one_counter() {
        let a = SpawnState::default();
        let b = a.clone();
        assert!(a.try_admit(1));
        assert!(!b.try_admit(1), "clone 必须共享同一计数器,而非各持一份——否则每层各有一个上限");
    }

    #[tokio::test]
    async fn spawn_is_rejected_past_per_turn_limit() {
        let provider = ScriptedProvider::default();
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(crate::tools::file_read::FileRead));
        let registry = Arc::new(registry);
        provider.push(text_reply("## 子任务摘要\n- **做了什么**:一"));
        provider.push(text_reply("## 子任务摘要\n- **做了什么**:二"));

        let state = SpawnState::default();
        let rt = runtime_with(&provider, 0, registry, state.clone(), 1);
        let req = || Req {
            task: "t".into(),
            allowed_tools: vec!["file_read".into()],
            context: None,
            context_budget: None,
            allow_nested: false,
        };
        assert!(rt.spawn(req()).await.is_ok(), "第 1 次应放行");
        let err = rt.spawn(req()).await.unwrap_err();
        assert!(err.to_string().contains("已达上限"), "实际: {err}");
        assert_eq!(state.spawned(), 1, "被拒的那次必须回滚,不占配额");
    }

    #[tokio::test]
    async fn child_begin_turn_does_not_clear_parent_counter() {
        // 设计中最易写错处:子 runtime 与父级共享同一个 SpawnState。
        // 若 begin_turn 不按 depth 设闸,子 agent 每跑一轮都会清空父级的当轮计数,
        // 护栏表面还在、实际已被架空。
        let provider = ScriptedProvider::default();
        let registry = Arc::new(ToolRegistry::new());
        let state = SpawnState::default();

        let parent = runtime_with(&provider, 0, registry.clone(), state.clone(), 4);
        let child = runtime_with(&provider, 1, registry, state.clone(), 4);

        assert!(state.try_admit(4));
        assert_eq!(state.spawned(), 1);

        child.begin_turn(); // 子 agent 开始一轮 → 不得触碰父级计数
        assert_eq!(state.spawned(), 1, "子 runtime 的 begin_turn 必须 no-op");

        parent.begin_turn(); // 根开始新一轮 → 清零
        assert_eq!(state.spawned(), 0, "只有 depth==0 才清零");
    }
}
