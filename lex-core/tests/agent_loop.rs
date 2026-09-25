use async_trait::async_trait;
use lex_core::agent::AgentLoop;
use lex_core::error::{LexError, Result};
use lex_core::message::{Block, Message};
use lex_core::provider::{Provider, ProviderEvent, RequestContext, StreamResult};
use lex_core::security::{PermissionHandler, SecurityGuard, SecurityRules};
use lex_core::tools::{Tool, ToolContext, ToolRegistry};
use serde_json::json;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// 按脚本回放的 MockProvider:每次 send 消费一条脚本(事件序列,末尾自动补 Completed)。
struct MockProvider {
    scripts: Mutex<Vec<Vec<ProviderEvent>>>,
}

impl MockProvider {
    fn new(scripts: Vec<Vec<ProviderEvent>>) -> Self {
        MockProvider { scripts: Mutex::new(scripts) }
    }
}

#[async_trait]
impl Provider for MockProvider {
    async fn send(&self, _ctx: RequestContext) -> Result<StreamResult> {
        let mut q = self.scripts.lock().unwrap();
        let script = q.remove(0);
        Ok(Box::pin(futures::stream::iter(script.into_iter().map(Ok))))
    }
}

struct AllowAll;
#[async_trait]
impl PermissionHandler for AllowAll {
    async fn confirm(&self, _a: &lex_core::security::PendingAction) -> Result<bool> { Ok(true) }
}

struct FakeRead;
#[async_trait]
impl Tool for FakeRead {
    fn name(&self) -> &str { "file_read" }
    fn description(&self) -> &str { "f" }
    fn schema(&self) -> serde_json::Value { json!({"type":"object"}) }
    fn read_only(&self) -> bool { true }
    async fn execute(&self, _i: serde_json::Value, _c: &ToolContext) -> Result<String> { Ok("文件内容X".into()) }
}

fn tool_call_event(id: &str, name: &str, input: serde_json::Value) -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::ToolUseStart { id: id.into(), name: name.into() },
        ProviderEvent::ToolUseComplete { id: id.into(), name: name.into(), input },
    ]
}

#[tokio::test]
async fn two_turn_loop_reads_then_answers() {
    let mock = MockProvider::new(vec![
        tool_call_event("t1", "file_read", json!({"path":"a.txt"})),
        vec![ProviderEvent::TextDelta("做完了".into())],
    ]);
    let mut loop_ = AgentLoop {
        provider: Box::new(mock),
        registry: ToolRegistry::new(),
        handler: lex_core::agent::SharedHandler::new(Arc::new(AllowAll)),
        tool_ctx: ToolContext { cwd: PathBuf::from("."), shell: None, todos: Default::default(), spawner: None },
        security: SecurityGuard::new(SecurityRules::defaults()),
        system: "sys".into(),
        history: vec![],
        max_turns: 5,
        cache_strategy: None,
        context_limit: None,
        pending_summary: None,
        compress_attempted: false,
        on_tool_result: None,
    };
    loop_.registry.register(Box::new(FakeRead));

    let mut saw_start = false;
    let final_text = loop_
        .run_turn("读一下 a.txt", &mut |e| {
            if matches!(e, ProviderEvent::ToolUseStart { .. }) { saw_start = true; }
        })
        .await
        .unwrap();

    assert_eq!(final_text, "做完了");
    assert!(saw_start);
    // 历史结构:user → assistant(tool_use) → user(tool_result 单条) → assistant(text)
    assert_eq!(loop_.history.len(), 4);
    assert_eq!(loop_.history[0], Message::user_text("读一下 a.txt"));
    assert!(matches!(&loop_.history[1].content[0], Block::ToolUse { id, .. } if id == "t1"));
    match &loop_.history[2] {
        Message { role: lex_core::message::Role::User, content } => {
            assert_eq!(content.len(), 1, "tool_result 必须合入单条 user 消息");
            assert!(matches!(&content[0], Block::ToolResult { content, is_error, .. } if content == "文件内容X" && !is_error));
        }
        o => panic!("{o:?}"),
    }
    assert!(matches!(&loop_.history[3].content[0], Block::Text { text } if text == "做完了"));
}

#[tokio::test]
async fn denied_tool_result_feeds_back_and_model_can_finish() {
    struct Deny;
    #[async_trait]
    impl PermissionHandler for Deny {
        async fn confirm(&self, _a: &lex_core::security::PendingAction) -> Result<bool> { Ok(false) }
    }
    struct FakeWrite;
    #[async_trait]
    impl Tool for FakeWrite {
        fn name(&self) -> &str { "file_edit" }
        fn description(&self) -> &str { "w" }
        fn schema(&self) -> serde_json::Value { json!({"type":"object"}) }
        fn read_only(&self) -> bool { false }
        async fn execute(&self, _i: serde_json::Value, _c: &ToolContext) -> Result<String> { Ok("已写入".into()) }
    }
    let mock = MockProvider::new(vec![
        tool_call_event("t1", "file_edit", json!({"path":"a.txt"})),
        vec![ProviderEvent::TextDelta("好的,不写了".into())],
    ]);
    let mut loop_ = AgentLoop {
        provider: Box::new(mock),
        registry: ToolRegistry::new(),
        handler: lex_core::agent::SharedHandler::new(Arc::new(Deny)),
        tool_ctx: ToolContext { cwd: PathBuf::from("."), shell: None, todos: Default::default(), spawner: None },
        security: SecurityGuard::new(SecurityRules::defaults()),
        system: String::new(),
        history: vec![],
        max_turns: 5,
        cache_strategy: None,
        context_limit: None,
        pending_summary: None,
        compress_attempted: false,
        on_tool_result: None,
    };
    loop_.registry.register(Box::new(FakeWrite));
    let text = loop_.run_turn("改 a.txt", &mut |_| {}).await.unwrap();
    assert_eq!(text, "好的,不写了");
    match &loop_.history[2].content[0] {
        Block::ToolResult { content, is_error, .. } => {
            assert!(is_error);
            assert!(content.contains("拒绝"));
        }
        o => panic!("{o:?}"),
    }
}

#[tokio::test]
async fn max_turns_exceeded_is_error() {
    // 每次 send 都返回 tool_use,永远不结束
    let script = tool_call_event("t1", "file_read", json!({"path":"a.txt"}));
    let mock = MockProvider::new((0..10).map(|_| script.clone()).collect());
    let mut loop_ = AgentLoop {
        provider: Box::new(mock),
        registry: ToolRegistry::new(),
        handler: lex_core::agent::SharedHandler::new(Arc::new(AllowAll)),
        tool_ctx: ToolContext { cwd: PathBuf::from("."), shell: None, todos: Default::default(), spawner: None },
        security: SecurityGuard::new(SecurityRules::defaults()),
        system: String::new(),
        history: vec![],
        max_turns: 3,
        cache_strategy: None,
        context_limit: None,
        pending_summary: None,
        compress_attempted: false,
        on_tool_result: None,
    };
    loop_.registry.register(Box::new(FakeRead));
    let err = loop_.run_turn("x", &mut |_| {}).await.unwrap_err();
    assert!(matches!(err, LexError::Provider(_)));
}

// ---------- 只读并发 / 副作用串行 ----------

use std::time::{Duration, Instant};

struct SlowTool {
    name: &'static str,
    read_only: bool,
}
#[async_trait]
impl Tool for SlowTool {
    fn name(&self) -> &str { self.name }
    fn description(&self) -> &str { "s" }
    fn schema(&self) -> serde_json::Value { json!({"type":"object"}) }
    fn read_only(&self) -> bool { self.read_only }
    async fn execute(&self, _i: serde_json::Value, _c: &ToolContext) -> Result<String> {
        tokio::time::sleep(Duration::from_millis(250)).await;
        Ok(format!("{}-done", self.name))
    }
}

fn slow_loop(scripts: Vec<Vec<ProviderEvent>>) -> AgentLoop {
    AgentLoop {
        provider: Box::new(MockProvider::new(scripts)),
        registry: ToolRegistry::new(),
        handler: lex_core::agent::SharedHandler::new(Arc::new(AllowAll)),
        tool_ctx: ToolContext { cwd: PathBuf::from("."), shell: None, todos: Default::default(), spawner: None },
        security: SecurityGuard::new(SecurityRules::defaults()),
        system: String::new(),
        history: vec![],
        max_turns: 5,
        cache_strategy: None,
        context_limit: None,
        pending_summary: None,
        compress_attempted: false,
        on_tool_result: None,
    }
}

#[tokio::test]
async fn readonly_tools_run_concurrently() {
    // 两个各 250ms 的只读工具:并发应 ~250ms,串行需 ~500ms
    let scripts = vec![
        [
            tool_call_event("t1", "slow_read", json!({"p":"a"})),
            tool_call_event("t2", "slow_read", json!({"p":"b"})),
        ].concat(),
        vec![ProviderEvent::TextDelta("都读完了".into())],
    ];
    let mut loop_ = slow_loop(scripts);
    loop_.registry.register(Box::new(SlowTool { name: "slow_read", read_only: true }));

    let start = Instant::now();
    let text = loop_.run_turn("并发读", &mut |_| {}).await.unwrap();
    let elapsed = start.elapsed();

    assert_eq!(text, "都读完了");
    assert!(elapsed < Duration::from_millis(400), "只读工具应并发执行,实际耗时 {elapsed:?}");
    // 结果顺序与 tool_use 顺序一致
    match &loop_.history[2].content[0] {
        Block::ToolResult { content, .. } => assert_eq!(content, "slow_read-done"),
        o => panic!("{o:?}"),
    }
}

#[tokio::test]
async fn mixed_batch_runs_serially() {
    // 一个只读 + 一个副作用:严格串行,总耗时 >= 两次执行之和
    let scripts = vec![
        [
            tool_call_event("t1", "slow_read", json!({"p":"a"})),
            tool_call_event("t2", "slow_write", json!({"p":"b"})),
        ].concat(),
        vec![ProviderEvent::TextDelta("完成".into())],
    ];
    let mut loop_ = slow_loop(scripts);
    loop_.registry.register(Box::new(SlowTool { name: "slow_read", read_only: true }));
    loop_.registry.register(Box::new(SlowTool { name: "slow_write", read_only: false }));

    let start = Instant::now();
    let _ = loop_.run_turn("混合批次", &mut |_| {}).await.unwrap();
    let elapsed = start.elapsed();

    assert!(elapsed >= Duration::from_millis(450), "混入副作用工具时必须串行,实际耗时 {elapsed:?}");
}

#[tokio::test]
async fn forbidden_command_is_hard_blocked_without_confirm() {
    // MockProvider 直接下发 Forbidden 命令(绕过模型自律,专测拦截层):
    // rm -rf .git 必须被硬拦截,不走用户确认,结果为 is_error 回填历史
    struct FakeBash;
    #[async_trait]
    impl Tool for FakeBash {
        fn name(&self) -> &str { "bash_exec" }
        fn description(&self) -> &str { "b" }
        fn schema(&self) -> serde_json::Value { json!({"type":"object"}) }
        fn read_only(&self) -> bool { false }
        async fn execute(&self, _i: serde_json::Value, _c: &ToolContext) -> Result<String> {
            panic!("Forbidden 命令不应被执行到工具层")
        }
    }
    let scripts = vec![
        tool_call_event("t1", "bash_exec", json!({"command": "rm -rf .git"})),
        vec![ProviderEvent::TextDelta("已拦截,我不会执行该操作".into())],
    ];
    let mut loop_ = slow_loop(scripts);
    loop_.registry.register(Box::new(FakeBash));

    let text = loop_.run_turn("删掉 .git", &mut |_| {}).await.unwrap();
    assert_eq!(text, "已拦截,我不会执行该操作");
    match &loop_.history[2].content[0] {
        Block::ToolResult { content, is_error, .. } => {
            assert!(is_error);
            assert!(content.contains("硬性拦截"), "实际: {content}");
        }
        o => panic!("{o:?}"),
    }
}

// ---------- 轮次生命周期钩子 ----------

struct BeginTurnProbe {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}
#[async_trait]
impl lex_core::tools::SubagentSpawner for BeginTurnProbe {
    async fn spawn(&self, _req: lex_core::tools::SubagentRequest) -> Result<String> {
        unreachable!("本测试不派生")
    }
    fn begin_turn(&self) {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[tokio::test]
async fn run_turn_signals_spawner_begin_turn() {
    let mock = MockProvider::new(vec![vec![ProviderEvent::TextDelta("好".into())]]);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut loop_ = AgentLoop {
        provider: Box::new(mock),
        registry: ToolRegistry::new(),
        handler: lex_core::agent::SharedHandler::new(Arc::new(AllowAll)),
        tool_ctx: ToolContext {
            cwd: PathBuf::from("."),
            shell: None,
            todos: Default::default(),
            spawner: Some(Arc::new(BeginTurnProbe { calls: calls.clone() })),
        },
        security: SecurityGuard::new(SecurityRules::defaults()),
        system: "sys".into(),
        history: vec![],
        max_turns: 5,
        cache_strategy: None,
        context_limit: None,
        pending_summary: None,
        compress_attempted: false,
        on_tool_result: None,
    };
    loop_.run_turn("任务", &mut |_| {}).await.unwrap();
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "run_turn 必须调用 begin_turn —— 否则轮次级状态永不重置,每轮上限会退化成会话上限"
    );
}
