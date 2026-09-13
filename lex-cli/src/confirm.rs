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
        anstream::print!("{prompt}");
        use std::io::Write;
        std::io::stdout().flush().ok();

        let mut line = String::new();
        let n = self
            .reader
            .lock()
            .await
            .read_line(&mut line)
            .await
            .map_err(LexError::Io)?;
        if n == 0 {
            return Ok(String::new());
        }
        Ok(line)
    }
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
        anstream::println!();
        anstream::println!("{}", theme::warn(&format!("⚠ 需要确认 [{}]", action.tool_name)));
        anstream::println!("{}", action.summary);
        let answer = self
            .read_line(&format!("{} 允许执行? [y/N] ", theme::accent("?")))
            .await?;
        let answer = answer.trim().to_ascii_lowercase();
        Ok(answer == "y" || answer == "yes")
    }
}
