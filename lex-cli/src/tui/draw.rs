use crate::tui::diff::{line_diff, DiffLine};
use crate::tui::state::AppState;
use crate::ui::theme;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

pub fn draw(app: &mut AppState, f: &mut Frame) {
    let chunks = Layout::vertical([Constraint::Min(3), Constraint::Length(3), Constraint::Length(1)]).split(f.area());
    let cols = Layout::horizontal([Constraint::Percentage(75), Constraint::Percentage(25)]).split(chunks[0]);

    draw_transcript(f, cols[0], app);
    draw_todos(f, cols[1], app);
    draw_input(f, chunks[1], app);
    draw_status(f, chunks[2], app);
    if app.pending_confirm.is_some() {
        draw_confirm(f, app);
    }
}

fn draw_transcript(f: &mut Frame, area: Rect, app: &mut AppState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::C_BORDER))
        .title(" 对话 ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let visible = inner.height as usize;
    let start = app.transcript.len().saturating_sub(visible.saturating_sub(1));
    let lines: Vec<Line> = app.transcript[start..].to_vec();
    // 必须显式折行:ratatui 默认不折行,超宽正文会被直接截断(长回复尾部看不见)
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_todos(f: &mut Frame, area: Rect, app: &mut AppState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::C_BORDER))
        .title(" 待办 ");
    let inner = block.inner(area);
    let items: Vec<ListItem> = app
        .todos
        .iter()
        .map(|t| {
            let (mark, style) = match t.status {
                lex_core::tools::TodoStatus::Pending => ("☐", Style::default().fg(theme::C_DIM)),
                lex_core::tools::TodoStatus::InProgress => ("◐", Style::default().fg(theme::C_ACCENT)),
                lex_core::tools::TodoStatus::Completed => ("☑", Style::default().fg(theme::C_SUCCESS)),
            };
            ListItem::new(Line::from(Span::styled(format!("{mark} {}", t.content), style)))
        })
        .collect();
    f.render_widget(block, area);
    f.render_widget(List::new(items), inner);
}

fn draw_input(f: &mut Frame, area: Rect, app: &mut AppState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if app.busy { theme::C_BORDER } else { theme::C_ACCENT }))
        .title(" 输入(Enter 提交,Ctrl+C 中断/退出) ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(format!("› {}", app.input)), inner);
}

fn draw_status(f: &mut Frame, area: Rect, app: &mut AppState) {
    let right = app.usage.clone().unwrap_or_default();
    let left = app.status.clone();
    f.render_widget(Paragraph::new(Line::from(vec![
        Span::styled(left, Style::default().fg(theme::C_DIM)),
        Span::raw("  "),
        Span::styled(right, Style::default().fg(theme::C_DIM)),
    ])), area);
}

/// diff 行 → 带色 Span 行(增绿删红,公开供测试断言)
pub fn styled_diff_lines(diff: &[DiffLine]) -> Vec<Line<'static>> {
    diff.iter()
        .map(|l| match l {
            DiffLine::Add(s) => Line::from(Span::styled(format!("+ {s}"), Style::default().fg(theme::C_DIFF_ADD))),
            DiffLine::Del(s) => Line::from(Span::styled(format!("- {s}"), Style::default().fg(theme::C_DIFF_DEL))),
            DiffLine::Ctx(s) => Line::from(Span::styled(format!("  {s}"), Style::default().fg(theme::C_DIM))),
        })
        .collect()
}

fn draw_confirm(f: &mut Frame, app: &mut AppState) {
    let Some(modal) = app.pending_confirm.as_ref() else { return };
    let area = centered_rect(80, 60, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::C_WARN))
        .title(format!(" 确认操作 [{}] ", modal.action.tool_name));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = vec![Line::from(Span::styled(modal.action.summary.clone(), Style::default().fg(theme::C_WARN)))];
    lines.push(Line::from(""));
    match &modal.action.detail {
        lex_core::security::PendingDetail::Bash { command } => {
            lines.push(Line::from(Span::styled(format!("$ {command}"), Style::default().fg(theme::C_ACCENT))));
        }
        lex_core::security::PendingDetail::FileEdit { path, old_string, new_string } => {
            lines.push(Line::from(Span::styled(format!("文件: {path}"), Style::default().fg(theme::C_ACCENT))));
            lines.extend(styled_diff_lines(&line_diff(old_string, new_string)));
        }
        lex_core::security::PendingDetail::Other => {}
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("[y] 允许   [n/Esc] 拒绝", Style::default().fg(theme::C_DIM))));
    f.render_widget(Paragraph::new(lines), inner);
}

fn centered_rect(percent_x: u16, percent_y: u16, outer: Rect) -> Rect {
    let v = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(outer);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(v[1])[1]
}

#[cfg(test)]
mod tests {
    use super::draw;
    use crate::tui::diff::DiffLine;
    use crate::tui::event::UiEvent;
    use crate::tui::state::{AppState, KeyOutcome};
    use crossterm::event::KeyCode;
    use lex_core::security::{PendingAction, PendingDetail};
    use ratatui::{backend::TestBackend, Terminal};
    use std::sync::{Arc, Mutex};

    fn render(app: &mut AppState, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut term = Terminal::new(backend).unwrap_or_else(|e| panic!("{e}"));
        term.draw(|f| draw(app, f)).unwrap_or_else(|e| panic!("{e}"));
        let buf = term.backend().buffer();
        let mut s = String::new();
        for y in 0..buf.area.height {
            // 宽字符(CJK)会占用两格,后一格是占位格;逐格拼接会拼出
            // "你 好"这种带假空格的串,故按显示宽度跳过占位格。
            let mut skip = 0usize;
            for x in 0..buf.area.width {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let sym = buf[(x, y)].symbol();
                s.push_str(sym);
                let width = unicode_width::UnicodeWidthStr::width(sym);
                if width > 1 {
                    skip = width - 1;
                }
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn layout_regions_present() {
        let todos: Arc<Mutex<Vec<lex_core::tools::Todo>>> = Arc::new(Mutex::new(vec![]));
        let mut app = AppState::new();
        app.apply(UiEvent::TextDelta("你好,世界".into()), &todos);
        let screen = render(&mut app, 80, 24);
        assert!(screen.contains("你好,世界"), "主输出区应渲染文本,实际:\n{screen}");
        assert!(screen.contains("待办"), "右侧应有待办面板标题,实际:\n{screen}");
        assert!(screen.contains("›"), "底部应有输入盒提示符,实际:\n{screen}");
    }

    #[test]
    fn todo_panel_reflects_shared_state() {
        let todos = Arc::new(Mutex::new(vec![lex_core::tools::Todo {
            content: "写测试".into(),
            status: lex_core::tools::TodoStatus::InProgress,
        }]));
        let mut app = AppState::new();
        app.apply(
            UiEvent::ToolResult { tool_name: "todo_write".into(), first_line: "待办已更新".into(), is_error: false },
            &todos,
        );
        let screen = render(&mut app, 80, 24);
        assert!(screen.contains("写测试"), "待办面板应反映共享状态,实际:\n{screen}");
    }

    #[test]
    fn confirm_modal_renders_diff_colored() {
        let mut app = AppState::new();
        app.open_confirm(PendingAction {
            tool_name: "file_edit".into(),
            summary: "编辑文件: a.rs".into(),
            detail: PendingDetail::FileEdit {
                path: "a.rs".into(),
                old_string: "old".into(),
                new_string: "new".into(),
            },
        });
        let screen = render(&mut app, 80, 24);
        assert!(screen.contains("确认操作"), "应有弹层标题,实际:\n{screen}");
        assert!(screen.contains("- old"), "弹层应渲染删除行,实际:\n{screen}");
        assert!(screen.contains("+ new"), "弹层应渲染新增行,实际:\n{screen}");
        assert!(screen.contains("[y] 允许"), "应展示快捷键,实际:\n{screen}");
    }

    #[test]
    fn keys_route_by_mode() {
        let mut app = AppState::new();
        // 输入模式:字符进输入盒,回车提交
        assert_eq!(app.handle_key(key_event(KeyCode::Char('h'))), KeyOutcome::None);
        assert_eq!(app.handle_key(key_event(KeyCode::Enter)), KeyOutcome::Submit("h".into()));
        // 确认模式:y/n 直接裁决
        app.open_confirm(PendingAction { tool_name: "bash_exec".into(), summary: "s".into(), detail: PendingDetail::Other });
        assert_eq!(app.handle_key(key_event(KeyCode::Char('y'))), KeyOutcome::Confirm(true));
        assert!(app.pending_confirm.is_none());
        app.open_confirm(PendingAction { tool_name: "bash_exec".into(), summary: "s".into(), detail: PendingDetail::Other });
        assert_eq!(app.handle_key(key_event(KeyCode::Esc)), KeyOutcome::Confirm(false));
    }

    // 测试辅助:构造 Press 键事件
    fn key_event(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::empty())
    }

    #[test]
    fn diff_lines_render_with_colors() {
        // 直接验证弹层 diff 行样式:增行绿、删行红(样式挂在 Span 上)
        let lines = vec![DiffLine::Add("new".into()), DiffLine::Del("old".into())];
        let styled = super::styled_diff_lines(&lines);
        assert_eq!(styled[0].spans[0].style.fg, Some(crate::ui::theme::C_DIFF_ADD));
        assert_eq!(styled[1].spans[0].style.fg, Some(crate::ui::theme::C_DIFF_DEL));
    }

    /// 超宽正文必须折行:不折行时 ratatui 会直接截断,长回复的尾部根本看不到
    #[test]
    fn long_transcript_line_wraps_instead_of_truncating() {
        let todos: Arc<Mutex<Vec<lex_core::tools::Todo>>> = Arc::new(Mutex::new(vec![]));
        let mut app = AppState::new();
        // 124 字符 > 对话面板内宽(80 列的 75% 再减两侧边框),必然需要多行
        app.apply(UiEvent::TextDelta(format!("{}结尾标记", "很长的一段话".repeat(20))), &todos);
        let screen = render(&mut app, 80, 24);
        assert!(screen.contains("结尾标记"), "超宽正文应折行而非截断,实际:\n{screen}");
    }
}
