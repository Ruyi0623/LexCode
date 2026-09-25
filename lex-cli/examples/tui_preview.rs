//! 用真实 TUI 渲染路径(ratatui `TestBackend` + `tui::draw::draw`)把界面导出成 SVG 静态图。
//!
//! 之所以走 `TestBackend` 而不是截图:它跑的就是线上同一套 `draw()` 代码,
//! 产出的图与真实终端逐格一致(含颜色、边框、弹层压盖),可在 CI 复现,无截图噪声。
//!
//! `#[path]` 把 `ui` / `tui` 模块树引进本 example  crate(lex-cli 只有 bin 目标,
//! 没有 lib 目标;为一张预览图去拆 lib 目标不划算)。
//!
//! 用法(仓库根目录):
//! ```bash
//! cargo run -p lex-cli --example tui_preview -- docs/assets/tui-main.svg docs/assets/tui-confirm.svg
//! ```
//! 不带参数时写到 `docs/assets/tui-{main,confirm}.svg`。

#![allow(dead_code)]

#[path = "../src/ui/mod.rs"]
pub mod ui;
#[path = "../src/tui/mod.rs"]
pub mod tui;
// 主程序里 `confirm` 也是 crate 根模块;`ui/input.rs` 用 `crate::confirm::CliInput`,
// 这里必须挂上同名路径,否则该模块编译不过。
#[path = "../src/confirm.rs"]
pub mod confirm;

// `tui/run.rs` 经 `crate::detect_project_type` 取项目类型;example 里给同签名桩。
fn detect_project_type(_cwd: &std::path::Path) -> String {
    "Rust".into()
}

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use ratatui::Terminal;
use tui::draw::draw;
use tui::event::UiEvent;
use tui::state::AppState;

const CELL_W: usize = 9;
const CELL_H: usize = 18;
const BG: &str = "#0d1117"; // GitHub Dark 终端底色
const FG: &str = "#c9d1d9";
const FONT: &str = "ui-monospace, SFMono-Regular, Menlo, Consolas, 'DejaVu Sans Mono', monospace";
const W: u16 = 110;
const H: u16 = 32;

// ---------- 纯函数:Buffer → SVG ----------

/// ratatui `Color` → CSS 颜色(与 GitHub Dark 终端配色对齐)
fn color_hex(c: Color) -> String {
    match c {
        Color::Reset => FG.to_string(),
        Color::Black => "#000000".to_string(),
        Color::Red => "#f85149".to_string(),
        Color::Green => "#3fb950".to_string(),
        Color::Yellow => "#d29922".to_string(),
        Color::Blue => "#58a6ff".to_string(),
        Color::Magenta => "#bc8cff".to_string(),
        Color::Cyan => "#39c5cf".to_string(),
        Color::Gray => "#8b949e".to_string(),
        Color::DarkGray => "#8b949e".to_string(),
        Color::LightRed => "#ff7b72".to_string(),
        Color::LightGreen => "#56d364".to_string(),
        Color::LightYellow => "#e3b341".to_string(),
        Color::LightBlue => "#79c0ff".to_string(),
        Color::LightMagenta => "#d2a8ff".to_string(),
        Color::LightCyan => "#56d4dd".to_string(),
        Color::White => "#c9d1d9".to_string(),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(i) => format!("#{i:02x}{i:02x}{i:02x}"),
    }
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// 渲染缓冲 → SVG:先铺背景色块(弹层 `Clear` 的黑色底要能压住下层文字),再按同色聚合输出文字。
pub fn buffer_to_svg(buf: &Buffer) -> String {
    let w = buf.area.width as usize;
    let h = buf.area.height as usize;
    let px_w = w * CELL_W;
    let px_h = h * CELL_H;
    let mut out = String::new();
    out.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{px_w}\" height=\"{px_h}\" viewBox=\"0 0 {px_w} {px_h}\">\n"
    ));
    out.push_str(&format!("<rect width=\"100%\" height=\"100%\" fill=\"{BG}\"/>\n"));

    // 1) 背景块:显式设了 bg 的格子(弹层 Clear 出来的黑底、diff 高亮等)
    for y in 0..h {
        for x in 0..w {
            let cell = &buf[(x as u16, y as u16)];
            if cell.bg != Color::Reset {
                let fill = color_hex(cell.bg);
                out.push_str(&format!(
                    "<rect x=\"{}\" y=\"{}\" width=\"{CELL_W}\" height=\"{CELL_H}\" fill=\"{fill}\"/>\n",
                    x * CELL_W,
                    y * CELL_H
                ));
            }
        }
    }

    // 2) 文字:同一行内把连续同色格子聚成一段 <text>
    out.push_str(&format!(
        "<style>text{{font-family:{FONT};font-size:{}px;white-space:pre}}</style>\n",
        CELL_H * 3 / 4
    ));
    for y in 0..h {
        let mut x = 0usize;
        while x < w {
            let first = &buf[(x as u16, y as u16)];
            // 宽字符(CJK)的后续占位格不产出文字(``Cell::skip`` 是公有字段)
            if first.skip {
                x += 1;
                continue;
            }
            let fg = color_hex(first.fg);
            let start = x;
            let mut run = String::new();
            while x < w {
                let c = &buf[(x as u16, y as u16)];
                if c.skip {
                    x += 1;
                    continue;
                }
                let s = c.symbol();
                if s.trim().is_empty() || color_hex(c.fg) != fg {
                    break;
                }
                run.push_str(s);
                x += 1;
            }
            let lead = run.len() - run.trim_start().len();
            let body = run.trim().to_string();
            if body.is_empty() {
                continue;
            }
            let tx = (start + lead) * CELL_W;
            let ty = y * CELL_H + CELL_H * 3 / 4;
            out.push_str(&format!("<text x=\"{tx}\" y=\"{ty}\" fill=\"{fg}\">{}</text>\n", escape_xml(&body)));
        }
    }

    out.push_str("</svg>\n");
    out
}

// ---------- 渲染一个 AppState 到 Buffer(走线上同一条 draw 路径) ----------

fn render_app(app: &mut AppState, w: u16, h: u16) -> Buffer {
    let backend = TestBackend::new(w, h);
    let mut term = Terminal::new(backend).unwrap_or_else(|e| panic!("创建 TestBackend 终端失败: {e}"));
    term.draw(|f| draw(app, f)).unwrap_or_else(|e| panic!("渲染失败: {e}"));
    term.backend().buffer().clone()
}

/// 主界面:对话历史(用户回显 + 模型回复 + 工具活动行)、右侧待办、底部用量尾注
pub fn main_view() -> AppState {
    let todos = std::sync::Mutex::new(vec![
        lex_core::tools::Todo { content: "读 AGENTS.md 与 smoke 第 11 节".into(), status: lex_core::tools::TodoStatus::Completed },
        lex_core::tools::Todo { content: "cargo test --workspace(214 个)".into(), status: lex_core::tools::TodoStatus::InProgress },
        lex_core::tools::Todo { content: "推送前复查密钥与 target-dir".into(), status: lex_core::tools::TodoStatus::Pending },
    ]);
    let mut app = AppState::new();
    app.apply(UiEvent::TurnStarted, &todos);
    app.push_user_input("帮我把 Phase 6 的改动整理成提交并推送到 GitHub");
    app.apply(
        UiEvent::TextDelta("好的。我先复核工作树与测试状态,再整理提交、生成预览图,最后更新自述文件。".into()),
        &todos,
    );
    app.apply(UiEvent::ToolStart { name: "bash_exec".into() }, &todos);
    app.apply(
        UiEvent::ToolResult { tool_name: "bash_exec".into(), first_line: "214 passed · 0 failed".into(), is_error: false },
        &todos,
    );
    // todo_write 的结果会把共享待办重读进面板
    app.apply(
        UiEvent::ToolResult { tool_name: "todo_write".into(), first_line: "待办已更新".into(), is_error: false },
        &todos,
    );
    app.apply(
        UiEvent::TextDelta("\n待办已同步到右侧面板。要我现在生成 TUI 预览图并写进 README 吗?".into()),
        &todos,
    );
    app.apply(UiEvent::Usage { input: 25482, output: 612, cache_hit: 23804 }, &todos);
    app.status = "就绪 · Rust".into();
    app
}

/// 权限确认弹层:file_edit 的带色 diff(核心差异化交互)
pub fn confirm_view() -> AppState {
    let todos = std::sync::Mutex::new(Vec::new());
    let mut app = AppState::new();
    app.apply(UiEvent::TextDelta("我先加上折行,避免超宽正文被截断。".into()), &todos);
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.apply(
        UiEvent::Confirm {
            action: lex_core::security::PendingAction {
                tool_name: "file_edit".into(),
                summary: "编辑文件: lex-cli/src/tui/draw.rs".into(),
                detail: lex_core::security::PendingDetail::FileEdit {
                    path: "lex-cli/src/tui/draw.rs".into(),
                    old_string: "use ratatui::widgets::Paragraph;\n\nf.render_widget(Paragraph::new(lines), inner);\n".into(),
                    new_string: "use ratatui::widgets::{Paragraph, Wrap};\n\nf.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);\n".into(),
                },
            },
            responder: tx,
        },
        &todos,
    );
    let _ = rx; // 静态图不等裁决
    app.status = "就绪 · Rust".into();
    app
}

fn main() {
    // 默认写到仓库根 docs/assets/;也可用参数指定
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (main_path, confirm_path) = if args.len() >= 2 {
        (std::path::PathBuf::from(&args[0]), std::path::PathBuf::from(&args[1]))
    } else {
        (root.join("docs/assets/tui-main.svg"), root.join("docs/assets/tui-confirm.svg"))
    };

    let main_svg = buffer_to_svg(&render_app(&mut main_view(), W, H));
    let confirm_svg = buffer_to_svg(&render_app(&mut confirm_view(), W, H));
    for (path, svg) in [(&main_path, &main_svg), (&confirm_path, &confirm_svg)] {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).unwrap_or_else(|e| eprintln!("创建目录 {} 失败: {e}", dir.display()));
        }
        std::fs::write(path, svg).unwrap_or_else(|e| eprintln!("写入 {} 失败: {e}", path.display()));
        println!("{} ({} 字节)", path.display(), std::fs::metadata(path).map(|m| m.len()).unwrap_or(0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_contains_all_regions_history_and_echo() {
        let svg = buffer_to_svg(&render_app(&mut main_view(), W, H));
        assert!(svg.contains("对话"), "缺对话区标题");
        assert!(svg.contains("待办"), "缺待办面板标题");
        assert!(svg.contains("输入"), "缺输入盒");
        assert!(svg.contains("214 passed"), "缺工具结果行");
        assert!(svg.contains("› 帮我把"), "缺用户输入回显");
        assert!(svg.contains("缓存命中 23804"), "缺用量尾注");
    }

    /// 核心差异化:确认弹层必须给命令原文/diff,而不是笼统提问
    #[test]
    fn svg_modal_shows_colored_diff_and_shortcuts() {
        let svg = buffer_to_svg(&render_app(&mut confirm_view(), W, H));
        assert!(svg.contains("确认操作"), "缺弹层标题");
        assert!(svg.contains("[y] 允许"), "缺快捷键提示");
        assert!(svg.contains("+ use ratatui::widgets::{Paragraph, Wrap};"), "缺新增行:\n{svg}");
        assert!(svg.contains("- use ratatui::widgets::Paragraph;"), "缺删除行");
        assert!(svg.contains("#3fb950"), "diff 增行应为绿");
        assert!(svg.contains("#f85149"), "diff 删行应为红");
        assert!(svg.contains("#000000"), "弹层应铺黑底压住下层");
    }

    #[test]
    fn svg_dimensions_follow_buffer_size() {
        let svg = buffer_to_svg(&render_app(&mut main_view(), W, H));
        assert!(svg.contains(&format!("width=\"{}\"", W as usize * CELL_W)));
        assert!(svg.contains(&format!("height=\"{}\"", H as usize * CELL_H)));
    }
}
