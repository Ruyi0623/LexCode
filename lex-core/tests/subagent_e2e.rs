//! 端到端:主循环 → spawn_subagent → 子 agent → 摘要回填主循环历史。
//! 全程脚本化 Provider,不触网。
//!
//! 本文件是跨层装配级回归(agent 状态机 + 工具注册表 + 安全边界 + 派生运行时),
//! 与 `agent::subagent` 内的单元测试互补:那里测子 agent 自身行为,这里测
//! 「主循环真的能通过工具路径把子任务派出去、且只拿回摘要 / 权限不因派生而放宽」。
use futures::StreamExt;
use lex_core::agent::{AgentLoop, ChildEvent, ChildEventHook, ChildEventKind, ToolResultHook};
use lex_core::error::Result;
use lex_core::message::{Block, Message};
use lex_core::provider::throttle::ThrottledProvider;
use lex_core::provider::{Provider, ProviderEvent, RequestContext, StreamResult};
use lex_core::security::{
    Decision, PermissionHandler, PendingAction, SecurityGuard, SecurityRules,
};
use lex_core::tools::bash_exec::BashExec;
use lex_core::tools::file_read::FileRead;
use lex_core::tools::spawn_subagent::SpawnSubagent;
use lex_core::tools::{SubagentSpawner, Todo, ToolContext, ToolRegistry};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

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
        let script = self.scripts.lock().unwrap_or_else(|p| p.into_inner()).pop_front().unwrap_or_default();
        Ok(futures::stream::iter(script.into_iter().map(Ok)).boxed())
    }
}

struct YesHandler;
#[async_trait::async_trait]
impl PermissionHandler for YesHandler {
    async fn confirm(&self, _: &PendingAction) -> Result<bool> {
        Ok(true)
    }
}

fn completed() -> ProviderEvent {
    ProviderEvent::Completed { usage: lex_core::message::Usage::default() }
}
fn text_reply(s: &str) -> Vec<ProviderEvent> {
    vec![ProviderEvent::TextDelta(s.to_string()), completed()]
}
fn tool_use(id: &str, name: &str, input: serde_json::Value) -> Vec<ProviderEvent> {
    vec![ProviderEvent::ToolUseComplete { id: id.into(), name: name.into(), input }, completed()]
}

/// 按生产装配方式构造主循环(与 lex-cli `build_loop` 同形:同一 handler/规则表/派生器)。
fn build_loop(
    provider: ThrottledProvider,
    registry: ToolRegistry,
    rules: SecurityRules,
    spawner: Arc<dyn SubagentSpawner>,
    todos: Arc<Mutex<Vec<Todo>>>,
    hook: Option<ToolResultHook>,
) -> AgentLoop {
    AgentLoop {
        provider: Box::new(provider),
        registry,
        handler: Box::new(YesHandler),
        tool_ctx: ToolContext { cwd: std::path::PathBuf::from("."), shell: None, todos, spawner: Some(spawner) },
        security: SecurityGuard::new(rules),
        system: "主提示词".into(),
        history: vec![],
        max_turns: 10,
        cache_strategy: None,
        context_limit: None,
        pending_summary: None,
        compress_attempted: false,
        on_tool_result: hook,
    }
}

/// 取主历史中指定 tool_use_id 的 tool_result。
fn tool_result(agent: &AgentLoop, id: &str) -> Option<(String, bool)> {
    agent.history.iter().find_map(|m: &Message| {
        m.content.iter().find_map(|b| match b {
            Block::ToolResult { tool_use_id, content, is_error } if tool_use_id == id => {
                Some((content.clone(), *is_error))
            }
            _ => None,
        })
    })
}

const CHILD_SUMMARY: &str = "## 子任务摘要\n- **做了什么**:通读了 tests 目录\n- **关键结论**:失败原因为断言过期\n- **修改的文件**:无";

#[tokio::test]
async fn main_loop_delegates_and_keeps_only_summary() {
    let provider = ScriptedProvider::default();
    // 第 1 轮(主):模型调用 spawn_subagent
    provider.push(tool_use(
        "t1",
        "spawn_subagent",
        serde_json::json!({"task":"排查测试失败原因","allowed_tools":["file_read"],"context":"测试目录 tests/"}),
    ));
    // 第 1 轮(子):子 agent 直接给出结构化摘要
    provider.push(text_reply(CHILD_SUMMARY));
    // 第 2 轮(主):模型基于摘要作答
    provider.push(text_reply("子任务已完成,结论:断言过期。"));

    let throttled = ThrottledProvider::new(Arc::new(provider.clone()), 3);
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FileRead));
    registry.register(Box::new(SpawnSubagent::new(4)));
    let todos: Arc<Mutex<Vec<Todo>>> = Default::default();
    let spawner: Arc<dyn SubagentSpawner> = Arc::new(lex_core::agent::subagent::SubagentRuntime::new(
        throttled.clone(),
        "主提示词".into(),
        Arc::new(YesHandler),
        SecurityRules::defaults(),
        std::path::PathBuf::from("."),
        None,
        Arc::new(registry),
        lex_core::agent::subagent::SpawnLimits { max_turns: 10, context_limit: Some(64_000), max_children_per_turn: 4 },
        lex_core::agent::SubagentHooks { on_child_event: None },
    ));

    let mut loop_registry = ToolRegistry::new();
    loop_registry.register(Box::new(FileRead));
    loop_registry.register(Box::new(SpawnSubagent::new(4)));
    let mut agent = build_loop(throttled, loop_registry, SecurityRules::defaults(), spawner, todos, None);

    let final_text = agent.run_turn("排查测试失败原因", &mut |_| {}).await.unwrap();
    assert!(final_text.contains("断言过期"));

    // 主上下文里:tool_result 内容 = 子 agent 最终回复原文(不含子过程)
    let (content, is_error) = tool_result(&agent, "t1").expect("主历史应包含 spawn_subagent 的 tool_result");
    assert!(!is_error, "spawn_subagent 应成功,实际结果: {content}");
    // 精确相等而不是 contains:任何「把子历史/子 system/任务原文捎带回来」的实现都会被这条抓住
    // (宽泛的 contains("子任务摘要") 对「摘要 + 一大坨子历史」同样成立,分不清摘要回传与历史回传)
    assert_eq!(content, CHILD_SUMMARY, "父级只应拿到子 agent 的最终摘要原文");
    assert!(!content.contains("主提示词"), "子 agent 的 system prompt 不得回流主历史");
    assert!(!content.contains("[父级上下文]"), "父级上下文片段不得回流主历史");

    // 更强:整条主历史里都不该出现子 agent 的首条任务消息(含其 system 前缀)
    let leaked = agent.history.iter().any(|m| {
        m.content.iter().any(|b| {
            let dbg = format!("{b:?}");
            dbg.contains("[父级上下文]") || dbg.contains("子任务模式") || dbg.contains("任务范围限定")
        })
    });
    assert!(!leaked, "子 agent 的内部消息(system 片段/任务原文)不得泄漏进主历史");
}

const FORBIDDEN_MARKER: &str = "LEXCHILD_FORBIDDEN_PROBE";

/// 权限继承:Forbidden 规则在子 agent 内同样硬拦截。
///
/// 安全性:探测命令是 `echo <标记> > <临时文件>`,即使拦截失效也只会留下一个临时文件,
/// 不涉及任何不可逆操作。断言两条独立证据——工具结果为 Forbidden 错误、命令未产生副作用。
#[tokio::test]
async fn forbidden_rule_still_blocks_inside_child_agent() {
    let provider = ScriptedProvider::default();

    // 副作用探针:相对路径 + 无引号无空格,保证 cmd / sh 两种 shell 下都能可靠重定向
    // (bash_exec 的 cwd = tool_ctx.cwd = ".")。先清理残留,避免假阴性。
    let probe = std::path::PathBuf::from(format!("lex_subagent_probe_{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&probe);
    let probe_cmd = format!("echo {FORBIDDEN_MARKER} > {}", probe.display());

    // 父级规则表:Forbidden 段追加一条无害标记命令(内置默认规则原样保留)
    let rules = SecurityRules::build(&lex_core::config::SecurityConfig {
        forbidden: vec![FORBIDDEN_MARKER.to_string()],
        ..Default::default()
    })
    .expect("规则表应可构建");
    // 前置自检:规则表本身必须判 Forbidden(否则下面的测试等于没测)
    assert!(
        matches!(rules.decide(&probe_cmd, false, false), Decision::Forbidden { .. }),
        "规则表未命中探测命令,测试前提不成立"
    );

    let hook_log: Arc<Mutex<Vec<(String, String, bool)>>> = Default::default();
    // 子 agent 的工具结果自 T3 起走子事件通道(不再经父级 on_tool_result),故用 ChildEventHook 观测
    let hook: ChildEventHook = {
        let log = hook_log.clone();
        Arc::new(move |ev: &ChildEvent| {
            if let ChildEventKind::ToolResult { name, first_line, is_error } = &ev.kind {
                log.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push((name.clone(), first_line.clone(), *is_error));
            }
        })
    };

    let throttled = ThrottledProvider::new(Arc::new(provider.clone()), 3);
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(BashExec));
    registry.register(Box::new(SpawnSubagent::new(4)));
    let todos: Arc<Mutex<Vec<Todo>>> = Default::default();
    let spawner: Arc<dyn SubagentSpawner> = Arc::new(lex_core::agent::subagent::SubagentRuntime::new(
        throttled.clone(),
        "主提示词".into(),
        Arc::new(YesHandler),
        rules.clone(),
        std::path::PathBuf::from("."),
        None,
        Arc::new(registry),
        lex_core::agent::subagent::SpawnLimits { max_turns: 10, context_limit: Some(64_000), max_children_per_turn: 4 },
        lex_core::agent::SubagentHooks { on_child_event: Some(hook.clone()) },
    ));

    // 父:派生一个只授权 bash_exec 的子 agent
    provider.push(tool_use(
        "t1",
        "spawn_subagent",
        serde_json::json!({"task":"执行标记命令","allowed_tools":["bash_exec"]}),
    ));
    // 子第 1 轮:调用 bash_exec 执行被 Forbidden 的命令
    provider.push(tool_use("c1", "bash_exec", serde_json::json!({ "command": probe_cmd })));
    // 子第 2 轮:收尾摘要
    provider.push(text_reply("## 子任务摘要\n- **做了什么**:尝试执行标记命令\n- **关键结论**:被安全规则拦截\n- **修改的文件**:无"));
    // 父第 2 轮:收尾
    provider.push(text_reply("子 agent 已回报。"));

    let mut loop_registry = ToolRegistry::new();
    loop_registry.register(Box::new(BashExec));
    loop_registry.register(Box::new(SpawnSubagent::new(4)));
    // 本轮不再需要父级结果钩子:子级结果已走 on_child_event(父级自己的 ⎿ 通道与本事无关)
    let mut agent = build_loop(throttled, loop_registry, rules.clone(), spawner, todos, None);

    let final_text = agent.run_turn("验证权限继承", &mut |_| {}).await.unwrap();
    assert!(final_text.contains("已回报"), "主循环应正常收尾,实际: {final_text}");

    // 证据一(先查并清理):命令从未真正执行——拦截失效时它会创建探测文件。
    // 放在断言之前取状态,保证即便断言失败也不把临时文件留在工作区。
    let side_effect = probe.exists();
    let _ = std::fs::remove_file(&probe);
    assert!(
        !side_effect,
        "Forbidden 命令不得实际执行,但探测文件已被创建: {}",
        probe.display()
    );

    // 证据二:子 agent 的 bash_exec 结果为 Forbidden 错误,而非命令输出。
    // 子 agent 的 tool_result 不会回到主历史,故经装配时下传给子运行时的 on_child_event
    // 的 ToolResult 事件观测。
    let bash_results: Vec<(String, String, bool)> = hook_log
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .filter(|(name, _, _)| name == "bash_exec")
        .cloned()
        .collect();
    let (_, first_line, is_error) = bash_results
        .first()
        .cloned()
        .expect("子 agent 的 bash_exec 必须产生结果回调(证明工具真的被子 agent 调用了)");
    assert!(is_error, "Forbidden 命中应回填错误 ToolResult,实际首行: {first_line}");
    assert!(first_line.contains("Forbidden"), "拦截应来自 Forbidden 规则,实际首行: {first_line}");
}
