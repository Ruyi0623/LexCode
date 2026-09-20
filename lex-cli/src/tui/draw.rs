use crate::tui::diff::{line_diff, DiffLine};
use crate::tui::state::AppState;
use crate::ui::theme;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
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
    f.render_widget(Paragraph::new(lines), inner);
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
