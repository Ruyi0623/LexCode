use lex_core::security::PendingAction;

use crate::ui::settings::SettingsView;

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
    /// 打开设置页(只读快照):agent 侧构建(需要读 agent 统计),UI 侧渲染与按键路由
    OpenSettings(SettingsView),
    /// 上下文容量:已用 token 估算 / 配置上限(0 = 未启用压缩)
    ContextUsage { used: u64, limit: u64 },
    TurnDone { ok: bool, message: String },
    Exit,
}

/// UI 线程 → agent 侧 的命令(经 tokio mpsc,同步线程用 blocking_send)
pub enum UiCommand {
    Submit(String),
    /// 设置页字段保存:整份新配置送 agent 侧热生效
    HotApply(Box<lex_core::config::Config>),
    Interrupt,
    Quit,
}
