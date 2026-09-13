use lex_core::agent::ToolResultInfo;
use lex_core::provider::ProviderEvent;
use serde_json::Value;

use crate::ui::theme;

/// 有状态事件渲染器:思考流分轨、工具活动行(● 工具(摘要)+ ⎿ 结果首行)、token 尾注。
pub struct Renderer {
    in_thinking: bool,
    hint_visible: bool,
}

impl Renderer {
    pub fn new() -> Self {
        Renderer { in_thinking: false, hint_visible: false }
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
    }

    pub fn render(&mut self, event: &ProviderEvent) {
        match event {
            ProviderEvent::TextDelta(t) => {
                self.erase_hint();
                self.break_thinking();
                anstream::print!("{t}");
                Self::flush();
            }
            ProviderEvent::ThinkingDelta(t) => {
                self.erase_hint();
                self.in_thinking = true;
                anstream::print!("{}{t}{}", theme::THINKING, theme::RESET);
                Self::flush();
            }
            ProviderEvent::ToolUseStart { .. } => {
                self.erase_hint();
                self.break_thinking(); // 参数未齐,不在 Start 打印
            }
            ProviderEvent::ToolUseDelta { .. } => {}
            ProviderEvent::ToolUseComplete { name, input, .. } => {
                self.erase_hint();
                self.break_thinking();
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
        _ => "",
    };
    let mut s: String = raw.trim().chars().take(80).collect();
    if raw.chars().count() > 80 {
        s.push('…');
    }
    s
}
