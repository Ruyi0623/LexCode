use crate::ui::theme;
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
