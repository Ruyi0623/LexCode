use crate::ui::theme;

/// 流式 Markdown 渲染:行缓冲,整行到达后应用样式输出;围栏状态跨行持有。
/// 行内支持 **粗体**、*斜体*、`代码`;行级支持 # 标题、-/* 列表、``` 围栏代码块。
pub struct MarkdownStream {
    buf: String,
    in_fence: bool,
}

impl MarkdownStream {
    pub fn new() -> Self {
        MarkdownStream { buf: String::new(), in_fence: false }
    }

    /// 喂入增量文本;每凑齐一行即输出渲染结果(以 \n 结尾)
    pub fn feed(&mut self, chunk: &str, out: &mut dyn FnMut(&str)) {
        self.buf.push_str(chunk);
        while let Some(pos) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=pos).collect();
            let line = line.trim_end_matches('\n');
            out(&format!("{}\n", self.render_line(line)));
        }
    }

    /// 流结束/切换轨道时调用:渲染残留的不完整行(无换行结尾)
    pub fn flush(&mut self, out: &mut dyn FnMut(&str)) {
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            out(&self.render_line(&line));
        }
    }

    fn render_line(&mut self, line: &str) -> String {
        let t = line.trim_start();
        if t.starts_with("```") {
            self.in_fence = !self.in_fence;
            return String::new(); // 围栏标记本身不显示
        }
        if self.in_fence {
            return theme::dim(line);
        }
        if t.starts_with('#') {
            let stripped = t.trim_start_matches('#').trim_start();
            if !stripped.is_empty() {
                // 标题整体加粗,且内部行内样式仍要生效 —— 否则 `# **粗体**` 会显示字面星号
                return theme::bold(&replace_inline(stripped));
            }
        }
        if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            // 列表项同样必须过一遍行内样式:模型最常用的 `- **标签**: …` 形状不能显示字面星号
            return format!("{}•{} {}", theme::ACCENT, theme::RESET, replace_inline(rest));
        }
        replace_inline(t)
    }
}

impl Default for MarkdownStream {
    fn default() -> Self {
        Self::new()
    }
}

/// 把一整段文本按行渲染为终端 Markdown(非流式场景用)。
pub fn render_str(text: &str) -> String {
    let mut md = MarkdownStream::new();
    let mut out = String::new();
    md.feed(text, &mut |s| out.push_str(s));
    md.flush(&mut |s| out.push_str(s));
    out
}

/// 单轮模式(一次性任务)收尾该打印什么:
/// - stdout 是 TTY(交互终端):正文已随流式事件渲染打印过 → `None`,不重复输出;
/// - stdout 非 TTY(管道/重定向):返回**渲染过的**完整正文,给脚本一份干净结果。
pub fn one_shot_tail(text: &str, stdout_is_tty: bool) -> Option<String> {
    if stdout_is_tty {
        return None;
    }
    let mut out = render_str(text);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Some(out)
}

/// 行内样式:**粗体**、*斜体*、`代码`。斜体只在 `*` 后跟非空白时开启,
/// 避免 "a * b" 这类乘号被误判;未闭合的样式在行尾复位,避免颜色外溢。
fn replace_inline(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut in_code = false;
    let mut in_bold = false;
    let mut in_italic = false;
    while i < chars.len() {
        let c = chars[i];
        if in_code {
            if c == '`' {
                in_code = false;
                out.push_str(theme::RESET);
            } else {
                out.push(c);
            }
            i += 1;
            continue;
        }
        match c {
            '`' => {
                in_code = true;
                out.push_str(theme::CODE);
            }
            '*' if in_italic => {
                in_italic = false;
                out.push_str(theme::RESET);
            }
            '*' if i + 1 < chars.len() && chars[i + 1] == '*' => {
                in_bold = !in_bold;
                out.push_str(if in_bold { theme::BOLD } else { theme::RESET });
                i += 2;
                continue;
            }
            '*' if i + 1 < chars.len() && chars[i + 1] != ' ' => {
                in_italic = true;
                out.push_str(theme::ITALIC);
            }
            _ => out.push(c),
        }
        i += 1;
    }
    if in_code || in_bold || in_italic {
        out.push_str(theme::RESET);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(chunks: &[&str]) -> String {
        let mut md = MarkdownStream::new();
        let mut out = String::new();
        for c in chunks {
            md.feed(c, &mut |s| out.push_str(s));
        }
        md.flush(&mut |s| out.push_str(s));
        out
    }

    #[test]
    fn bold_and_inline_code() {
        assert_eq!(
            render(&["**粗体** 和 `code`\n"]),
            format!(
                "{}粗体{} 和 {}code{}\n",
                theme::BOLD, theme::RESET, theme::CODE, theme::RESET
            )
        );
    }

    #[test]
    fn partial_line_flushed_at_end() {
        assert_eq!(
            render(&["**粗", "体**"]),
            format!("{}粗体{}", theme::BOLD, theme::RESET)
        );
    }

    #[test]
    fn italic_needs_non_space_after_star() {
        assert_eq!(render(&["a * b\n"]), "a * b\n");
        assert_eq!(
            render(&["这是 *重点* 说明\n"]),
            format!("这是 {}重点{} 说明\n", theme::ITALIC, theme::RESET)
        );
    }

    #[test]
    fn heading_and_bullet() {
        assert_eq!(
            render(&["# 标题\n- 项一\n* 项二\n"]),
            format!(
                "{}标题{}\n{}•{} 项一\n{}•{} 项二\n",
                theme::BOLD, theme::RESET, theme::ACCENT, theme::RESET, theme::ACCENT, theme::RESET
            )
        );
    }

    #[test]
    fn fence_block_is_dim_and_hidden() {
        assert_eq!(
            render(&["```rust\nfn a() {}\n```\nafter\n"]),
            format!(
                "\n{}fn a() {{}}{}\n\nafter\n",
                theme::DIM,
                theme::RESET
            )
        );
    }

    #[test]
    fn unclosed_inline_style_resets_at_line_end() {
        assert_eq!(
            render(&["**粗体"]),
            format!("{}粗体{}", theme::BOLD, theme::RESET)
        );
    }

    #[test]
    fn inline_styles_apply_inside_bullet_and_heading() {
        // 回归:模型最常用的形状恰是「列表项 + 粗体标签」——本项目自己的子 agent 摘要格式
        // (`- **做了什么**: …`)就是这种。修复前 render_line 对列表项/标题是直接 return 的,
        // 跳过了 replace_inline,于是终端上显示字面 `**`。
        assert_eq!(
            render(&["- **做了什么**: 读了文件\n"]),
            format!(
                "{}•{} {}做了什么{}: 读了文件\n",
                theme::ACCENT,
                theme::RESET,
                theme::BOLD,
                theme::RESET
            )
        );
        // 标题内的行内代码同样要生效(标题整体加粗)
        assert_eq!(
            render(&["# 用法 `--help`\n"]),
            format!(
                "{}用法 {}--help{}{}\n",
                theme::BOLD,
                theme::CODE,
                theme::RESET,
                theme::RESET
            )
        );
    }

    #[test]
    fn one_shot_tail_skips_reprint_on_tty_and_renders_for_pipes() {
        // 单轮模式收尾:TTY 下正文已随流式渲染打印过,再打印一遍就是重复输出;
        // 非 TTY(管道/重定向)下要给脚本一份 **渲染过的** 完整正文,而不是带字面 `**` 的原文。
        let text = "**重点** 说明\n第二行";
        assert_eq!(one_shot_tail(text, true), None, "TTY 下不得重复输出正文");

        let piped = one_shot_tail(text, false).expect("非 TTY 应产出一份收尾正文");
        assert!(
            piped.contains(&theme::BOLD.to_string()),
            "收尾正文应经 markdown 渲染: {piped:?}"
        );
        assert!(!piped.contains("**"), "不应残留字面星号: {piped:?}");
        assert!(piped.ends_with('\n'), "应补上结尾换行: {piped:?}");
    }
}
