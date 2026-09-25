use crate::tui::diff::{line_diff, DiffLine};
use crate::tui::state::AppState;
use crate::ui::theme;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

pub fn draw(app: &mut AppState, f: &mut Frame) {
    // 输入栏下方:上下文容量行(常驻)+ 补全菜单(仅输入 "/" 时出现)
    let menu_open = !app.completion_candidates().is_empty();
    let comp_h = u16::from(menu_open) * 2;
    let chunks = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(comp_h),
        Constraint::Length(1),
    ])
    .split(f.area());
    let cols = Layout::horizontal([Constraint::Percentage(75), Constraint::Percentage(25)]).split(chunks[0]);

    draw_transcript(f, cols[0], app);
    draw_todos(f, cols[1], app);
    draw_input(f, chunks[1], app);
    draw_ctx_usage(f, chunks[2], app);
    if menu_open {
        draw_completions(f, chunks[3], app);
    }
    draw_status(f, chunks[4], app);
    if app.pending_confirm.is_some() {
        draw_confirm(f, app);
    }
    // 设置页全屏覆盖层最后绘制(压住主界面)
    if app.settings.is_some() {
        draw_settings(f, f.area(), app);
    }
}

/// 上下文容量行:输入栏下方常驻("未启用压缩"时只显示已用估算)
fn draw_ctx_usage(f: &mut Frame, area: Rect, app: &AppState) {
    let line = match app.ctx_usage {
        Some((used, 0)) => Line::from(Span::styled(
            format!("上下文 {} tokens(未启用压缩)", used),
            Style::default().fg(theme::C_DIM),
        )),
        Some((used, limit)) if limit > 0 => {
            let pct = (used * 100 / limit.max(1)).min(100);
            // 逼近 80% 自动压缩阈值时改用警示色
            let style = if pct >= 80 {
                Style::default().fg(theme::C_WARN)
            } else {
                Style::default().fg(theme::C_DIM)
            };
            Line::from(Span::styled(
                format!("上下文 {used}/{limit} tokens({pct}%)"),
                style,
            ))
        }
        _ => Line::from(Span::styled("上下文 -", Style::default().fg(theme::C_DIM))),
    };
    f.render_widget(Paragraph::new(line), area);
}

/// 斜杠命令补全菜单:候选一行(选中强调蓝,其余暗色)+ 操作提示行
fn draw_completions(f: &mut Frame, area: Rect, app: &AppState) {
    let cands = app.completion_candidates();
    let sel = app.completion_sel.min(cands.len().saturating_sub(1));
    let spans: Vec<Span> = cands
        .iter()
        .enumerate()
        .flat_map(|(i, c)| {
            let style = if i == sel {
                Style::default().fg(theme::C_ACCENT).add_modifier(ratatui::style::Modifier::BOLD)
            } else {
                Style::default().fg(theme::C_DIM)
            };
            vec![
                Span::styled((*c).to_string(), style),
                Span::raw("  ".to_string()),
            ]
        })
        .collect();
    let hint = Line::from(Span::styled(
        "↑/↓ 选择 · Tab 补全 · Enter 执行",
        Style::default().fg(theme::C_DIM),
    ));
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);
    f.render_widget(Paragraph::new(Line::from(spans)), rows[0]);
    f.render_widget(Paragraph::new(hint), rows[1]);
}

/// 设置页:列表页复用纯渲染函数;详情页渲染可编辑字段(选中光标/编辑态/结果消息)
fn draw_settings(f: &mut Frame, area: Rect, app: &mut AppState) {
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::C_WARN))
        .title(" 设置 ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some((view, page)) = app.settings.as_ref() else { return };
    let mut lines: Vec<Line> = Vec::new();
    if !page.in_detail {
        // 列表页:复用纯渲染函数(输出含 ANSI,进 TUI 前剥掉)
        for l in crate::ui::settings::render_list(view, page.selected) {
            lines.push(Line::from(strip_ansi(&l)));
        }
    } else {
        let title = crate::ui::settings::MODULE_TITLES
            .get(page.selected)
            .copied()
            .unwrap_or("<未知模块>");
        lines.push(Line::from(Span::styled(
            title.to_string(),
            Style::default().fg(theme::C_ACCENT).add_modifier(ratatui::style::Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        let defs = crate::ui::settings::field_defs(page.selected);
        if defs.is_empty() {
            lines.push(Line::from(Span::styled(
                "该模块只读(值来自环境变量与编译期信息)",
                Style::default().fg(theme::C_DIM),
            )));
        }
        for (i, def) in defs.iter().enumerate() {
            let cur = crate::ui::settings::field_display(view, page.selected, i);
            let selected = page.field_sel == i && !page.editing;
            let mark = if selected { "> ".to_string() } else { "  ".to_string() };
            let mark_style = if selected {
                Style::default().fg(theme::C_ACCENT)
            } else {
                Style::default().fg(theme::C_DIM)
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{mark}{}", def.label), mark_style),
                Span::raw(": "),
                Span::styled(cur, Style::default().fg(theme::C_CODE).bg(theme::C_CODE_BG)),
            ]));
        }
        lines.push(Line::from(""));
        if page.editing {
            lines.push(Line::from(vec![
                Span::styled("编辑中: ", Style::default().fg(theme::C_WARN)),
                Span::styled(app.input.clone(), Style::default().fg(theme::C_CODE).bg(theme::C_CODE_BG)),
                Span::styled("   Enter 保存 · Esc 取消", Style::default().fg(theme::C_DIM)),
            ]));
        } else if !defs.is_empty() {
            lines.push(Line::from(Span::styled(
                "↑/↓ 选字段 · Enter 编辑/切换 · Esc 返回列表",
                Style::default().fg(theme::C_DIM),
            )));
        }
        if let Some((msg, ok)) = &app.settings_msg {
            let style = if *ok { Style::default().fg(theme::C_SUCCESS) } else { Style::default().fg(theme::C_ERROR) };
            lines.push(Line::from(Span::styled(msg.clone(), style)));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "保存即写回 lex-code.toml(注释不保留)并热生效;max_children 等个别项重启生效",
            Style::default().fg(theme::C_DIM),
        )));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// 剥离 ANSI 转义序列(CSI:ESC [ ... 终止于单个字母)
fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '' {
            for c2 in chars.by_ref() {
                if c2.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn draw_transcript(f: &mut Frame, area: Rect, app: &mut AppState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::C_BORDER))
        .title(" 对话 ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let width = inner.width as usize;
    let visible = inner.height as usize;
    // 视口必须按"折行后的显示高度"切片,而不是逻辑行数:一行长回复会折成多行,
    // 按逻辑行取窗口会让折行溢出面板底边,回复尾部/刚回显的输入被裁掉看不见
    let per_line: Vec<usize> = app.transcript.iter().map(|l| wrapped_rows(l, width)).collect();
    let total_rows: usize = per_line.iter().sum();
    let max_scroll = total_rows.saturating_sub(visible);
    let scroll = app.scroll.min(max_scroll);
    let bottom = total_rows - scroll;
    let offset = bottom.saturating_sub(visible);
    // 跳过完整落在视口上方的逻辑行;首行内部要跳过的显示行交给 Paragraph.scroll
    let mut start = 0usize;
    let mut acc = 0usize;
    while start < app.transcript.len() && acc + per_line[start] <= offset {
        acc += per_line[start];
        start += 1;
    }
    let skip_first = (offset - acc) as u16;
    let lines: Vec<Line> = app.transcript[start..].to_vec();
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((skip_first, 0)),
        inner,
    );
}

/// 估算逻辑行折行后的显示行数(词级贪心,与 ratatui `Wrap { trim: false }` 行为近似;
/// CJK 无空格按超宽词逐字硬折)。只用于视口定位,允许与实际有 1 行级出入。
fn wrapped_rows(line: &Line, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let mut rows = 1usize;
    let mut cur = 0usize; // 当前行已占显示列
    for word in text.split(' ') {
        let ww = unicode_width::UnicodeWidthStr::width(word);
        if ww == 0 {
            continue;
        }
        if ww > width {
            // 超宽词按字符硬折
            if cur > 0 {
                rows += 1;
            }
            rows += ww / width;
            cur = ww % width;
            if cur == 0 {
                cur = width; // 整除时末行占满
            }
        } else if cur + ww <= width {
            cur += ww;
        } else {
            rows += 1;
            cur = ww;
        }
        // 词后空格占一列(放不下则折行)
        if cur + 1 <= width {
            cur += 1;
        } else {
            rows += 1;
            cur = 1;
        }
    }
    rows
}

fn draw_todos(f: &mut Frame, area: Rect, app: &AppState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::C_BORDER))
        .title(" 待办 ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    // 手工按显示宽度折行:图标与内容首段固定同行,续行悬挂缩进——
    // 交给 Paragraph 会把落在行尾的图标折到单独一行(图标孤立)
    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    for (i, t) in app.todos.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        let (mark, style) = match t.status {
            lex_core::tools::TodoStatus::Pending => ("○", Style::default().fg(theme::C_DIM)),
            lex_core::tools::TodoStatus::InProgress => ("◐", Style::default().fg(theme::C_ACCENT)),
            lex_core::tools::TodoStatus::Completed => ("◉", Style::default().fg(theme::C_SUCCESS)),
        };
        let prefix = format!("{mark} ");
        let indent_w = unicode_width::UnicodeWidthStr::width(prefix.as_str()).min(width.max(1) - 1);
        let indent = " ".repeat(indent_w);
        let mut cur_w = indent_w;
        let mut row = String::new();
        let mut first = true;
        for ch in t.content.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if cur_w + cw > width && cur_w > indent_w {
                if first {
                    lines.push(Line::from(vec![
                        Span::styled(prefix.clone(), style),
                        Span::raw(std::mem::take(&mut row)),
                    ]));
                    first = false;
                } else {
                    lines.push(Line::from(Span::raw(std::mem::take(&mut row))));
                }
                row = indent.clone();
                cur_w = indent_w;
            }
            row.push(ch);
            cur_w += cw;
        }
        if first {
            lines.push(Line::from(vec![Span::styled(prefix, style), Span::raw(row)]));
        } else {
            lines.push(Line::from(Span::raw(row)));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_input(f: &mut Frame, area: Rect, app: &mut AppState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if app.busy { theme::C_BORDER } else { theme::C_ACCENT }))
        .title(" 输入(Enter 提交,Ctrl+C 中断/退出) ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(format!("› {}", app.input)), inner);
    // 终端光标:置于输入框内 app.cursor 指示的位置(CJK 按显示宽度计,超宽钳到框内)
    let before: String = app.input.chars().take(app.cursor).collect();
    let w = unicode_width::UnicodeWidthStr::width(before.as_str()) as u16;
    let x = inner.x + 2 + w;
    let max_x = inner.x + inner.width.saturating_sub(1);
    f.set_cursor_position(ratatui::layout::Position::new(x.min(max_x), inner.y));
}

fn draw_status(f: &mut Frame, area: Rect, app: &AppState) {
    let right = app.usage.clone().unwrap_or_default();
    let left = app.status.clone();
    let mut spans = vec![Span::styled(left, Style::default().fg(theme::C_DIM)), Span::raw("  ")];
    if app.scroll > 0 {
        // 回看中:提示当前偏移,避免用户忘记自己不在底部
        spans.push(Span::styled(
            format!("↑ 回看 {} 行(PgDn 回到底部)", app.scroll),
            Style::default().fg(theme::C_ACCENT),
        ));
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(right, Style::default().fg(theme::C_DIM)));
    f.render_widget(Paragraph::new(ratatui::text::Line::from(spans)), area);
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

    // 内容行(可滚动)+ 固定在底部的快捷键提示行
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
    // 底部留一行给提示;内容按折行后显示高度钳位滚动,长命令/大 diff 能看全
    let content_area = ratatui::layout::Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: inner.height.saturating_sub(1),
    };
    let total_rows: usize = lines.iter().map(|l| wrapped_rows(l, content_area.width as usize)).sum();
    let max_scroll = total_rows.saturating_sub(content_area.height as usize);
    let scroll = (app.confirm_scroll as usize).min(max_scroll);
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((scroll as u16, 0)),
        content_area,
    );
    let hint = if max_scroll > 0 {
        format!("[y] 允许   [n/Esc] 拒绝   ↑↓ 滚动查看({}/{})", scroll, max_scroll)
    } else {
        "[y] 允许   [n/Esc] 拒绝".to_string()
    };
    let hint_area = ratatui::layout::Rect {
        x: inner.x,
        y: inner.y + content_area.height,
        width: inner.width,
        height: 1,
    };
    f.render_widget(Paragraph::new(Line::from(Span::styled(hint, Style::default().fg(theme::C_DIM)))), hint_area);
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

    /// 待办图标为自绘字符三态:◉ 完成(绿)/ ◐ 进行中(蓝)/ ○ 待办(灰)
    #[test]
    fn todo_marks_use_drawn_glyphs() {
        let todos = Arc::new(Mutex::new(vec![
            lex_core::tools::Todo { content: "甲".into(), status: lex_core::tools::TodoStatus::Completed },
            lex_core::tools::Todo { content: "乙".into(), status: lex_core::tools::TodoStatus::InProgress },
            lex_core::tools::Todo { content: "丙".into(), status: lex_core::tools::TodoStatus::Pending },
        ]));
        let mut app = AppState::new();
        app.todos = todos.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let screen = render(&mut app, 80, 24);
        assert!(screen.contains("◉ 甲"), "完成项应显示 ◉,实际:\n{screen}");
        assert!(screen.contains("◐ 乙"), "进行中应显示 ◐,实际:\n{screen}");
        assert!(screen.contains("○ 丙"), "待办项应显示 ○,实际:\n{screen}");
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

    /// 回看必须真的移动视口:默认显示最新行,scroll 后显示最早的行。
    /// (按键路径有 state 层单测;这条守住 draw 层的钳位与切片不出错)
    #[test]
    fn scroll_back_shifts_viewport() {
        use ratatui::text::Line;
        let todos: Arc<Mutex<Vec<lex_core::tools::Todo>>> = Arc::new(Mutex::new(vec![]));
        let mut app = AppState::new();
        for i in 0..40 {
            app.transcript.push(Line::from(format!("行{i:02}")));
        }
        // 80×24:对话区内高约 17 行,未回看时应显示尾部
        let screen = render(&mut app, 80, 24);
        assert!(screen.contains("行39"), "默认应显示最新行,实际:\n{screen}");
        assert!(!screen.contains("行00"), "默认不应显示最早的行,实际:\n{screen}");
        app.scroll = 100; // 超上限会被 draw 钳到最大可回看行数
        let screen = render(&mut app, 80, 24);
        assert!(screen.contains("行00"), "回看后应显示最早的行,实际:\n{screen}");
        assert!(!screen.contains("行39"), "回看后最新行应移出视口,实际:\n{screen}");
    }

    /// 把测试画面压成无空白、无边框的字符串:折行/面板边框不影响内容断言
    fn flatten_screen(screen: &str) -> String {
        screen.chars().filter(|c| !c.is_whitespace() && !"│┌┐└┘├┤┬┴┼─".contains(*c)).collect()
    }
    /// 回归:长回复折行后尾部必须完整可见。曾按逻辑行数取视口窗口,折行溢出
    /// 面板底边把回复最后一句话裁掉,直到用户下次提交才"浮"出来。
    #[test]
    fn wrapped_tail_of_last_reply_fully_visible() {
        let todos: Arc<Mutex<Vec<lex_core::tools::Todo>>> = Arc::new(Mutex::new(vec![]));
        let mut app = AppState::new();
        // 前置填充:每行都折 2 个显示行,制造"逻辑行窗口装不下"的情形
        for i in 0..20 {
            app.apply(UiEvent::TextDelta(format!("第{i}行{}\n", "很".repeat(60))), &todos);
        }
        app.apply(UiEvent::TextDelta(format!("{}结尾标记", "很".repeat(200))), &todos);
        app.apply(UiEvent::TurnDone { ok: true, message: String::new() }, &todos);
        let screen = render(&mut app, 80, 24);
        // 折行点可能落在标记中间,去掉换行后整体匹配
        let flat = flatten_screen(&screen);
        assert!(
            flat.contains("结尾标记"),
            "最后一行回复的折行尾部必须完整可见,实际:\n{screen}"
        );
    }

    /// 回归:超长待办描述必须折行完整显示(List 一行一项不折行,超宽直接截断)
    #[test]
    fn long_todo_content_wraps_instead_of_truncating() {
        let todos = Arc::new(Mutex::new(vec![lex_core::tools::Todo {
            content: format!("{}任务结尾", "很".repeat(40)),
            status: lex_core::tools::TodoStatus::InProgress,
        }]));
        let mut app = AppState::new();
        app.todos = todos.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let screen = render(&mut app, 80, 24);
        let flat = flatten_screen(&screen);
        assert!(
            flat.contains("任务结尾"),
            "长待办描述折行后尾部应完整可见,实际:\n{screen}"
        );
    }

    /// 回归:弹层内容超长时必须可滚动看全(内容区滚动,提示行固定底部)
    #[test]
    fn confirm_modal_long_content_scrollable() {
        let mut app = AppState::new();
        app.open_confirm(PendingAction {
            tool_name: "bash_exec".into(),
            summary: "执行长命令".into(),
            detail: lex_core::security::PendingDetail::Bash {
                command: format!("deploy --flag {}TAILMARK", "x".repeat(800)),
            },
        });
        // 顶部视角:长命令的尾部被视口裁掉
        let screen = render(&mut app, 80, 24);
        let flat = flatten_screen(&screen);
        assert!(!flat.contains("TAILMARK"), "顶部视角不应看到尾部:
{screen}");
        // 滚到最底:尾部完整可见
        app.confirm_scroll = 100; // draw 内会钳位到最大可滚行数
        let screen = render(&mut app, 80, 24);
        let flat = flatten_screen(&screen);
        assert!(flat.contains("TAILMARK"), "滚动后应看到长内容尾部:
{screen}");
        assert!(screen.contains("滚动查看"), "可滚时提示行应带滚动指示");
    }

    /// 输入栏下方:上下文容量行常驻,输入 "/" 时出现补全菜单
    #[test]
    fn ctx_usage_row_and_completion_menu_render() {
        let mut app = AppState::new();
        app.ctx_usage = Some((3200, 64000));
        for c in "/se".chars() {
            app.input.push(c);
        }
        app.cursor = 3;
        let screen = render(&mut app, 80, 24);
        assert!(screen.contains("上下文 3200/64000 tokens(5%)"), "容量行应显示:
{screen}");
        assert!(screen.contains("/settings"), "补全菜单应列出前缀命中的命令:
{screen}");
        assert!(!screen.contains("/compact"), "未命中前缀的命令不应列出:
{screen}");
        assert!(screen.contains("Tab 补全"), "应有操作提示:
{screen}");
    }

    /// 容量逼近 80% 阈值时容量行百分比仍正确(颜色切警示由样式决定,断言数值)
    #[test]
    fn ctx_usage_row_warns_near_threshold() {
        let mut app = AppState::new();
        app.ctx_usage = Some((60000, 64000));
        app.input = "/".into();
        app.cursor = 1;
        let screen = render(&mut app, 80, 24);
        assert!(screen.contains("(93%)"), "百分比应正确:
{screen}");
        assert!(screen.contains("/settings"), "空前缀应列出全部命令:
{screen}");
        assert!(screen.contains("/compact"), "空前缀应列出全部命令:
{screen}");
        assert!(screen.contains("/init"), "空前缀应列出全部命令:
{screen}");
    }

    /// 待办折行时图标必须与内容首段同行,续行悬挂缩进(不出现孤立图标行)
    #[test]
    fn todo_icon_stays_with_content_on_wrap() {
        let todos = Arc::new(Mutex::new(vec![lex_core::tools::Todo {
            content: format!("{}结尾", "很".repeat(40)),
            status: lex_core::tools::TodoStatus::InProgress,
        }]));
        let mut app = AppState::new();
        app.todos = todos.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let screen = render(&mut app, 80, 24);
        assert!(screen.contains("◐ 很"), "图标应与内容首段同行:
{screen}");
        assert!(screen.contains("│  很"), "续行应悬挂缩进两个显示列:
{screen}");
        assert!(
            !screen.contains("◐
"),
            "不应出现孤立图标行:
{screen}"
        );
    }

    /// 终端光标始终置于输入框内(输入文本末尾之后)
    #[test]
    fn input_cursor_sits_in_input_box() {
        use ratatui::backend::Backend as _;
        let mut app = AppState::new();
        app.input = "abc".into();
        app.cursor = 3;
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap_or_else(|e| panic!("{e}"));
        term.draw(|f| draw(&mut app, f)).unwrap_or_else(|e| panic!("{e}"));
        let pos = term.backend_mut().get_cursor_position().unwrap_or_else(|e| panic!("{e}"));
        // 24 行高:输入盒在底部区域(最后 4 行内)
        assert!(pos.y >= 20, "光标应在输入盒行上,实际 y={}", pos.y);
        // "› abc" 之后:列 >= 提示符 2 格 + 3 字符
        assert!(pos.x >= 6, "光标应在输入文本之后,实际 x={}", pos.x);
        assert!(pos.x < 79, "光标不应越出输入盒右边界,实际 x={}", pos.x);
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
