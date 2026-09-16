use crate::ui::theme;
use crossterm::tty::IsTty;
use unicode_width::UnicodeWidthChar;

// ---------- 纯逻辑(单测覆盖,不依赖终端) ----------

/// 单行编辑器状态:字符向量 + 光标(字符索引)+ 水平滚动视口
pub struct InputState {
    text: Vec<char>,
    pub cursor: usize,
    scroll: usize,
    width: usize, // 视口显示宽度(列)
}

impl InputState {
    pub fn new(width: usize) -> Self {
        InputState { text: vec![], cursor: 0, scroll: 0, width: width.max(4) }
    }
    pub fn insert(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += 1;
    }
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.text.remove(self.cursor);
        }
    }
    pub fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }
    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }
    pub fn right(&mut self) {
        if self.cursor < self.text.len() {
            self.cursor += 1;
        }
    }
    pub fn home(&mut self) {
        self.cursor = 0;
    }
    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.scroll = 0;
    }
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    /// 视口内容与光标列(按显示宽度;CJK 宽字符按 2 列计)。
    /// 保证:光标总在视口内;滚动窗口随光标单向跟随。
    pub fn viewport(&mut self) -> (String, usize) {
        let mut scroll = self.scroll.min(self.text.len()).min(self.cursor);
        loop {
            let fit = fit_width(&self.text[scroll..], self.width);
            if self.cursor <= scroll + fit || scroll >= self.text.len() {
                break;
            }
            scroll += 1;
        }
        self.scroll = scroll;
        let fit = fit_width(&self.text[scroll..], self.width);
        let visible: String = self.text[scroll..scroll + fit].iter().collect();
        let col: usize =
            self.text[scroll..self.cursor].iter().map(|c| c.width().unwrap_or(1)).sum();
        (visible, col)
    }
}

/// 显示宽度不超过 width 的最长前缀长度(字符数)
fn fit_width(s: &[char], width: usize) -> usize {
    let mut w = 0;
    for (i, c) in s.iter().enumerate() {
        let cw = c.width().unwrap_or(1);
        if w + cw > width {
            return i;
        }
        w += cw;
    }
    s.len()
}

/// 会话输入历史:↑↓ 翻阅;未提交的草稿在翻到头时恢复
#[derive(Default)]
pub struct InputHistory {
    items: Vec<String>,
    draft: Option<String>,
    idx: Option<usize>,
}

impl InputHistory {
    pub fn push(&mut self, s: String) {
        if !s.trim().is_empty() {
            self.items.push(s);
        }
        self.idx = None;
        self.draft = None;
    }
    /// ↑:更早一条;已到最早返回 None
    pub fn prev(&mut self, current: &str) -> Option<String> {
        if self.items.is_empty() {
            return None;
        }
        match self.idx {
            None => {
                self.draft = Some(current.to_string());
                self.idx = Some(self.items.len() - 1);
            }
            Some(0) => return None,
            Some(i) => self.idx = Some(i - 1),
        }
        Some(self.items[self.idx.unwrap_or(0)].clone())
    }
    /// ↓:更新一条;到头恢复草稿(可能为 None)
    pub fn next(&mut self) -> Option<String> {
        match self.idx {
            None => None,
            Some(i) if i + 1 >= self.items.len() => {
                self.idx = None;
                self.draft.clone()
            }
            Some(i) => {
                self.idx = Some(i + 1);
                Some(self.items[self.idx.unwrap_or(0)].clone())
            }
        }
    }
}

// ---------- 终端集成 ----------

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal;
use lex_core::error::{LexError, Result};

/// 输入结果:Submitted 提交文本;Exit 退出会话(Ctrl+D / 空输入时再次 Ctrl+C)
pub enum InputOutcome {
    Submitted(String),
    Exit,
}

/// raw mode 守卫:Drop 恢复,panic/错误路径也不残留
pub(crate) struct RawGuard;
impl RawGuard {
    pub(crate) fn new() -> std::io::Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(RawGuard)
    }
}
impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

/// 单例事件读取线程:阻塞 read() 经无界 channel 供给异步侧;
/// 任务执行期间产生的按键会在下次输入会话开始时被清空。
pub(crate) fn event_bus() -> &'static (
    tokio::sync::mpsc::UnboundedSender<Event>,
    tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Event>>,
) {
    static BUS: std::sync::OnceLock<(
        tokio::sync::mpsc::UnboundedSender<Event>,
        tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Event>>,
    )> = std::sync::OnceLock::new();
    BUS.get_or_init(|| {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let tx_thread = tx.clone();
        std::thread::spawn(move || loop {
            match crossterm::event::read() {
                Ok(ev) => {
                    if tx_thread.send(ev).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        });
        (tx, tokio::sync::Mutex::new(rx))
    })
}

async fn drain_pending(rx: &tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Event>>) {
    let mut rx = rx.lock().await;
    while rx.try_recv().is_ok() {}
}

/// 输入入口:stdin/stdout 均为 TTY 时用 crossterm 输入盒;否则降级共享 CliInput 行式读取
pub async fn read_input(
    history: &mut InputHistory,
    fallback: &crate::confirm::CliInput,
) -> Result<InputOutcome> {
    let (_, rx) = event_bus();
    drain_pending(rx).await;
    let is_tty = std::io::stdin().is_tty() && std::io::stdout().is_tty();
    if !is_tty {
        let line = fallback.read_line("\n› ").await?;
        return Ok(match line.trim() {
            "" => InputOutcome::Exit,
            t => InputOutcome::Submitted(t.to_string()),
        });
    }
    boxed_input(history, rx).await
}

async fn boxed_input(
    history: &mut InputHistory,
    rx: &tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Event>>,
) -> Result<InputOutcome> {
    let _raw = RawGuard::new().map_err(LexError::Io)?;
    let (term_w, _) = terminal::size().unwrap_or((100, 30));
    let width = (term_w as usize).saturating_sub(2).min(100);

    // 画盒:顶边 + 初始内容行 + 底边;光标回到内容行等待输入。
    // raw mode 下 \n 只下移不回车,盒子绘制阶段的换行必须显式 \r\n。
    anstream::print!("{}╭{}╮{}", theme::ACCENT, "─".repeat(width), theme::RESET);
    anstream::print!("\r\n");
    let mut state = InputState::new(width);
    redraw(&mut state, false);
    anstream::print!("\r\n{}╰{}╯{}", theme::ACCENT, "─".repeat(width), theme::RESET);
    // 回到内容行,光标停在输入起点("│ › " 前缀占 4 列,MoveToColumn 为 0 基)
    crossterm::execute!(std::io::stdout(), crossterm::cursor::MoveUp(1), crossterm::cursor::MoveToColumn(4))
        .map_err(LexError::Io)?;

    let mut ctrl_c_on_empty = false;
    let mut rx = rx.lock().await;
    loop {
        let ev = rx.recv().await;
        let Some(ev) = ev else { return Ok(InputOutcome::Exit) }; // 读线程终止
        let Event::Key(key) = ev else { continue };
        if key.kind != KeyEventKind::Press {
            continue; // Windows 会发 Release 事件
        }

        let mut exit = false;
        match (key.code, key.modifiers) {
            (KeyCode::Enter, _) => {
                let text = state.text();
                collapse_box(&text);
                return Ok(InputOutcome::Submitted(text));
            }
            (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                if state.is_empty() {
                    if ctrl_c_on_empty {
                        exit = true;
                    } else {
                        ctrl_c_on_empty = true;
                    }
                } else {
                    state.clear();
                    ctrl_c_on_empty = false;
                }
            }
            (KeyCode::Char('d'), m) if m.contains(KeyModifiers::CONTROL) => exit = true,
            (KeyCode::Char('u'), m) if m.contains(KeyModifiers::CONTROL) => {
                state.clear();
                ctrl_c_on_empty = false;
            }
            (KeyCode::Backspace, _) => state.backspace(),
            (KeyCode::Delete, _) => state.delete(),
            (KeyCode::Left, _) => state.left(),
            (KeyCode::Right, _) => state.right(),
            (KeyCode::Home, _) => state.home(),
            (KeyCode::End, _) => state.end(),
            (KeyCode::Up, _) => {
                let cur = state.text();
                if let Some(s) = history.prev(&cur) {
                    set_text(&mut state, &s);
                }
            }
            (KeyCode::Down, _) => {
                if let Some(s) = history.next() {
                    set_text(&mut state, &s);
                }
            }
            (KeyCode::Char(c), m)
                if !m.contains(KeyModifiers::CONTROL) && !m.contains(KeyModifiers::ALT) =>
            {
                state.insert(c);
                ctrl_c_on_empty = false;
            }
            _ => {}
        }
        if exit {
            erase_box();
            return Ok(InputOutcome::Exit);
        }
        redraw(&mut state, ctrl_c_on_empty);
    }
}

fn set_text(state: &mut InputState, s: &str) {
    state.clear();
    for c in s.chars() {
        state.insert(c);
    }
}

/// 重绘内容行:行首清行 → 蓝竖线 + › 提示符 + 视口文本 → 光标定位到 4+col+1 列
fn redraw(state: &mut InputState, hint: bool) {
    let (visible, col) = state.viewport();
    let hint = if hint { theme::dim("  (再按一次 Ctrl+C 退出)") } else { String::new() };
    anstream::print!(
        "{}{}│{} › {}{}{}{}",
        theme::CLEAR_LINE,
        theme::ACCENT,
        theme::RESET,
        theme::ACCENT,
        theme::RESET,
        visible,
        hint,
    );
    // "│ › " 前缀占 4 列(ANSI G 从 1 计)
    anstream::print!("{}", theme::goto_col(4 + col + 1));
    use std::io::Write;
    std::io::stdout().flush().ok();
}

/// 提交收尾:拆掉上下边框,内容行改写为无框的 `› 文本` 回执(仿 Claude Code),
/// 光标落在回执下方的空行,后续输出从那里继续。
fn collapse_box(text: &str) {
    // 1) 内容行去框,保留回执
    anstream::print!("{}{}› {}{}", theme::CLEAR_LINE, theme::DIM, text, theme::RESET);
    // 2) 下移清除底边框;这条空行就是后续输出的起点
    anstream::print!("\r\n{}", theme::CLEAR_LINE);
    // 3) 上移清除顶边框
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::cursor::MoveUp(2),
        crossterm::cursor::MoveToColumn(0)
    );
    anstream::print!("{}", theme::CLEAR_LINE);
    // 4) 光标回到输出行(原底边行)
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::cursor::MoveDown(2),
        crossterm::cursor::MoveToColumn(0)
    );
    use std::io::Write;
    std::io::stdout().flush().ok();
}

/// 退出收尾:盒子三行(顶边/内容/底边)全部清除,不留残框
fn erase_box() {
    anstream::print!("{}", theme::CLEAR_LINE); // 内容行
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::cursor::MoveUp(1),
        crossterm::cursor::MoveToColumn(0)
    );
    anstream::print!("{}", theme::CLEAR_LINE); // 顶边
    anstream::print!("\r\n{}", theme::CLEAR_LINE); // 回内容行
    anstream::print!("\r\n{}", theme::CLEAR_LINE); // 底边
    use std::io::Write;
    std::io::stdout().flush().ok();
}

#[cfg(test)]
mod editor_tests {
    use super::*;

    #[test]
    fn insert_backspace_delete() {
        let mut s = InputState::new(10);
        for c in "abc".chars() {
            s.insert(c);
        }
        s.left();
        s.left(); // 光标在 'b' 前
        s.insert('X'); // "aXbc"
        assert_eq!(s.text(), "aXbc");
        s.backspace(); // 删 'X'
        assert_eq!(s.text(), "abc");
        s.delete(); // 删光标处 'b'
        assert_eq!(s.text(), "ac");
    }

    #[test]
    fn cursor_movement_bounds() {
        let mut s = InputState::new(10);
        for c in "ab".chars() {
            s.insert(c);
        }
        s.left();
        s.left();
        s.left(); // 不能越过 0
        assert_eq!(s.cursor, 0);
        s.right();
        s.right();
        s.right(); // 不能越过 len
        assert_eq!(s.cursor, 2);
        s.home();
        assert_eq!(s.cursor, 0);
        s.end();
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn viewport_follows_cursor_right() {
        let mut s = InputState::new(5);
        for c in "abcdefghij".chars() {
            s.insert(c);
        }
        let (visible, col) = s.viewport();
        assert_eq!(visible, "fghij");
        assert_eq!(col, 5);
    }

    #[test]
    fn viewport_follows_cursor_left() {
        let mut s = InputState::new(5);
        for c in "abcdefghij".chars() {
            s.insert(c);
        }
        let _ = s.viewport(); // 先滚到末尾
        s.home();
        let (visible, col) = s.viewport();
        assert_eq!(visible, "abcde");
        assert_eq!(col, 0);
    }

    #[test]
    fn viewport_counts_cjk_as_double() {
        let mut s = InputState::new(4);
        for c in "中文ab".chars() {
            s.insert(c);
        } // 显示宽 2+2+1+1=6 > 4
        let (visible, col) = s.viewport();
        // 光标必须可见:窗口右移到 "文ab"(宽 2+1+1=4 恰好占满)
        assert_eq!(visible, "文ab");
        assert_eq!(col, 4);
    }

    #[test]
    fn history_prev_next_roundtrip() {
        let mut h = InputHistory::default();
        h.push("第一".into());
        h.push("第二".into());
        assert_eq!(h.prev("草稿"), Some("第二".into()));
        assert_eq!(h.prev("第二"), Some("第一".into()));
        assert_eq!(h.prev("第一"), None); // 已到最早
        assert_eq!(h.next(), Some("第二".into()));
        assert_eq!(h.next(), Some("草稿".into())); // 回到未提交草稿
        assert_eq!(h.next(), None);
    }
}
