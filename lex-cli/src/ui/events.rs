use lex_core::agent::ToolResultInfo;
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
