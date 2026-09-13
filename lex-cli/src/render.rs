use lex_core::provider::ProviderEvent;

/// 有状态事件渲染器:跟踪思考流是否正在输出,以便正文开始时换行分轨。
#[derive(Default)]
pub struct Renderer {
    in_thinking: bool,
}

impl Renderer {
    pub fn new() -> Self {
        Renderer::default()
    }

    pub fn render(&mut self, event: &ProviderEvent) {
        match event {
            ProviderEvent::TextDelta(t) => {
                if self.in_thinking {
                    // 思考流结束,正文另起一行
                    anstream::println!();
                    self.in_thinking = false;
                }
                anstream::print!("{t}");
                use std::io::Write;
                std::io::stdout().flush().ok();
            }
            // DeepSeek 思考模式(delta.reasoning_content)实时渲染为暗色斜体
            ProviderEvent::ThinkingDelta(t) => {
                self.in_thinking = true;
                anstream::print!("\x1b[2;3m{t}\x1b[0m");
                use std::io::Write;
                std::io::stdout().flush().ok();
            }
            ProviderEvent::ToolUseStart { name, .. } => {
                if self.in_thinking {
                    anstream::println!();
                    self.in_thinking = false;
                }
                anstream::println!();
                anstream::println!("\x1b[36m▸ 正在执行: {name}\x1b[0m");
            }
            ProviderEvent::ToolUseDelta { .. } => {}
            ProviderEvent::ToolUseComplete { .. } => {}
            ProviderEvent::Completed { usage } => {
                if self.in_thinking {
                    anstream::println!();
                    self.in_thinking = false;
                }
                anstream::println!();
                // 缓存命中只在 DeepSeek(或 Anthropic 显式缓存)有数据时显示
                if usage.cache_hit_tokens > 0 {
                    anstream::println!(
                        "\x1b[2m(输入 {} tokens · 输出 {} tokens · 缓存命中 {})\x1b[0m",
                        usage.input_tokens,
                        usage.output_tokens,
                        usage.cache_hit_tokens
                    );
                } else {
                    anstream::println!(
                        "\x1b[2m(输入 {} tokens · 输出 {} tokens)\x1b[0m",
                        usage.input_tokens,
                        usage.output_tokens
                    );
                }
            }
        }
    }
}
