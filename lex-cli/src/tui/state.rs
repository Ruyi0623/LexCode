use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lex_core::security::PendingAction;
use ratatui::text::Line;

use super::event::UiEvent;

pub struct ConfirmModal {
    pub action: PendingAction,
    pub responder: tokio::sync::oneshot::Sender<bool>,
}

pub struct AppState {
    pub transcript: Vec<Line<'static>>,
    pub todos: Vec<lex_core::tools::Todo>,
    pub input: String,
    pub status: String,
    pub busy: bool,
    pub usage: Option<String>,
    pub pending_confirm: Option<ConfirmModal>,
    pub quit: bool,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            transcript: Vec::new(),
            todos: Vec::new(),
            input: String::new(),
            status: "就绪".into(),
            busy: false,
            usage: None,
            pending_confirm: None,
            quit: false,
        }
    }

    /// 事件应用:全部渲染状态变更只发生在 UI 线程
    pub fn apply(&mut self, ev: UiEvent, todos: &std::sync::Mutex<Vec<lex_core::tools::Todo>>) {
        match ev {
            UiEvent::TurnStarted => {
                self.busy = true;
                self.status = "思考中…".into();
                self.usage = None;
            }
            UiEvent::TextDelta(t) => {
                for (i, seg) in t.split('\n').enumerate() {
                    if i > 0 {
                        self.transcript.push(Line::from("".to_string()));
                    }
                    if !seg.is_empty() {
                        self.transcript.push(Line::from(seg.to_string()));
                    }
                }
            }
            UiEvent::ThinkingDelta(text) => {
                // 纯文本路径会把思考内容打到屏幕上;TUI 无回滚区,
                // 退而在状态行显示"最后一行"的截断片段(避免逐 token 抖动时整屏铺开)
                let snippet: String = text.lines().last().unwrap_or("").trim().chars().take(40).collect();
                self.status = if snippet.is_empty() {
                    "思考中…".into()
                } else {
                    format!("思考中… {snippet}")
                };
            }
            UiEvent::ToolStart { name } => {
                self.status = format!("执行 {name}…");
            }
            UiEvent::ToolResult { tool_name, first_line, is_error } => {
                let mark = if is_error { "⎿ ✗ " } else { "⎿ " };
                self.transcript.push(Line::from(format!("  {mark}{first_line}")));
                if tool_name == "todo_write" {
                    self.todos = todos.lock().unwrap_or_else(|p| p.into_inner()).clone();
                }
            }
            UiEvent::Usage { input, output, cache_hit } => {
                self.usage = Some(if cache_hit > 0 {
                    format!("输入 {input} · 输出 {output} · 缓存命中 {cache_hit}")
                } else {
                    format!("输入 {input} · 输出 {output}")
                });
            }
            UiEvent::Confirm { action, responder } => {
                self.pending_confirm = Some(ConfirmModal { action, responder });
            }
            UiEvent::TurnDone { ok, message } => {
                self.busy = false;
                if ok {
                    self.status = "就绪".into();
                } else {
                    self.status = "本轮已中断/失败".into();
                    if !message.is_empty() {
                        self.transcript.push(Line::from(format!("⚠ {message}")));
                    }
                }
            }
            UiEvent::Exit => {
                self.quit = true;
            }
        }
    }

    pub fn open_confirm(&mut self, action: PendingAction) {
        // 测试辅助:直接构造无 responder 的弹层
        let (tx, _rx) = tokio::sync::oneshot::channel();
        self.pending_confirm = Some(ConfirmModal { action, responder: tx });
    }

    fn answer_confirm(&mut self, allow: bool) {
        if let Some(modal) = self.pending_confirm.take() {
            let _ = modal.responder.send(allow);
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> KeyOutcome {
        if self.pending_confirm.is_some() {
            return match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.answer_confirm(true);
                    KeyOutcome::Confirm(true)
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.answer_confirm(false);
                    KeyOutcome::Confirm(false)
                }
                _ => KeyOutcome::None,
            };
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                if self.busy {
                    KeyOutcome::Interrupt
                } else {
                    self.quit = true;
                    KeyOutcome::Quit
                }
            }
            (KeyCode::Enter, _) if !self.input.trim().is_empty() => {
                let text = std::mem::take(&mut self.input);
                KeyOutcome::Submit(text)
            }
            (KeyCode::Backspace, _) => {
                self.input.pop();
                KeyOutcome::None
            }
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                self.input.clear();
                KeyOutcome::None
            }
            (KeyCode::Char(c), m) if !m.contains(KeyModifiers::CONTROL) => {
                self.input.push(c);
                KeyOutcome::None
            }
            _ => KeyOutcome::None,
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, PartialEq)]
pub enum KeyOutcome {
    None,
    Submit(String),
    Confirm(bool),
    Interrupt,
    Quit,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_todos() -> std::sync::Mutex<Vec<lex_core::tools::Todo>> {
        std::sync::Mutex::new(Vec::new())
    }

    /// 纯文本路径会把思考内容打到屏幕上;TUI 无回滚区,退而在状态行显示最新片段
    #[test]
    fn thinking_delta_shows_snippet_in_status() {
        let mut app = AppState::new();
        app.apply(UiEvent::ThinkingDelta("正在分析缓存前缀".into()), &no_todos());
        assert!(app.status.contains("正在分析缓存前缀"), "状态行应反映思考内容,实际: {}", app.status);
    }

    #[test]
    fn thinking_snippet_is_truncated_and_takes_last_line() {
        let mut app = AppState::new();
        let long = "第一行\n".to_string() + &"很长的一段思考".repeat(20);
        app.apply(UiEvent::ThinkingDelta(long), &no_todos());
        assert!(app.status.contains("很长"), "应取最后一行内容,实际: {}", app.status);
        assert!(!app.status.contains("第一行"), "只取最后一行,实际: {}", app.status);
        assert!(app.status.chars().count() <= 60, "状态片段须截断,实际长度 {}", app.status.chars().count());
    }

    #[test]
    fn empty_thinking_keeps_plain_hint() {
        let mut app = AppState::new();
        app.apply(UiEvent::ThinkingDelta(String::new()), &no_todos());
        assert_eq!(app.status, "思考中…");
    }
}
