use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lex_core::security::PendingAction;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::ui::theme;

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
    /// 最后一行是否为"正在流式写入"的正文行(决定下一片 token 续接还是另起一行)
    text_open: bool,
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
            text_open: false,
        }
    }

    /// 事件应用:全部渲染状态变更只发生在 UI 线程
    pub fn apply(&mut self, ev: UiEvent, todos: &std::sync::Mutex<Vec<lex_core::tools::Todo>>) {
        match ev {
            UiEvent::TurnStarted => {
                self.busy = true;
                self.status = "思考中…".into();
                self.usage = None;
                // 新一轮的正文不得续接到上一轮结尾
                self.text_open = false;
            }
            UiEvent::TextDelta(t) => {
                for (i, seg) in t.split('\n').enumerate() {
                    if i == 0 {
                        // 首片续接当前行(流式 token 逐片到达)
                        self.append_text(seg);
                    } else {
                        // '\n' 之后的分片各自开一行(末片为空时留作待续接的行)
                        self.transcript.push(Line::from(seg.to_string()));
                        self.text_open = true;
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
                // 工具结果行之后的正文另起一行,不续到 ⎿ 行上
                self.text_open = false;
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
                        self.text_open = false;
                    }
                }
            }
            UiEvent::Exit => {
                self.quit = true;
            }
        }
    }

    /// 测试辅助:直接构造无 responder 的弹层(真实路径经 `apply(UiEvent::Confirm)`)
    #[cfg(test)]
    pub fn open_confirm(&mut self, action: PendingAction) {
        let (tx, _rx) = tokio::sync::oneshot::channel();
        self.pending_confirm = Some(ConfirmModal { action, responder: tx });
    }

    /// 回显用户提交的输入:输入盒提交后会清空,不回显就等于"自己说过的话凭空消失"
    pub fn push_user_input(&mut self, text: &str) {
        self.transcript.push(Line::from(vec![
            Span::styled("› ", Style::default().fg(theme::C_ACCENT)),
            Span::raw(text.to_string()),
        ]));
        self.text_open = false;
    }

    /// 追加一段流式正文到"当前行":token 逐片到达,同一段回复必须续在同一行。
    /// `text_open == false`(工具结果/用户回显/新一轮之后)时另起一行。
    fn append_text(&mut self, seg: &str) {
        if self.text_open {
            if let Some(last) = self.transcript.last_mut() {
                if !seg.is_empty() {
                    last.spans.push(Span::raw(seg.to_string()));
                }
                return;
            }
        }
        if seg.is_empty() {
            return; // 空片段不建行,避免误插空行
        }
        self.transcript.push(Line::from(seg.to_string()));
        self.text_open = true;
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

    fn line_text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// 回归:正文按 token 流式到达,同一段回复必须续在同一行
    /// (曾把每个 TextDelta 都当新行 → 整段回复被拆成"一行一个词")
    #[test]
    fn consecutive_text_deltas_share_one_line() {
        let mut app = AppState::new();
        app.apply(UiEvent::TextDelta("排查".into()), &no_todos());
        app.apply(UiEvent::TextDelta("某个".into()), &no_todos());
        app.apply(UiEvent::TextDelta("bug".into()), &no_todos());
        assert_eq!(app.transcript.len(), 1, "同段回复不应拆成多行");
        assert_eq!(line_text(&app.transcript[0]), "排查某个bug");
    }

    #[test]
    fn newline_in_delta_breaks_line_and_next_delta_continues() {
        let mut app = AppState::new();
        app.apply(UiEvent::TextDelta("第一行\n第二".into()), &no_todos());
        app.apply(UiEvent::TextDelta("行".into()), &no_todos());
        assert_eq!(app.transcript.len(), 2);
        assert_eq!(line_text(&app.transcript[0]), "第一行");
        assert_eq!(line_text(&app.transcript[1]), "第二行");
    }

    /// 工具结果行之后到达的正文必须另起一行,不能续到 ⎿ 行上
    #[test]
    fn text_after_tool_result_starts_new_line() {
        let mut app = AppState::new();
        app.apply(UiEvent::TextDelta("先看文件".into()), &no_todos());
        app.apply(
            UiEvent::ToolResult { tool_name: "file_read".into(), first_line: "读到 3 行".into(), is_error: false },
            &no_todos(),
        );
        app.apply(UiEvent::TextDelta("结论如下".into()), &no_todos());
        assert_eq!(app.transcript.len(), 3);
        assert!(line_text(&app.transcript[1]).starts_with("  ⎿ 读到 3 行"));
        assert_eq!(line_text(&app.transcript[2]), "结论如下");
    }

    /// 用户提交的话必须回显,否则输入盒清空后自己说了什么就看不到了
    #[test]
    fn submitted_input_is_echoed_into_transcript() {
        let mut app = AppState::new();
        app.push_user_input("修一下这个 bug");
        assert_eq!(app.transcript.len(), 1);
        assert_eq!(line_text(&app.transcript[0]), "› 修一下这个 bug");
        // 回显之后模型正文另起一行
        app.apply(UiEvent::TextDelta("好的".into()), &no_todos());
        assert_eq!(app.transcript.len(), 2);
        assert_eq!(line_text(&app.transcript[1]), "好的");
    }

    /// 新一轮开始:上一轮的正文不再被续接
    #[test]
    fn new_turn_does_not_append_to_previous_reply() {
        let mut app = AppState::new();
        app.apply(UiEvent::TextDelta("上一轮结尾".into()), &no_todos());
        app.apply(UiEvent::TurnStarted, &no_todos());
        app.apply(UiEvent::TextDelta("本轮开头".into()), &no_todos());
        assert_eq!(app.transcript.len(), 2);
        assert_eq!(line_text(&app.transcript[0]), "上一轮结尾");
        assert_eq!(line_text(&app.transcript[1]), "本轮开头");
    }
}
