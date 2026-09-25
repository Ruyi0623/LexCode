//! TUI 专用的行级 markdown 渲染:markdown 文本 → ratatui `Span` 序列。
//!
//! 规则与 `ui/markdown.rs`(CLI 纯文本路径)对齐:`#` 标题整行加粗、`-`/`*` 列表
//! 渲染为强调蓝 `•`、``` 围栏内容变暗且带底色(标记行本身不显示)、行内
//! `**粗体**` / `*斜体*` / `` `代码` ``(亮青)。之所以不复用 CLI 渲染器:
//! 它输出 ANSI 转义串,会以字面形式混进 ratatui 单元格破坏画面。
//!
//! 表格需要跨行对齐(列宽取整块最宽单元格),单行无法独立渲染:
//! `MarkdownState` 由 `AppState` 持有,`|` 开头的行缓冲进表格块,
//! 块结束(首个非表格行 / 行落定收尾)时按显示宽度对齐一次性输出。

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

use crate::ui::theme;

/// 跨行渲染状态:围栏开合 + 表格行缓冲。由 `AppState` 持有并跨行传递。
pub struct MarkdownState {
    pub fence: bool,
    /// 缓冲中的表格行;`None` = 分隔行(`|---|---|`)
    table: Vec<Option<Vec<String>>>,
}

impl MarkdownState {
    pub fn new() -> Self {
        MarkdownState { fence: false, table: Vec::new() }
    }

    /// 新一轮开始时复位:围栏不跨轮,未落定的表格块一并丢弃
    /// (正常路径已在轮末 `take_table` 落定,这里兜底防状态泄漏)
    pub fn reset(&mut self) {
        self.fence = false;
        self.table.clear();
    }

    /// 渲染一行,返回 0..n 行 Span:
    /// - 围栏标记行 / 表格缓冲行:0 行(调用方留空行占位,表格整块结束时对齐输出)
    /// - 普通行:1 行
    /// - 表格结束后的首个非表格行:先输出对齐的表格块,再输出该行
    pub fn render_line(&mut self, line: &str) -> Vec<Vec<Span<'static>>> {
        let t = line.trim_start();
        if t.starts_with("```") {
            self.fence = !self.fence;
            return Vec::new();
        }
        if self.fence {
            return vec![vec![Span::styled(
                line.to_string(),
                Style::default().fg(theme::C_DIM).bg(theme::C_CODE_BG),
            )]];
        }
        if is_table_row(t) {
            self.table.push(parse_table_row(t));
            return Vec::new();
        }
        let mut out = self.take_table();
        out.push(plain_line_spans(t));
        out
    }

    /// 取走缓冲的表格块(对齐后的行);无缓冲返回空。
    /// 表格作为本轮最后内容时由 `AppState` 在行落定后调用。
    pub fn take_table(&mut self) -> Vec<Vec<Span<'static>>> {
        if self.table.is_empty() {
            return Vec::new();
        }
        render_table(std::mem::take(&mut self.table))
    }

    /// 是否有表格行在缓冲中(调用方据此移除占位行)
    pub fn table_is_buffering(&self) -> bool {
        !self.table.is_empty()
    }
}

impl Default for MarkdownState {
    fn default() -> Self {
        Self::new()
    }
}

/// 表格行判定:`|` 开头且 `|` 结尾(截图实测模型输出均为此形状)
fn is_table_row(t: &str) -> bool {
    t.starts_with('|') && t.ends_with('|') && t.chars().count() > 1
}

/// 拆分单元格;分隔行(`|---|:--:|`)返回 `None`
fn parse_table_row(t: &str) -> Option<Vec<String>> {
    let cells: Vec<String> = t.trim_matches('|').split('|').map(|c| c.trim().to_string()).collect();
    let is_sep = !cells.is_empty()
        && cells.iter().all(|c| !c.is_empty() && c.contains('-') && c.chars().all(|ch| ch == '-' || ch == ':'));
    if is_sep {
        None
    } else {
        Some(cells)
    }
}

/// 整块表格 → 对齐的多行:列宽 = 各列最宽单元格的显示宽度(CJK 按 2 计),
/// 分隔行渲染为 `├──┼──┤`,竖线用强调蓝,表头加粗。
fn render_table(rows: Vec<Option<Vec<String>>>) -> Vec<Vec<Span<'static>>> {
    let n_cols = rows.iter().flatten().map(|r| r.len()).max().unwrap_or(0);
    if n_cols == 0 {
        return Vec::new();
    }
    let mut widths = vec![0usize; n_cols];
    for row in rows.iter().flatten() {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(UnicodeWidthStr::width(cell.as_str()));
        }
    }
    // 表头 = 紧邻分隔行之前的第一个非分隔行(典型 markdown 表格的第 0 行)
    let header_idx = if rows.len() > 1 && rows[1].is_none() { Some(0usize) } else { None };

    let mut out = Vec::new();
    for (idx, row) in rows.into_iter().enumerate() {
        match row {
            None => {
                let seg: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
                out.push(vec![Span::styled(
                    format!("├{}┤", seg.join("┼")),
                    Style::default().fg(theme::C_BORDER),
                )]);
            }
            Some(cells) => {
                let is_header = header_idx == Some(idx);
                let mut spans = vec![Span::styled("│".to_string(), Style::default().fg(theme::C_ACCENT))];
                for i in 0..n_cols {
                    let cell = cells.get(i).map(String::as_str).unwrap_or("");
                    spans.push(Span::raw(" ".to_string()));
                    let mut cell_spans = inline_spans(cell);
                    let mut dw = 0usize;
                    for s in cell_spans.iter_mut() {
                        if is_header {
                            s.style = s.style.add_modifier(Modifier::BOLD);
                        }
                        dw += UnicodeWidthStr::width(s.content.as_ref());
                    }
                    spans.extend(cell_spans);
                    // 右侧补齐到列宽(+1 为固定尾随空格)
                    spans.push(Span::raw(" ".repeat(widths[i] - dw + 1)));
                    spans.push(Span::styled("│".to_string(), Style::default().fg(theme::C_ACCENT)));
                }
                out.push(spans);
            }
        }
    }
    out
}

/// 单行(非围栏):标题加粗、列表蓝点、其余走行内样式
fn plain_line_spans(t: &str) -> Vec<Span<'static>> {
    if t.starts_with('#') {
        let stripped = t.trim_start_matches('#').trim_start();
        if !stripped.is_empty() {
            // 标题整体加粗(与 CLI 路径一致:标题内部行内样式一并生效)
            return vec![Span::styled(
                stripped.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            )];
        }
    }
    if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
        let mut spans = vec![
            Span::styled("•".to_string(), Style::default().fg(theme::C_ACCENT)),
            Span::raw(" ".to_string()),
        ];
        spans.extend(inline_spans(rest));
        return spans;
    }
    inline_spans(t)
}

/// 行内样式解析:`**粗体**`、`*斜体*`、`` `代码` ``(亮青 + 底色)。
/// 语义与 `ui/markdown.rs::replace_inline` 相同:斜体只在 `*` 后跟非空白时开启、
/// 已开启斜体时任意 `*` 视为关闭、未闭合样式随行结束自然复位(Span 有界)。
pub fn inline_spans(s: &str) -> Vec<Span<'static>> {
    let chars: Vec<char> = s.chars().collect();
    let mut spans = Vec::new();
    let mut buf = String::new();
    let mut in_code = false;
    let mut in_bold = false;
    let mut in_italic = false;

    // 索引循环:`**` 需要一次消费两个字符,for 枚举会把第二个 * 重复当字面量
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_code {
            if c == '`' {
                flush(&mut buf, in_code, in_bold, in_italic, &mut spans);
                in_code = false;
            } else {
                buf.push(c);
            }
            i += 1;
            continue;
        }
        match c {
            '`' => {
                flush(&mut buf, in_code, in_bold, in_italic, &mut spans);
                in_code = true;
                i += 1;
            }
            '*' if in_italic => {
                flush(&mut buf, in_code, in_bold, in_italic, &mut spans);
                in_italic = false;
                i += 1;
            }
            '*' if i + 1 < chars.len() && chars[i + 1] == '*' => {
                flush(&mut buf, in_code, in_bold, in_italic, &mut spans);
                in_bold = !in_bold;
                i += 2;
            }
            '*' if i + 1 < chars.len() && chars[i + 1] != ' ' => {
                flush(&mut buf, in_code, in_bold, in_italic, &mut spans);
                in_italic = true;
                i += 1;
            }
            _ => {
                buf.push(c);
                i += 1;
            }
        }
    }
    flush(&mut buf, in_code, in_bold, in_italic, &mut spans);
    spans
}

/// 把累计缓冲按当前样式落成一个 Span;代码样式独占且带底色
/// (与 CLI 路径的 CODE/RESET 序列一致,底色与终端背景区分开)
fn flush(buf: &mut String, in_code: bool, in_bold: bool, in_italic: bool, spans: &mut Vec<Span<'static>>) {
    if buf.is_empty() {
        return;
    }
    let content = std::mem::take(buf);
    let style = if in_code {
        Style::default().fg(theme::C_CODE).bg(theme::C_CODE_BG)
    } else {
        let mut m = Modifier::empty();
        if in_bold {
            m |= Modifier::BOLD;
        }
        if in_italic {
            m |= Modifier::ITALIC;
        }
        Style::default().add_modifier(m)
    };
    spans.push(Span::styled(content, style));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn heading_line_is_bold_and_strips_hashes() {
        let mut md = MarkdownState::new();
        let out = md.render_line("### 提交计划");
        assert_eq!(out.len(), 1);
        assert_eq!(text_of(&out[0]), "提交计划");
        assert!(out[0][0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn bullet_uses_accent_dot_and_inline_styles() {
        let mut md = MarkdownState::new();
        let out = md.render_line("- 复核 **214** 个测试");
        assert_eq!(text_of(&out[0]), "• 复核 214 个测试");
        assert_eq!(out[0][0].style.fg, Some(theme::C_ACCENT), "列表点应为强调蓝");
        let bold = out[0].iter().find(|s| s.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(bold.map(|s| s.content.as_ref()), Some("214"));
    }

    #[test]
    fn inline_code_is_cyan_with_distinct_background() {
        let mut md = MarkdownState::new();
        let out = md.render_line("跑 `cargo test` 即可");
        let code = out[0].iter().find(|s| s.style.fg == Some(theme::C_CODE));
        assert_eq!(code.map(|s| s.content.as_ref()), Some("cargo test"));
        assert_eq!(code.map(|s| s.style.bg), Some(Some(theme::C_CODE_BG)), "行内代码应有底色");
    }

    #[test]
    fn fence_toggles_dims_content_with_background() {
        let mut md = MarkdownState::new();
        assert!(md.render_line("```rust").is_empty(), "围栏标记行不显示");
        assert!(md.fence, "开栏后状态应翻转");
        let body = md.render_line("fn a() {}");
        assert_eq!(body[0][0].style.fg, Some(theme::C_DIM), "围栏内容应变暗");
        assert_eq!(body[0][0].style.bg, Some(theme::C_CODE_BG), "围栏内容应带底色与终端背景区分");
        assert!(md.render_line("```").is_empty());
        assert!(!md.fence, "闭栏后状态应翻转");
    }

    #[test]
    fn multiplication_star_is_not_italic() {
        let mut md = MarkdownState::new();
        let out = md.render_line("a * b = c");
        assert_eq!(text_of(&out[0]), "a * b = c");
        assert!(out[0].iter().all(|s| !s.style.add_modifier.contains(Modifier::ITALIC)));
    }

    #[test]
    fn italic_closes_on_any_star() {
        let mut md = MarkdownState::new();
        let out = md.render_line("*斜体* 后文");
        assert_eq!(text_of(&out[0]), "斜体 后文");
        let italic: Vec<_> = out[0].iter().filter(|s| s.style.add_modifier.contains(Modifier::ITALIC)).collect();
        assert_eq!(italic.len(), 1);
        assert_eq!(italic[0].content.as_ref(), "斜体");
    }

    /// 表格整块缓冲:行进块时不出行,块结束(非表格行)时按显示宽度对齐一次输出
    #[test]
    fn table_buffered_then_flushed_aligned_on_block_end() {
        let mut md = MarkdownState::new();
        assert!(md.render_line("| 工具 | 状态 |").is_empty(), "表格行应缓冲");
        assert!(md.render_line("|------|------|").is_empty(), "分隔行应缓冲");
        assert!(md.render_line("| file_read | 通过 |").is_empty(), "数据行应缓冲");
        // 表格后的普通行触发整块输出:3 行表格 + 1 行正文
        let out = md.render_line("表格结束");
        assert_eq!(out.len(), 4, "应输出对齐表格 3 行 + 正文 1 行,实际 {}", out.len());
        // CJK 显示宽度对齐:表头/分隔/数据三行等宽,竖线落在同一列
        let w = |s: &str| unicode_width::UnicodeWidthStr::width(s);
        let header = text_of(&out[0]);
        let data = text_of(&out[2]);
        assert!(header.starts_with("│ 工具"), "表头应左对齐补齐列宽: {header}");
        assert!(data.starts_with("│ file_read"), "数据行应左对齐补齐列宽: {data}");
        assert_eq!(w(&header), w(&text_of(&out[1])), "表头与分隔行应等宽对齐: {header}");
        assert_eq!(w(&header), w(&data), "表头与数据行应等宽对齐: {header} vs {data}");
        assert_eq!(out[3][0].content, "表格结束");
        assert!(out[0].iter().any(|s| s.style.add_modifier.contains(Modifier::BOLD)), "表头应加粗");
    }

    /// 表格作为本轮最后内容:take_table 在行落定后补齐输出
    #[test]
    fn table_flushed_via_take_table_at_block_end() {
        let mut md = MarkdownState::new();
        md.render_line("| a | b |");
        md.render_line("|---|---|");
        md.render_line("| 1 | 2 |");
        let out = md.take_table();
        assert_eq!(out.len(), 3, "缓冲的表格应整块输出");
        assert!(text_of(&out[2]).starts_with("│ 1 "), "数据行应对齐: {}", text_of(&out[2]));
    }
}
