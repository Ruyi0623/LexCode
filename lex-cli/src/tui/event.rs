use lex_core::security::PendingAction;

/// agent/工具侧 → UI 线程 的事件(经 std mpsc,非阻塞 send)。
/// 含 oneshot::Sender(不可 Debug/Clone),故枚举不加 derive。
pub enum UiEvent {
    TurnStarted,
    TextDelta(String),
    ThinkingDelta(String),
    ToolStart { name: String },
    ToolResult { tool_name: String, first_line: String, is_error: bool },
    Usage { input: u64, output: u64, cache_hit: u64 },
    Confirm { action: PendingAction, responder: tokio::sync::oneshot::Sender<bool> },
    TurnDone { ok: bool, message: String },
    Exit,
}

/// UI 线程 → agent 侧 的命令(经 tokio mpsc,同步线程用 blocking_send)
pub enum UiCommand {
    Submit(String),
    Interrupt,
    Quit,
}
