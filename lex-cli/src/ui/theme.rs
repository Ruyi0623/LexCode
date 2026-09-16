/// 蓝色主题:全部界面颜色的唯一定义处。
/// 其他文件禁止出现裸 \x1b[;输出一律经 anstream(老终端自动降级)。
pub const ACCENT: &str = "\x1b[38;2;59;130;246m"; // 强调蓝 RGB(59,130,246)
pub const ACCENT_BOLD: &str = "\x1b[1;38;2;59;130;246m";
pub const DIM: &str = "\x1b[2m";
pub const ERROR: &str = "\x1b[31m";
pub const WARN: &str = "\x1b[33m";
pub const SUCCESS: &str = "\x1b[32m";
pub const THINKING: &str = "\x1b[2;3m";
pub const BOLD: &str = "\x1b[1m";
pub const ITALIC: &str = "\x1b[3m";
/// 亮青:行内代码
pub const CODE: &str = "\x1b[96m";
pub const RESET: &str = "\x1b[0m";
/// 回到行首并清除整行(重绘内容行用)
pub const CLEAR_LINE: &str = "\r\x1b[2K";
/// 整屏清除 + 光标归位(设置页等全屏界面进出用;3J 连同回滚缓冲一起清)
pub const CLEAR_SCREEN: &str = "\x1b[2J\x1b[3J\x1b[H";

pub fn accent(s: &str) -> String {
    format!("{ACCENT}{s}{RESET}")
}
pub fn accent_bold(s: &str) -> String {
    format!("{ACCENT_BOLD}{s}{RESET}")
}
pub fn dim(s: &str) -> String {
    format!("{DIM}{s}{RESET}")
}
pub fn error(s: &str) -> String {
    format!("{ERROR}{s}{RESET}")
}
pub fn warn(s: &str) -> String {
    format!("{WARN}{s}{RESET}")
}
pub fn success(s: &str) -> String {
    format!("{SUCCESS}{s}{RESET}")
}
pub fn bold(s: &str) -> String {
    format!("{BOLD}{s}{RESET}")
}
/// 光标定位到第 n 列(ANSI G 序列,1-based)
pub fn goto_col(n: usize) -> String {
    format!("\x1b[{n}G")
}
