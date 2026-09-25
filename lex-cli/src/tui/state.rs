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
    /// 当前流式正文行的原始文本(未渲染 markdown);行关闭时整行过 `tui::markdown` 渲染
    text_buf: String,
    /// markdown 跨行渲染状态(围栏开合 + 表格块缓冲),只在正文行渲染时推进,不跨轮保留
    md: super::markdown::MarkdownState,
    /// 对话回看偏移(按显示行计,0 = 跟随最新);PageUp/PageDown/滚轮调节
    pub scroll: usize,
    /// 输入光标(按字符计,0 = 行首);←/→/Home/End 移动,插入/删除发生在光标处
    pub cursor: usize,
    /// 设置页(只读快照):Some 时全屏渲染并接管按键,Esc/q 关闭
    pub settings: Option<(crate::ui::settings::SettingsView, crate::ui::settings::PageState)>,
    /// 确认弹层内容的滚动偏移(按显示行计,0 = 顶部);弹层为最上层时滚轮/PgUp/PgDn 只滚弹层
    pub confirm_scroll: usize,
    /// 上下文容量(已用 token 估算,配置上限;None = 未上报)
    pub ctx_usage: Option<(u64, u64)>,
    /// 斜杠命令补全:候选菜单中的选中下标(候选由当前输入实时推导)
    pub completion_sel: usize,
    /// 设置页字段编辑中:输入框临时接管为字段编辑器(True 时 Enter/Esc 特殊处理)
    pub settings_edit: bool,
    /// 字段编辑前的主输入暂存(取消/保存后恢复)
    input_stash: Option<String>,
    /// 设置页最近一次操作结果(展示在页面底部;(文案, 是否成功))
    pub settings_msg: Option<(String, bool)>,
    /// 待发送的配置热生效命令(字段保存后由 ui_loop 取走发往 agent 侧)
    settings_commit: Option<Box<lex_core::config::Config>>,
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
            text_buf: String::new(),
            md: super::markdown::MarkdownState::new(),
            scroll: 0,
            cursor: 0,
            settings: None,
            confirm_scroll: 0,
            ctx_usage: None,
            completion_sel: 0,
            settings_edit: false,
            input_stash: None,
            settings_msg: None,
            settings_commit: None,
        }
    }

    /// 事件应用:全部渲染状态变更只发生在 UI 线程
    pub fn apply(&mut self, ev: UiEvent, todos: &std::sync::Mutex<Vec<lex_core::tools::Todo>>) {
        match ev {
            UiEvent::TurnStarted => {
                self.busy = true;
                self.status = "思考中…".into();
                self.usage = None;
                // 上一轮未关闭的正文行与表格块先落定,markdown 状态不跨轮保留
                self.close_open_text(true);
                self.md.reset();
            }
            UiEvent::TextDelta(t) => {
                for (i, seg) in t.split('\n').enumerate() {
                    if i == 0 {
                        // 首片续接当前行(流式 token 逐片到达)
                        self.append_text(seg);
                    } else {
                        // '\n' 意味着上一行已完整:整行过 markdown 渲染后另起新行
                        // (末片为空时留作待续接的行)
                        self.close_open_text(false);
                        self.open_text_line(seg);
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
                // 正文行到工具结果为止:先落定,⎿ 行不能被续写也不参与 markdown 渲染
                self.close_open_text(true);
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
                self.confirm_scroll = 0; // 每次弹层从顶部看起
            }
            UiEvent::OpenSettings(view) => {
                self.settings = Some((view, crate::ui::settings::PageState::new()));
            }
            UiEvent::ContextUsage { used, limit } => {
                self.ctx_usage = Some((used, limit));
            }
            UiEvent::TurnDone { ok, message } => {
                self.busy = false;
                // 轮次结束:未关闭的正文行(无换行结尾的最后一行)在此落定渲染
                self.close_open_text(true);
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
        self.close_open_text(true);
        self.scroll = 0; // 新输入跳回底部跟随
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
                    self.text_buf.push_str(seg);
                    last.spans.push(Span::raw(seg.to_string()));
                }
                return;
            }
            // text_open 却无可续写行(异常状态):降级按新行处理
            self.text_open = false;
        }
        if seg.is_empty() {
            return; // 空片段不建行,避免误插空行
        }
        self.open_text_line(seg);
    }

    /// 新开一行流式正文:先以原始文本入列(token 逐片可见),关闭时才整行做 markdown 渲染
    fn open_text_line(&mut self, seg: &str) {
        self.text_buf = seg.to_string();
        self.transcript.push(Line::from(seg.to_string()));
        self.text_open = true;
    }

    /// 关闭当前流式正文行:整行过 `tui::markdown` 渲染,替换为带样式 Span。
    /// 围栏标记行渲染为空 Span(占住行位,避免后续行序漂移);
    /// 渲染可能产出多行(表格整块对齐输出),首行替换占位、其余追加。
    /// `final_drain`:是否把缓冲中的表格块一并落定。行间换行关闭传 false
    /// (表格可能还没写完,不能冲掉);工具结果/用户回显/轮次结束等
    /// "后续不再是本段正文"的边界传 true。
    fn close_open_text(&mut self, final_drain: bool) {
        let mut table_placeholder_popped = false;
        if self.text_open {
            self.text_open = false;
            let raw = std::mem::take(&mut self.text_buf);
            let outputs = self.md.render_line(&raw);
            if outputs.is_empty() && !self.md.table_is_buffering() {
                // 围栏标记行:空占位,保住后续行序
                if let Some(last) = self.transcript.last_mut() {
                    last.spans = Vec::new();
                }
            } else if outputs.is_empty() {
                // 该行被缓冲进表格块:占位行直接移除,整块对齐后从当前位置输出,
                // 避免表格前堆一排空行
                table_placeholder_popped = true;
            } else if let Some(last) = self.transcript.last_mut() {
                last.spans = Vec::new();
            }
            if table_placeholder_popped {
                self.transcript.pop();
            }
            // 非空输出:首行替换占位、其余追加(表格块 + 当前行或普通行)
            if !outputs.is_empty() {
                let mut iter = outputs.into_iter();
                if let Some(last) = self.transcript.last_mut() {
                    last.spans = iter.next().unwrap_or_default();
                }
                for spans in iter {
                    self.transcript.push(Line::from(spans));
                }
            }
        }
        if final_drain {
            let table_outputs = self.md.take_table();
            if !table_outputs.is_empty() {
                if table_placeholder_popped {
                    for spans in table_outputs {
                        self.transcript.push(Line::from(spans));
                    }
                } else {
                    place_outputs(&mut self.transcript, table_outputs);
                }
            }
        }
    }

    /// 取走待热生效的配置(字段保存后由 ui_loop 发往 agent 侧)
    pub fn take_settings_commit(&mut self) -> Option<Box<lex_core::config::Config>> {
        self.settings_commit.take()
    }

    /// 进入字段编辑:主输入暂存,字段当前值载入输入框
    fn begin_settings_edit(&mut self) {
        let (module, field) = match &self.settings {
            Some((_, page)) => (page.selected, page.field_sel),
            None => return,
        };
        self.input_stash = Some(std::mem::take(&mut self.input));
        self.input = crate::ui::settings::field_display(&self.settings.as_ref().unwrap().0, module, field);
        self.cursor = self.input.chars().count();
        if let Some((_, page)) = self.settings.as_mut() {
            page.editing = true;
        }
        self.settings_edit = true;
        self.settings_msg = None;
    }

    /// 取消编辑:恢复主输入
    fn cancel_settings_edit(&mut self) {
        if let Some(stash) = self.input_stash.take() {
            self.input = stash;
            self.cursor = self.input.chars().count();
        }
        self.settings_edit = false;
        if let Some((_, page)) = self.settings.as_mut() {
            page.editing = false;
        }
    }

    /// 保存编辑:应用配置改动 + 写回 toml,结果进页面消息;成功则排队热生效
    fn commit_settings_edit(&mut self) {
        let (module, field) = match &self.settings {
            Some((_, page)) => (page.selected, page.field_sel),
            None => return,
        };
        let res = crate::ui::settings::apply_edit(&mut self.settings.as_mut().unwrap().0, module, field, &self.input);
        match res {
            Ok(msg) => {
                self.settings_msg = Some((msg, true));
                self.settings_commit = Some(Box::new(self.settings.as_ref().unwrap().0.config.clone()));
            }
            Err(msg) => self.settings_msg = Some((msg, false)),
        }
        if let Some(stash) = self.input_stash.take() {
            self.input = stash;
            self.cursor = self.input.chars().count();
        }
        self.settings_edit = false;
        if let Some((_, page)) = self.settings.as_mut() {
            page.editing = false;
        }
    }

    /// 斜杠命令补全候选:输入为 "/" 开头且不含空格时,按前缀过滤命令表
    pub fn completion_candidates(&self) -> Vec<&'static str> {
        let t = self.input.trim();
        // 尾随空格 = 已进入参数区,补全关闭(用原始输入判断,trim 会吃掉尾空格)
        if !t.starts_with('/') || self.input.contains(' ') {
            return Vec::new();
        }
        crate::ui::settings::command_names()
            .iter()
            .copied()
            .filter(|c| c.starts_with(t))
            .collect()
    }

    /// Tab 补全:把输入替换为当前选中的候选命令
    pub fn complete_input(&mut self) {
        let cands = self.completion_candidates();
        if cands.is_empty() {
            return;
        }
        let sel = self.completion_sel.min(cands.len() - 1);
        self.input = cands[sel].to_string();
        self.cursor = self.input.chars().count();
    }

    /// 第 char_idx 个字符的字节偏移(String::remove/insert 需要)
    fn char_byte_index(&self, char_idx: usize) -> usize {
        self.input.chars().take(char_idx).map(char::len_utf8).sum()
    }

    fn answer_confirm(&mut self, allow: bool) {
        if let Some(modal) = self.pending_confirm.take() {
            let _ = modal.responder.send(allow);
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> KeyOutcome {
        // 设置页接管按键;Ctrl+C 例外,仍走打断/退出
        if self.settings.is_some() {
            let editing = self.settings_edit;
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                // 落到下方常规分支处理
            } else if editing {
                // 编辑态:Enter 保存 / Esc 取消,其余按键落进输入编辑分支
                match key.code {
                    KeyCode::Enter => {
                        self.commit_settings_edit();
                        return KeyOutcome::None;
                    }
                    KeyCode::Esc => {
                        self.cancel_settings_edit();
                        return KeyOutcome::None;
                    }
                    _ => {}
                }
            } else {
                let module = self.settings.as_ref().map(|(_, p)| p.selected).unwrap_or(0);
                let action = self
                    .settings
                    .as_mut()
                    .map(|(_, page)| page.handle_key(module, key.code))
                    .unwrap_or(crate::ui::settings::PageAction::None);
                match action {
                    crate::ui::settings::PageAction::Quit => self.settings = None,
                    crate::ui::settings::PageAction::EditField(_) => self.begin_settings_edit(),
                    _ => {}
                }
                return KeyOutcome::None;
            }
        }
        if self.pending_confirm.is_some() {
            return match key.code {
                // 弹层为最上层时,PgUp/PgDn 滚弹层内容而不是主对话
                KeyCode::PageUp => {
                    self.confirm_scroll = self.confirm_scroll.saturating_sub(10);
                    KeyOutcome::None
                }
                KeyCode::PageDown => {
                    self.confirm_scroll = self.confirm_scroll.saturating_add(10);
                    KeyOutcome::None
                }
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
            // 对话回看:PageUp 上翻 / PageDown 下翻(End 让位给输入光标"跳到行尾")
            (KeyCode::PageUp, _) => {
                self.scroll = self.scroll.saturating_add(10);
                KeyOutcome::None
            }
            (KeyCode::PageDown, _) => {
                self.scroll = self.scroll.saturating_sub(10);
                KeyOutcome::None
            }
            // 斜杠命令补全菜单打开时,↑/↓ 在候选间循环,Tab 补全
            (KeyCode::Up, _) if !self.completion_candidates().is_empty() => {
                let n = self.completion_candidates().len();
                self.completion_sel = (self.completion_sel + n - 1) % n;
                KeyOutcome::None
            }
            (KeyCode::Down, _) if !self.completion_candidates().is_empty() => {
                let n = self.completion_candidates().len();
                self.completion_sel = (self.completion_sel + 1) % n;
                KeyOutcome::None
            }
            (KeyCode::Tab, _) if !self.completion_candidates().is_empty() => {
                self.complete_input();
                KeyOutcome::None
            }
            // —— 输入行编辑:光标按字符移动,插入/删除发生在光标处 ——
            (KeyCode::Left, _) => {
                self.cursor = self.cursor.saturating_sub(1);
                KeyOutcome::None
            }
            (KeyCode::Right, _) => {
                self.cursor = (self.cursor + 1).min(self.input.chars().count());
                KeyOutcome::None
            }
            (KeyCode::Home, _) => {
                self.cursor = 0;
                KeyOutcome::None
            }
            (KeyCode::End, _) => {
                self.cursor = self.input.chars().count();
                KeyOutcome::None
            }
            (KeyCode::Backspace, _) => {
                if self.cursor > 0 {
                    let idx = self.char_byte_index(self.cursor - 1);
                    self.input.remove(idx);
                    self.cursor -= 1;
                }
                KeyOutcome::None
            }
            (KeyCode::Delete, _) => {
                if self.cursor < self.input.chars().count() {
                    let idx = self.char_byte_index(self.cursor);
                    self.input.remove(idx);
                }
                KeyOutcome::None
            }
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
                self.cursor = 0;
                self.completion_sel = 0;
                KeyOutcome::Submit(text)
            }
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                self.input.clear();
                self.cursor = 0;
                KeyOutcome::None
            }
            (KeyCode::Char(c), m) if !m.contains(KeyModifiers::CONTROL) => {
                let idx = self.char_byte_index(self.cursor);
                self.input.insert(idx, c);
                self.cursor += 1;
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

/// 渲染输出落位(作用于 transcript 最后一行):0 行 → 占位行置空(围栏标记的占位);
/// 多行(表格整块)→ 首行替换占位、其余按序追加
fn place_outputs(transcript: &mut Vec<Line<'static>>, outputs: Vec<Vec<Span<'static>>>) {
    let mut iter = outputs.into_iter();
    match iter.next() {
        None => {
            if let Some(last) = transcript.last_mut() {
                last.spans = Vec::new();
            }
        }
        Some(first) => {
            if let Some(last) = transcript.last_mut() {
                last.spans = first;
            }
            for spans in iter {
                transcript.push(Line::from(spans));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_event(code: KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::empty())
    }

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

    /// 正文行关闭(遇到换行)时整行做 markdown 渲染:标题行落定为加粗 Span
    #[test]
    fn closed_text_line_renders_markdown() {
        let mut app = AppState::new();
        app.apply(UiEvent::TextDelta("### 计划\n正文继续".into()), &no_todos());
        assert_eq!(line_text(&app.transcript[0]), "计划", "# 标记应被剥去");
        assert!(
            app.transcript[0].spans[0].style.add_modifier.contains(ratatui::style::Modifier::BOLD),
            "标题行应加粗"
        );
        assert_eq!(line_text(&app.transcript[1]), "正文继续");
    }

    /// 流式中的行保持原始文本,轮次结束(TurnDone)时落定渲染
    #[test]
    fn open_line_renders_on_turn_done() {
        let mut app = AppState::new();
        app.apply(UiEvent::TextDelta("看 `code` 就行".into()), &no_todos());
        let raw_before: String =
            app.transcript[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(raw_before, "看 `code` 就行", "流式期间应保持原文");
        app.apply(UiEvent::TurnDone { ok: true, message: String::new() }, &no_todos());
        assert!(
            app.transcript[0].spans.iter().any(|s| s.style.fg == Some(theme::C_CODE)),
            "落定后行内代码应为亮青"
        );
    }

    /// 围栏:标记行占空行、内容变暗;一条 delta 内的多行各自落定
    #[test]
    fn fence_lines_render_dim_with_placeholder_markers() {
        let mut app = AppState::new();
        app.apply(UiEvent::TextDelta("```rust\nfn a() {}\n```\n之后".into()), &no_todos());
        // 行序列:```(空占位) / fn a() {}(dim) / ```(空占位) / 之后(待续接)
        assert_eq!(app.transcript.len(), 4);
        assert_eq!(line_text(&app.transcript[0]), "", "围栏标记行应为空行占位");
        assert_eq!(app.transcript[1].spans[0].style.fg, Some(theme::C_DIM), "围栏内容应变暗");
        assert_eq!(line_text(&app.transcript[3]), "之后");
    }

    /// 工具结果到达时先落定未关闭的正文行:列表项渲染为强调蓝圆点
    #[test]
    fn tool_result_closes_open_markdown_line() {
        let mut app = AppState::new();
        app.apply(UiEvent::TextDelta("- 任务一".into()), &no_todos());
        app.apply(
            UiEvent::ToolResult { tool_name: "bash_exec".into(), first_line: "ok".into(), is_error: false },
            &no_todos(),
        );
        assert_eq!(app.transcript[0].spans[0].content.as_ref(), "•", "列表项应渲染为圆点");
        assert_eq!(app.transcript[0].spans[0].style.fg, Some(theme::C_ACCENT));
    }

    /// 表格作为本轮最后内容:TurnDone 时整块对齐落定,且表格前不堆空行
    #[test]
    fn table_at_turn_end_flushes_aligned() {
        let mut app = AppState::new();
        app.apply(
            UiEvent::TextDelta("| 工具 | 状态 |
|---|---|
| file_read | 通过 |
".into()),
            &no_todos(),
        );
        app.apply(UiEvent::TurnDone { ok: true, message: String::new() }, &no_todos());
        let texts: Vec<String> = app
            .transcript
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(
            texts.iter().any(|t| t.starts_with("│ 工具")),
            "表头应对齐落定: {texts:?}"
        );
        assert!(texts.iter().any(|t| t.starts_with("├")), "分隔行应输出 ├──┼──┤: {texts:?}");
        assert!(
            texts.iter().any(|t| t.starts_with("│ file_read")),
            "数据行应对齐落定: {texts:?}"
        );
        // 表格行之前不应有连续空行占位(占位行已被移除)
        let first_table = texts.iter().position(|t| t.starts_with("│")).unwrap();
        assert!(
            texts[..first_table].iter().all(|t| !t.is_empty()),
            "表格前不应有空行占位: {texts:?}"
        );
    }

    /// 斜杠命令补全:前缀过滤、↑/↓ 循环选择、Tab 补全、提交后重置
    #[test]
    fn slash_completion_filter_cycle_and_tab() {
        let mut app = AppState::new();
        let key = |code: KeyCode| crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::empty());
        app.handle_key(key(KeyCode::Char('/')));
        assert_eq!(app.completion_candidates().len(), 3, "空前缀应列出全部命令");
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.completion_sel, 1, "Down 循环选择");
        app.handle_key(key(KeyCode::Up));
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.completion_sel, 2, "Up 反向循环(3 → 2 → 1 → 0 → 2)");
        for c in "compact".chars() {
            app.handle_key(key_event(KeyCode::Char(c)));
        }
        let cands = app.completion_candidates();
        assert_eq!(cands, vec!["/compact"], "前缀过滤只剩 /compact");
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.input, "/compact", "Tab 应补全为选中命令");
        assert_eq!(app.cursor, 8, "补全后光标在行尾");
        // 带空格后不再出候选(进入参数区)
        app.handle_key(key(KeyCode::Char(' ')));
        assert!(app.completion_candidates().is_empty(), "含空格后不补全");
        app.handle_key(key(KeyCode::Backspace));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.completion_sel, 0, "提交后选中下标重置");
    }

    /// 设置编辑端到端:Enter 进入编辑 → 改值 → 保存 → config 变更 + 热生效命令排队 + toml 落盘
    #[test]
    fn settings_edit_flow_commit_and_hot_apply() {
        use crate::ui::settings::{PageAction, SettingsRuntime};
        use lex_core::config::Config;
        let tag = "lex-settings-edit-flow-test";
        let cwd = std::env::temp_dir().join(tag);
        let _ = std::fs::remove_dir_all(&cwd);
        std::fs::create_dir_all(&cwd).unwrap();
        let mut app = AppState::new();
        let view = crate::ui::settings::SettingsView::from(
            &Config::default(),
            SettingsRuntime {
                compress_attempted: false,
                token_estimate: 0,
                forbidden_count: 0,
                confirm_count: 0,
                auto_count: 0,
                log_level: "warn".into(),
                project_type: "Rust".into(),
                cwd: cwd.to_string_lossy().into_owned(),
            },
        );
        app.apply(UiEvent::OpenSettings(view), &no_todos());
        app.handle_key(key_event(KeyCode::Enter)); // 进详情
        let action = {
            let (_, page) = app.settings.as_mut().unwrap();
            page.handle_key(page.selected, KeyCode::Enter)
        };
        assert!(matches!(action, PageAction::EditField(0)), "详情页 Enter 应进入字段编辑");
        app.handle_key(key_event(KeyCode::Enter)); // 字段 0 = provider,载入 "anthropic"
        assert!(app.settings_edit, "应进入编辑态");
        assert_eq!(app.input, "anthropic");
        // 改成 openai:清空后重输
        for _ in 0..9 {
            app.handle_key(key_event(KeyCode::Backspace));
        }
        for c in "openai".chars() {
            app.handle_key(key_event(KeyCode::Char(c)));
        }
        app.handle_key(key_event(KeyCode::Enter)); // 保存
        assert!(!app.settings_edit, "保存后退出编辑态");
        let (view, _) = app.settings.as_ref().unwrap();
        assert_eq!(view.config.provider, "openai", "config 应更新");
        assert_eq!(view.config.openai.model, Config::default().openai.model, "切 provider 后展示同步");
        assert!(app.settings_msg.as_ref().unwrap().1, "保存应成功");
        let commit = app.take_settings_commit();
        assert!(commit.is_some(), "应排队热生效命令");
        // toml 落盘且可解析回 provider=openai
        let text = std::fs::read_to_string(cwd.join("lex-code.toml")).unwrap();
        assert!(text.contains("provider = \"openai\""), "写回内容: {text}");
        let _ = std::fs::remove_dir_all(&cwd);
    }

    /// 非法输入:保存失败不改 config、不排队热生效,错误进页面消息
    #[test]
    fn settings_edit_invalid_input_rejected() {
        use crate::ui::settings::SettingsRuntime;
        use lex_core::config::Config;
        let cwd = std::env::temp_dir().join("lex-settings-edit-invalid-test");
        let _ = std::fs::remove_dir_all(&cwd);
        std::fs::create_dir_all(&cwd).unwrap();
        let mut app = AppState::new();
        let view = crate::ui::settings::SettingsView::from(
            &Config::default(),
            SettingsRuntime {
                compress_attempted: false,
                token_estimate: 0,
                forbidden_count: 0,
                confirm_count: 0,
                auto_count: 0,
                log_level: "warn".into(),
                project_type: "Rust".into(),
                cwd: cwd.to_string_lossy().into_owned(),
            },
        );
        app.apply(UiEvent::OpenSettings(view), &no_todos());
        app.handle_key(key_event(KeyCode::Enter));
        app.handle_key(key_event(KeyCode::Enter));
        let before = app.settings.as_ref().unwrap().0.config.clone();
        for c in "nonsense".chars() {
            app.handle_key(key_event(KeyCode::Char(c)));
        }
        app.handle_key(key_event(KeyCode::Enter));
        assert_eq!(app.settings.as_ref().unwrap().0.config.provider, before.provider, "非法输入不改 config");
        assert!(app.take_settings_commit().is_none(), "失败不排队热生效");
        assert!(!app.settings_msg.as_ref().unwrap().1, "应标记为失败消息");
        let _ = std::fs::remove_dir_all(&cwd);
    }

    /// 上下文容量事件更新容量行状态
    #[test]
    fn context_usage_event_updates_state() {
        let mut app = AppState::new();
        assert!(app.ctx_usage.is_none());
        app.apply(UiEvent::ContextUsage { used: 3200, limit: 64000 }, &no_todos());
        assert_eq!(app.ctx_usage, Some((3200, 64000)));
    }

    /// 确认弹层为最上层时,PgUp/PgDn 滚弹层内容;y 仍正常裁决
    #[test]
    fn confirm_modal_keys_scroll_content() {
        let mut app = AppState::new();
        let key = |code: KeyCode| crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::empty());
        app.open_confirm(PendingAction {
            tool_name: "bash_exec".into(),
            summary: "执行命令".into(),
            detail: lex_core::security::PendingDetail::Other,
        });
        assert_eq!(app.confirm_scroll, 0, "弹层打开时从顶部看起");
        app.handle_key(key(KeyCode::PageDown));
        app.handle_key(key(KeyCode::PageDown));
        assert_eq!(app.confirm_scroll, 20, "PageDown 向后看");
        app.handle_key(key(KeyCode::PageUp));
        assert_eq!(app.confirm_scroll, 10, "PageUp 向前看");
        assert!(app.pending_confirm.is_some(), "滚动不改变待裁决状态");
        app.handle_key(key(KeyCode::Char('y')));
        assert!(app.pending_confirm.is_none(), "y 仍正常允许");
    }

    /// 设置页打开时接管按键:字符不进输入盒,Esc 关闭回到主界面
    #[test]
    fn settings_overlay_intercepts_keys_and_esc_closes() {
        let mut app = AppState::new();
        let view = crate::ui::settings::SettingsView::from(
            &lex_core::config::Config::default(),
            crate::ui::settings::SettingsRuntime {
                compress_attempted: false,
                token_estimate: 0,
                forbidden_count: 0,
                confirm_count: 0,
                auto_count: 0,
                log_level: "warn".into(),
                project_type: "Rust".into(),
                cwd: ".".into(),
            },
        );
        app.apply(UiEvent::OpenSettings(view), &no_todos());
        assert!(app.settings.is_some(), "OpenSettings 应打开设置页");
        let key = |code| crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::empty());
        app.handle_key(key(KeyCode::Char('a')));
        assert!(app.input.is_empty(), "设置页打开时字符不应进输入盒");
        app.handle_key(key(KeyCode::Esc));
        assert!(app.settings.is_none(), "Esc 应回到主界面");
        app.handle_key(key(KeyCode::Char('a')));
        assert_eq!(app.input, "a", "关闭后输入恢复正常");
    }

    /// PageUp/PageDown 调节回看偏移;新输入跳回底部(End 已让位给输入光标)
    #[test]
    fn scroll_keys_adjust_offset_and_submit_resets() {
        let mut app = AppState::new();
        let key = |code: KeyCode| crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::empty());
        app.handle_key(key(KeyCode::PageUp));
        app.handle_key(key(KeyCode::PageUp));
        assert_eq!(app.scroll, 20, "PageUp 应回看 10 行/次");
        app.handle_key(key(KeyCode::PageDown));
        app.handle_key(key(KeyCode::PageDown));
        assert_eq!(app.scroll, 0, "PageDown 应下翻 10 行/次");
        app.handle_key(key(KeyCode::PageUp));
        app.handle_key(key(KeyCode::Enter)); // 空输入不提交,但仍应走按键路径
        app.push_user_input("新问题");
        assert_eq!(app.scroll, 0, "新输入应跳回底部跟随");
    }

    /// 输入行编辑:光标移动、任意位置插入/删除、CJK 按字符处理
    #[test]
    fn input_editing_cursor_moves_and_edits() {
        let mut app = AppState::new();
        let key = |code: KeyCode| crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::empty());
        for c in "abc".chars() {
            app.handle_key(key_event(KeyCode::Char(c)));
        }
        assert_eq!(app.input, "abc");
        assert_eq!(app.cursor, 3, "追加后光标在行尾");
        app.handle_key(key(KeyCode::Left));
        app.handle_key(key(KeyCode::Char('X')));
        assert_eq!(app.input, "abXc", "光标处插入");
        assert_eq!(app.cursor, 3);
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.input, "abc", "退格删除光标前一字符");
        assert_eq!(app.cursor, 2);
        app.handle_key(key(KeyCode::Left));
        app.handle_key(key(KeyCode::Left));
        assert_eq!(app.cursor, 0, "Left 在行首不再左移");
        app.handle_key(key(KeyCode::Delete));
        assert_eq!(app.input, "bc", "Delete 删除光标处字符");
        app.handle_key(key(KeyCode::Home));
        app.handle_key(key(KeyCode::Char('中')));
        assert_eq!(app.input, "中bc");
        assert_eq!(app.cursor, 1);
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.input, "bc", "退格删除整个 CJK 字符");
        assert_eq!(app.cursor, 0);
        app.handle_key(key(KeyCode::End));
        assert_eq!(app.cursor, 2, "End 跳到行尾");
    }
}
