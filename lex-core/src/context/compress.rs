use crate::error::Result;
use crate::message::{Block, Message, Role};
use crate::provider::{Provider, ProviderEvent, RequestContext};
use futures::StreamExt;

/// 内部摘要器提示词(仅用于历史压缩,不是运行时系统提示词,允许内置)
const SUMMARIZER_SYSTEM: &str = "你是会话历史压缩器。把给定的编程 agent 对话历史压缩成一段摘要,必须保留:关键决策及理由、涉及的文件路径、未完成的待办、重要的命令与结果。直接输出摘要正文,不要寒暄。";

/// 单个块文本超过该长度截断(摘要请求本身也要可控)
const BLOCK_TEXT_LIMIT: usize = 2000;

/// 本地 token 估算启发式:字符数 ÷ 4(设计文档第 9 节)。
/// 只计历史消息:system 提示词体积固定且不可压缩,计入会让首次判定必然跨阈值。
pub fn estimate_tokens(messages: &[Message]) -> u64 {
    let mut chars: usize = 0;
    for m in messages {
        for b in &m.content {
            let text = match b {
                Block::Text { text } => text,
                Block::Thinking { reasoning_content } => reasoning_content,
                Block::ToolUse { name, input, .. } => {
                    chars += name.chars().count();
                    chars += input.to_string().chars().count();
                    continue;
                }
                Block::ToolResult { content, .. } => content,
            };
            chars += text.chars().count();
        }
    }
    (chars as u64).div_ceil(4)
}

/// 找最后一段"完整轮次"的起点作为压缩切点:
/// 即最后一条以 Text 开头的 user 消息(每轮任务都由它开启,切它前面不会拆散
/// tool_use/tool_result 配对)。返回 None 表示没有可安全切分的位置。
pub fn find_cut_index(messages: &[Message]) -> Option<usize> {
    messages
        .iter()
        .rposition(|m| m.role == Role::User && matches!(m.content.first(), Some(Block::Text { .. })))
        .filter(|&i| i > 0) // 至少要留出一段可摘要的历史
}

/// 把历史渲染成摘要器可读的文本(思维链不参与摘要)
pub fn render_history_text(messages: &[Message]) -> String {
    let mut out = String::new();
    for m in messages {
        out.push_str(match m.role {
            Role::User => "## 用户\n",
            Role::Assistant => "## 助手\n",
        });
        for b in &m.content {
            match b {
                Block::Text { text } => out.push_str(&truncate(text)),
                Block::Thinking { .. } => {}
                Block::ToolUse { name, input, .. } => {
                    out.push_str(&format!("[调用工具 {name}] {}\n", truncate(&input.to_string())));
                }
                Block::ToolResult { content, is_error, .. } => {
                    let tag = if *is_error { "工具结果(失败)" } else { "工具结果" };
                    out.push_str(&format!("[{tag}] {}\n", truncate(content)));
                }
            }
        }
        out.push('\n');
    }
    out
}

fn truncate(s: &str) -> String {
    let mut end = BLOCK_TEXT_LIMIT;
    while !s.is_char_boundary(end.min(s.len())) {
        end -= 1;
    }
    if s.len() <= end {
        s.to_string()
    } else {
        format!("{}…(截断)", &s[..end])
    }
}

/// 调当前 provider 对早期历史生成摘要(设计文档第 9 节)。
/// 失败向上返回,由调用方决定降级(记 warning、跳过本次压缩)。
pub async fn summarize(provider: &dyn Provider, early: &[Message]) -> Result<String> {
    let transcript = render_history_text(early);
    let ctx = RequestContext {
        system: SUMMARIZER_SYSTEM.into(),
        tools: vec![],
        messages: vec![Message::user_text(format!(
            "以下是需要压缩的对话历史:\n\n{transcript}\n请输出摘要。"
        ))],
    };
    let mut stream = provider.send(ctx).await?;
    let mut parts: Vec<String> = Vec::new();
    while let Some(item) = stream.next().await {
        if let ProviderEvent::TextDelta(t) = item? {
            parts.push(t);
        }
    }
    let summary = parts.join("").trim().to_string();
    if summary.is_empty() {
        return Err(crate::error::LexError::Provider("摘要器返回空内容".into()));
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_is_chars_over_four() {
        // 8 个字符 ÷ 4 = 2
        assert_eq!(estimate_tokens(&[Message::user_text("一二三四五六七八")]), 2);
        assert_eq!(estimate_tokens(&[]), 0);
    }

    #[test]
    fn cut_is_last_user_turn_start() {
        let history = vec![
            Message::user_text("任务一"),
            Message::assistant(vec![Block::Text { text: "答".into() }]),
            Message::user_text("任务二"),
            Message::assistant(vec![
                Block::ToolUse { id: "t".into(), name: "file_read".into(), input: serde_json::json!({}) },
            ]),
            Message::tool_results(vec![("t", "内容".into(), false)]),
            Message::assistant(vec![Block::Text { text: "完成".into() }]),
        ];
        // 最后一段完整轮次从 "任务二" 开始;切在它前面不会拆散 tool 配对
        assert_eq!(find_cut_index(&history), Some(2));
        // 单条消息:没有可摘要的前缀
        assert_eq!(find_cut_index(&[Message::user_text("x")]), None);
    }

    #[test]
    fn render_skips_thinking_and_labels_roles() {
        let history = vec![
            Message::user_text("你好"),
            Message::assistant(vec![
                Block::Thinking { reasoning_content: "内部思考".into() },
                Block::ToolUse { id: "t".into(), name: "grep_search".into(), input: serde_json::json!({"pattern":"x"}) },
            ]),
            Message::tool_results(vec![("t", "命中".into(), false)]),
        ];
        let text = render_history_text(&history);
        assert!(text.contains("## 用户"));
        assert!(text.contains("## 助手"));
        assert!(text.contains("[调用工具 grep_search]"));
        assert!(text.contains("[工具结果] 命中"));
        assert!(!text.contains("内部思考"), "思维链不参与摘要");
    }

    #[test]
    fn truncate_respects_char_boundary() {
        let long = "汉".repeat(BLOCK_TEXT_LIMIT + 100);
        let out = truncate(&long);
        assert!(out.ends_with("…(截断)"));
        assert!(out.chars().count() <= BLOCK_TEXT_LIMIT + 8);
    }
}
