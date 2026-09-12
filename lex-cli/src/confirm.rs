use async_trait::async_trait;
use lex_core::error::LexError;
use lex_core::error::Result;
use lex_core::security::PendingAction;
use lex_core::security::PermissionHandler;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;

pub struct CliConfirmHandler;

#[async_trait]
impl PermissionHandler for CliConfirmHandler {
    async fn confirm(&self, action: &PendingAction) -> Result<bool> {
        anstream::println!();
        anstream::println!("⚠ 需要确认 [{}]:", action.tool_name);
        anstream::println!("{}", action.summary);
        anstream::print!("允许执行? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush().ok();

        let mut line = String::new();
        BufReader::new(tokio::io::stdin())
            .read_line(&mut line)
            .await
            .map_err(LexError::Io)?;
        let answer = line.trim().to_ascii_lowercase();
        Ok(answer == "y" || answer == "yes")
    }
}
