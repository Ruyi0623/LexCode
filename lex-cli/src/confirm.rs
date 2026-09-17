use async_trait::async_trait;
use lex_core::error::LexError;
use lex_core::error::Result;
use lex_core::security::PendingAction;
use lex_core::security::PermissionHandler;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::io::Stdin;

/// 全 CLI 共享的 stdin 读取器。
/// 确认弹窗与交互主循环必须走同一个 BufReader:
/// 若每次确认新建 BufReader,其预读会吞掉管道中尚未消费的行,缓冲随对象丢弃而丢失。
pub struct CliInput {
    reader: tokio::sync::Mutex<BufReader<Stdin>>,
}

impl CliInput {
    pub fn new() -> Self {
        CliInput { reader: tokio::sync::Mutex::new(BufReader::new(tokio::io::stdin())) }
    }

    /// 打印提示并读取一行;EOF 返回空字符串。
    pub async fn read_line(&self, prompt: &str) -> Result<String> {
        let mut reader = self.reader.lock().await;
        read_line_locked(&mut reader, prompt).await
    }
}

/// 在已持有 stdin 锁的前提下打印提示并读取一行;EOF 返回空字符串。
/// 提示与读取必须在同一把锁内完成:否则并发确认的横幅会在读取串行化之前交叉打印,
/// 用户眼前的提示与真正消费其输入的那次确认可能不是同一个。
async fn read_line_locked(reader: &mut BufReader<Stdin>, prompt: &str) -> Result<String> {
    anstream::print!("{prompt}");
    use std::io::Write;
    std::io::stdout().flush().ok();

    let mut line = String::new();
    let n = reader.read_line(&mut line).await.map_err(LexError::Io)?;
    if n == 0 {
        return Ok(String::new());
    }
    Ok(line)
}

impl Default for CliInput {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PermissionHandler for CliInput {
    async fn confirm(&self, action: &PendingAction) -> Result<bool> {
        use crate::ui::theme;
        // 先取锁再打印:并发确认(如两个 spawn_subagent 同时待批)时,横幅与提问
        // 必须和它的那次读取一起串行化,否则用户可能批准屏幕上临近的另一个动作。
        let mut reader = self.reader.lock().await;
        anstream::println!();
        anstream::println!("{}", theme::warn(&format!("⚠ 需要确认 [{}]", action.tool_name)));
        anstream::println!("{}", action.summary);
        let answer = read_line_locked(&mut reader, &format!("{} 允许执行? [y/N] ", theme::accent("?"))).await?;
        let answer = answer.trim().to_ascii_lowercase();
        Ok(answer == "y" || answer == "yes")
    }
}
