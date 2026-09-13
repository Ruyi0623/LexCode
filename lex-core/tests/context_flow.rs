use async_trait::async_trait;
use lex_core::agent::AgentLoop;
use lex_core::error::{LexError, Result};
use lex_core::message::{Block, Message};
use lex_core::provider::cache::{CacheStrategy, ImplicitPrefixCacheStrategy};
use lex_core::provider::{Provider, ProviderEvent, RequestContext, StreamResult};
use lex_core::security::{PermissionHandler, SecurityGuard, SecurityRules};
use lex_core::tools::{ToolContext, ToolRegistry};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// 记录收到的 RequestContext,便于断言压缩摘要请求与正式请求的组装
struct CapturingMock {
    scripts: Mutex<Vec<Vec<ProviderEvent>>>,
    seen: Arc<Mutex<Vec<RequestContext>>>,
}

impl CapturingMock {
    fn new(scripts: Vec<Vec<ProviderEvent>>) -> (Self, Arc<Mutex<Vec<RequestContext>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        (CapturingMock { scripts: Mutex::new(scripts), seen: seen.clone() }, seen)
    }
}

#[async_trait]
impl Provider for CapturingMock {
    async fn send(&self, ctx: RequestContext) -> Result<StreamResult> {
        self.seen.lock().unwrap().push(ctx);
        let script = self
            .scripts
            .lock()
            .unwrap()
            .pop_front_or_err()
            .ok_or_else(|| LexError::Provider("测试脚本耗尽".into()))?;
        Ok(Box::pin(futures::stream::iter(script.into_iter().map(Ok))))
    }
}

trait PopFront {
    fn pop_front_or_err(&mut self) -> Option<Vec<ProviderEvent>>;
}
impl PopFront for Vec<Vec<ProviderEvent>> {
    fn pop_front_or_err(&mut self) -> Option<Vec<ProviderEvent>> {
        if self.is_empty() { None } else { Some(self.remove(0)) }
    }
}

struct AllowAll;
#[async_trait]
impl PermissionHandler for AllowAll {
    async fn confirm(&self, _a: &lex_core::security::PendingAction) -> Result<bool> { Ok(true) }
}

fn text_script(t: &str) -> Vec<ProviderEvent> {
    vec![ProviderEvent::TextDelta(t.into())]
}

fn seeded_history() -> Vec<Message> {
    let big = "长".repeat(200);
    vec![
        Message::user_text(big.clone()),
        Message::assistant(vec![Block::Text { text: big }]),
        Message::user_text("任务二:继续"),
        Message::assistant(vec![Block::Text { text: "好的".into() }]),
    ]
}

fn make_loop(
    mock: CapturingMock,
    cache: Option<Arc<dyn CacheStrategy>>,
    context_limit: Option<u32>,
) -> AgentLoop {
    AgentLoop {
        provider: Box::new(mock),
        registry: ToolRegistry::new(),
        handler: Box::new(AllowAll),
        tool_ctx: ToolContext { cwd: PathBuf::from("."), shell: None, todos: Default::default() },
        security: SecurityGuard::new(SecurityRules::defaults()),
        system: String::new(),
        history: seeded_history(),
        max_turns: 5,
        cache_strategy: cache,
        context_limit,
        pending_summary: None,
        compress_attempted: false,
    }
}

fn first_text(m: &Message) -> String {
    match &m.content[0] {
        Block::Text { text } => text.clone(),
        o => panic!("期望 Text 块,实际 {o:?}"),
    }
}

#[tokio::test]
async fn compression_triggers_once_and_merges_into_next_user_message() {
    let (mock, seen) = CapturingMock::new(vec![
        text_script("决策:方案A;文件:src/a.rs"),
        text_script("本轮完成"),
        text_script("占位"),
    ]);
    let cache: Arc<dyn CacheStrategy> = Arc::new(ImplicitPrefixCacheStrategy::new());
    let mut loop_ = make_loop(mock, Some(cache.clone()), Some(8)); // 阈值 = 6 token,种子历史远超

    let text = loop_.run_turn("新任务:收尾", &mut |_| {}).await.unwrap();
    assert_eq!(text, "本轮完成");

    // 摘要请求:无工具定义,单条 user 消息承载早期历史
    let seen = seen.lock().unwrap();
    assert!(seen[0].tools.is_empty(), "摘要请求不应携带工具定义");
    assert_eq!(seen[0].messages.len(), 1);
    let summarize_input = first_text(&seen[0].messages[0]);
    assert!(!summarize_input.contains("任务二"), "切点之后的历史不参与摘要");
    assert!(summarize_input.contains("长长长"), "早期历史应进入摘要请求");

    // 正式请求:摘要并入本轮用户消息(位于保留历史之后),角色交替保持
    let merged = seen[1]
        .messages
        .iter()
        .map(first_text)
        .find(|t| t.contains("[前期对话摘要]"))
        .expect("正式请求应包含并入摘要的用户消息");
    assert!(merged.contains("方案A"));
    assert!(merged.contains("[本轮任务]\n新任务:收尾"));

    // 历史被改写 → 缓存链断开恰好一次
    assert_eq!(cache.invalidations(), 1);
    drop(seen);

    // 第二轮不再触发压缩(会话内只触发一次),摘要只出现一次
    loop_.run_turn("再来一轮", &mut |_| {}).await.unwrap();
    let summary_count = loop_
        .history
        .iter()
        .filter(|m| first_text(m).contains("[前期对话摘要]"))
        .count();
    assert_eq!(summary_count, 1, "摘要只应出现一次");
    assert_eq!(cache.invalidations(), 1);
}

#[tokio::test]
async fn no_compression_below_threshold() {
    let (mock, _seen) = CapturingMock::new(vec![text_script("直接回答")]);
    let cache: Arc<dyn CacheStrategy> = Arc::new(ImplicitPrefixCacheStrategy::new());
    let mut loop_ = make_loop(mock, Some(cache.clone()), None); // 未配置上限 → 关闭压缩

    let text = loop_.run_turn("任务", &mut |_| {}).await.unwrap();
    assert_eq!(text, "直接回答");
    assert_eq!(cache.invalidations(), 0);
    // 历史原样保留,首条消息未被摘要替换
    assert_eq!(loop_.history[0], Message::user_text("长".repeat(200)));
}

#[tokio::test]
async fn cache_strategy_receives_prepare_across_turns() {
    let (mock, _seen) = CapturingMock::new(vec![text_script("ok"), text_script("ok2")]);
    let cache: Arc<dyn CacheStrategy> = Arc::new(ImplicitPrefixCacheStrategy::new());
    let mut loop_ = make_loop(mock, Some(cache.clone()), None);

    loop_.run_turn("任务一", &mut |_| {}).await.unwrap();
    loop_.run_turn("任务二", &mut |_| {}).await.unwrap();
    // 多轮历史是纯前缀追加 → 无违例、无断链
    assert_eq!(cache.violations(), 0);
    assert_eq!(cache.invalidations(), 0);
}
