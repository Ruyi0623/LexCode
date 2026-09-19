use lex_core::agent::{ChildEvent, ChildEventKind, ToolResultInfo};
use lex_core::message::Usage;
use lex_core::provider::ProviderEvent;
use serde_json::Value;

use crate::ui::markdown::MarkdownStream;
use crate::ui::theme;

/// 有状态事件渲染器:思考流分轨、工具活动行(● 工具(摘要)+ ⎿ 结果首行)、
/// token 尾注、Markdown 适配(行缓冲,整行输出)。
/// 带工具调用的轮次,其 token 尾注延迟到全部 ⎿ 结果行之后打印(保持阅读时序)。
pub struct Renderer {
    in_thinking: bool,
    hint_visible: bool,
    pending_tools: usize,
    pending_usage: Option<Usage>,
    md: MarkdownStream,
}

impl Renderer {
    pub fn new() -> Self {
        Renderer {
            in_thinking: false,
            hint_visible: false,
            pending_tools: 0,
            pending_usage: None,
            md: MarkdownStream::new(),
        }
    }

    fn flush() {
        use std::io::Write;
        std::io::stdout().flush().ok();
    }

    fn break_thinking(&mut self) {
        if self.in_thinking {
            anstream::println!();
            self.in_thinking = false;
        }
    }

    /// 发请求前调用:显示思考中占位行(首个事件到达时擦除)
    pub fn thinking_hint(&mut self) {
        self.hint_visible = true;
        anstream::print!("{}✻ 思考中…{}", theme::DIM, theme::RESET);
        Self::flush();
    }

    fn erase_hint(&mut self) {
        if self.hint_visible {
            anstream::print!("{}", theme::CLEAR_LINE);
            self.hint_visible = false;
        }
    }

    /// 工具执行完成后由 on_tool_result 回调调用,打印 ⎿ 结果首行
    pub fn tool_result(&mut self, info: &ToolResultInfo) {
        self.break_thinking();
        if info.is_error {
            anstream::println!("  {}⎿ {}{}", theme::ERROR, info.first_line, theme::RESET);
        } else {
            anstream::println!("  {}⎿ {}{}", theme::DIM, info.first_line, theme::RESET);
        }
        self.pending_tools = self.pending_tools.saturating_sub(1);
        if self.pending_tools == 0 {
            self.flush_pending_usage();
        }
    }

    /// 子 agent 活动行(由 ChildEventHook 回调驱动)。
    /// 有意**不触碰** `pending_tools`:子 agent 的 ●/⎿ 自成一套,父级的轮次尾注只由父级自己的工具决定。
    ///
    /// `erase_hint()` 是本渲染器自身的**一致性契约**:凡要往 stdout 打整行的出口都必须先擦掉
    /// 「✻ 思考中…」占位行,否则会留下「半行 hint + 新行挤在同一行」的错乱状态(同 `render()`
    /// 的 `ToolUseStart` 分支顺序:erase_hint → break_thinking → flush_markdown → 输出)。
    ///
    /// 定性说明(勿改写):当前装配下两个 `Renderer` 实例分离 —— `thinking_hint()` 只在
    /// `main.rs` 交互模式那份实例上调用,而 `child_event` 经 `make_child_event_hook` 落在 `run()`
    /// 层那份实例上,故本实例的 `hint_visible` **恒为 false**,这次补的 `erase_hint()` 今日是
    /// no-op,**并非修复某个用户可见缺陷**。它消除的是渲染器内部的潜在一致性缺口,等 TUI 把
    /// 两个渲染器合流后才会显形。
    pub fn child_event(&mut self, ev: &ChildEvent) {
        let Some(line) = render_child_event(ev) else { return };
        self.erase_hint();
        self.break_thinking();
        self.flush_markdown();
        anstream::println!("{line}");
    }

    /// 打印延迟的轮次 token 尾注(若未收到 Completed 则无事发生)
    fn flush_pending_usage(&mut self) {
        if let Some(usage) = self.pending_usage.take() {
            self.print_usage(&usage);
        }
    }

    fn print_usage(&mut self, usage: &Usage) {
        anstream::println!();
        if usage.cache_hit_tokens > 0 {
            anstream::println!(
                "{}(输入 {} tokens · 输出 {} tokens · 缓存命中 {}){}",
                theme::DIM, usage.input_tokens, usage.output_tokens, usage.cache_hit_tokens, theme::RESET
            );
        } else {
            anstream::println!(
                "{}(输入 {} tokens · 输出 {} tokens){}",
                theme::DIM, usage.input_tokens, usage.output_tokens, theme::RESET
            );
        }
    }

    /// 输出 Markdown 残留行(工具行/尾注前调用,保持输出顺序)
    fn flush_markdown(&mut self) {
        self.md.flush(&mut |s| {
            anstream::print!("{s}");
            Self::flush();
        });
    }

    pub fn render(&mut self, event: &ProviderEvent) {
        match event {
            ProviderEvent::TextDelta(t) => {
                self.erase_hint();
                self.break_thinking();
                self.md.feed(t, &mut |s| {
                    anstream::print!("{s}");
                    Self::flush();
                });
            }
            ProviderEvent::ThinkingDelta(t) => {
                self.erase_hint();
                self.in_thinking = true;
                anstream::print!("{}{t}{}", theme::THINKING, theme::RESET);
                Self::flush();
            }
            ProviderEvent::ToolUseStart { .. } => {
                self.erase_hint();
                self.break_thinking();
                self.flush_markdown(); // 残留半行先落盘,再打工具行
            }
            ProviderEvent::ToolUseDelta { .. } => {}
            ProviderEvent::ToolUseComplete { name, input, .. } => {
                self.erase_hint();
                self.break_thinking();
                self.pending_tools += 1;
                let summary = summarize(name, input);
                anstream::println!(
                    "{}●{} {}",
                    theme::ACCENT,
                    theme::RESET,
                    theme::accent(&format!("{name}({summary})"))
                );
            }
            ProviderEvent::Completed { usage } => {
                self.erase_hint();
                self.break_thinking();
                self.flush_markdown();
                if self.pending_tools > 0 {
                    // 轮次带工具调用:尾注等 ⎿ 结果行打完再出,保持阅读时序
                    self.pending_usage = Some(usage.clone());
                } else {
                    self.print_usage(usage);
                }
            }
        }
    }
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

/// 子 agent 事件的单行渲染。返回 None 表示该事件不输出。
/// 颜色只引用 theme.rs 常量(项目硬约束);归属由 [子N] 标签承载,
/// 整行用 DIM 以区别于父级的 ACCENT ●。
fn render_child_event(ev: &ChildEvent) -> Option<String> {
    match &ev.kind {
        ChildEventKind::Started { task } => {
            let first: String = task.lines().next().unwrap_or_default().chars().take(80).collect();
            Some(format!("  {}⤷ [子{}] 派生: {}{}", theme::DIM, ev.child_id, first, theme::RESET))
        }
        ChildEventKind::ToolCall { name, input } => Some(format!(
            "  {}● [子{}] {}({}){}",
            theme::DIM,
            ev.child_id,
            name,
            summarize(name, input),
            theme::RESET
        )),
        ChildEventKind::ToolResult { first_line, is_error, .. } => {
            let color = if *is_error { theme::ERROR } else { theme::DIM };
            Some(format!("  {}⎿ [子{}] {}{}", color, ev.child_id, first_line, theme::RESET))
        }
        // 子 agent 的结局已由父级那条 ⎿ 行(摘要首行)体现,再打一行是冗余噪声
        ChildEventKind::Finished { .. } => None,
    }
}

/// 工具参数单行摘要(≤80 字符)
fn summarize(name: &str, input: &Value) -> String {
    let raw = match name {
        "bash_exec" => input.get("command").and_then(Value::as_str).unwrap_or("<未知命令>"),
        "file_read" | "file_edit" => input.get("path").and_then(Value::as_str).unwrap_or("<未知路径>"),
        "grep_search" => input.get("pattern").and_then(Value::as_str).unwrap_or("<未知模式>"),
        "todo_write" => "更新待办清单",
        "spawn_subagent" => input.get("task").and_then(Value::as_str).unwrap_or("<未知任务>"),
        _ => "",
    };
    let mut s: String = raw.trim().chars().take(80).collect();
    if raw.chars().count() > 80 {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(id: u32, kind: ChildEventKind) -> ChildEvent {
        ChildEvent { child_id: id, depth: 1, kind }
    }

    #[test]
    fn summarize_covers_spawn_subagent_task() {
        // 既有的 spawn_subagent 分支不得回归
        assert_eq!(summarize("spawn_subagent", &serde_json::json!({"task": "排查"})), "排查");
        assert_eq!(summarize("spawn_subagent", &serde_json::json!({})), "<未知任务>");
    }

    #[test]
    fn rendered_child_events_carry_attribution_and_finished_is_silent() {
        // 归属标记 [子N] 必须出现在每一条会输出的事件里;
        // Finished 不输出(其结局已由父级 ⎿ 行体现)
        let started = render_child_event(&ev(2, ChildEventKind::Started { task: "排查".into() }));
        assert!(started.as_deref().unwrap_or_default().contains("[子2]"), "Started: {started:?}");
        assert!(started.as_deref().unwrap_or_default().contains("排查"));

        let call = render_child_event(&ev(2, ChildEventKind::ToolCall {
            name: "file_read".into(),
            input: serde_json::json!({"path": "a.rs"}),
        }));
        assert!(call.as_deref().unwrap_or_default().contains("[子2]"), "ToolCall: {call:?}");
        assert!(call.as_deref().unwrap_or_default().contains("a.rs"), "应复用 summarize 的参数摘要: {call:?}");

        let ok = render_child_event(&ev(2, ChildEventKind::ToolResult {
            name: "file_read".into(), first_line: "读到了".into(), is_error: false,
        }));
        assert!(ok.as_deref().unwrap_or_default().contains("[子2]"), "ToolResult: {ok:?}");

        assert!(render_child_event(&ev(2, ChildEventKind::Finished { summary_first_line: "done".into() })).is_none(),
            "Finished 不应输出");
    }

    #[test]
    fn child_events_do_not_disturb_parent_pending_tools() {
        // 计数平衡:子 agent 的活动不得触碰父级的 pending_tools。
        // 旧实现下子级的 ⎿ 会减该计数却无 ● 来加,使父级 token 尾注提前打印。
        let mut r = Renderer::new();
        r.pending_tools = 2;
        r.child_event(&ev(1, ChildEventKind::Started { task: "排查".into() }));
        r.child_event(&ev(1, ChildEventKind::ToolCall { name: "file_read".into(), input: serde_json::json!({"path": "a"}) }));
        r.child_event(&ev(1, ChildEventKind::ToolResult { name: "file_read".into(), first_line: "ok".into(), is_error: false }));
        r.child_event(&ev(1, ChildEventKind::Finished { summary_first_line: "done".into() }));
        assert_eq!(r.pending_tools, 2, "子事件不得改动父级计数");
    }
}
