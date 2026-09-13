use crate::ui::theme;

const WORDMARK: &str = "\
██╗     ███████╗██╗  ██╗     ██████╗ ██████╗ ██████╗ ███████╗
██║     ██╔════╝╚██╗██╔╝    ██╔════╝██╔═══██╗██╔══██╗██╔════╝
██║     █████╗   ╚███╔╝     ██║     ██║   ██║██║  ██║█████╗
██║     ██╔══╝   ██╔██╗     ██║     ██║   ██║██║  ██║██╔══╝
███████╗███████╗██╔╝ ██╗    ╚██████╗╚██████╔╝██████╔╝███████╗
╚══════╝╚══════╝╚═╝  ╚═╝     ╚═════╝ ╚═════╝ ╚═════╝ ╚══════╝";

/// 启动画面:仅交互模式调用;一次性输出,不进入任何请求 payload。
pub fn print(provider: &str, model: &str, cwd: &std::path::Path, project_type: &str) {
    let (term_w, _) = crossterm::terminal::size().unwrap_or((100, 30));
    anstream::println!();
    let widest = WORDMARK.lines().map(|l| l.chars().count()).max().unwrap_or(0);
    if term_w as usize >= widest + 2 {
        for line in WORDMARK.lines() {
            anstream::println!("{}{line}{}", theme::ACCENT, theme::RESET);
        }
    } else {
        anstream::println!("{}██ Lex Code{}", theme::ACCENT, theme::RESET);
    }
    anstream::println!("{}v{}{}", theme::DIM, env!("CARGO_PKG_VERSION"), theme::RESET);
    anstream::println!();
    anstream::println!("{}●{} provider: {provider} · model: {model}", theme::ACCENT, theme::RESET);
    anstream::println!(
        "{}●{} 目录: {} · {project_type}",
        theme::ACCENT,
        theme::RESET,
        cwd.display()
    );
    anstream::println!("{}", theme::dim("? Ctrl+C 清空/退出 · Ctrl+D 退出 · ↑↓ 翻历史"));
    anstream::println!();
}
