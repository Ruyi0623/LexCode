use lex_core::provider::ProviderEvent;

pub fn render_event(event: &ProviderEvent) {
    match event {
        ProviderEvent::TextDelta(t) => {
            anstream::print!("{t}");
            use std::io::Write;
            std::io::stdout().flush().ok();
        }
        ProviderEvent::ThinkingDelta(_) => {} // Phase 1 Anthropic 路径不产生
        ProviderEvent::ToolUseStart { name, .. } => {
            anstream::println!();
            anstream::println!("\x1b[36m▸ 正在执行: {name}\x1b[0m");
        }
        ProviderEvent::ToolUseDelta { .. } => {}
        ProviderEvent::ToolUseComplete { .. } => {}
        ProviderEvent::Completed { usage } => {
            anstream::println!();
            anstream::println!(
                "\x1b[2m(输入 {} tokens · 输出 {} tokens)\x1b[0m",
                usage.input_tokens,
                usage.output_tokens
            );
        }
    }
}
