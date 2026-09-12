use super::{ShellCommand, Tool, ToolContext};
use crate::error::{LexError, Result};
use serde_json::Value;
use tokio::process::Command;

pub struct BashExec;

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_TIMEOUT_SECS: u64 = 600;
const MAX_OUTPUT_CHARS: usize = 20_000;

impl ShellCommand {
    pub fn platform_default() -> ShellCommand {
        #[cfg(windows)]
        {
            ShellCommand { command: "cmd".into(), args: vec!["/C".into()] }
        }
        #[cfg(not(windows))]
        {
            ShellCommand { command: "/bin/sh".into(), args: vec!["-c".into()] }
        }
    }
}

fn truncate(label: &str, s: &str) -> String {
    if s.chars().count() <= MAX_OUTPUT_CHARS {
        s.to_string()
    } else {
        let head: String = s.chars().take(MAX_OUTPUT_CHARS).collect();
        format!("{head}\n…[{label} 超过 {MAX_OUTPUT_CHARS} 字符,已截断]")
    }
}

#[async_trait::async_trait]
impl Tool for BashExec {
    fn name(&self) -> &str { "bash_exec" }
    fn description(&self) -> &str { "在项目目录用 shell 执行命令并返回 stdout/stderr 与退出码" }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "要执行的命令" },
                "timeout_secs": { "type": "integer", "description": "超时秒数,默认 120,上限 600" }
            },
            "required": ["command"]
        })
    }
    fn read_only(&self) -> bool { false }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<String> {
        let cmd = super::file_read::require_str(&input, "command")?;
        let timeout_secs = match input.get("timeout_secs") {
            Some(Value::Number(n)) => n.as_u64().unwrap_or(DEFAULT_TIMEOUT_SECS).min(MAX_TIMEOUT_SECS),
            _ => DEFAULT_TIMEOUT_SECS,
        };
        let shell = ctx.shell.clone().unwrap_or_else(ShellCommand::platform_default);

        let mut command = Command::new(&shell.command);
        command
            .args(&shell.args)
            .arg(&cmd)
            .current_dir(&ctx.cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let child = command
            .spawn()
            .map_err(|e| LexError::Tool(format!("无法启动 shell {}: {e}", shell.command)))?;
        let output = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), child.wait_with_output())
            .await
            .map_err(|_| LexError::Tool(format!("命令超时(>{timeout_secs}s): {cmd}")))?
            .map_err(|e| LexError::Tool(format!("命令执行失败: {e}")))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(format!(
            "exit code: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            output.status.code().unwrap_or(-1),
            truncate("stdout", stdout.trim_end()),
            truncate("stderr", stderr.trim_end())
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ctx() -> ToolContext {
        ToolContext { cwd: PathBuf::from(std::env::temp_dir()), shell: None }
    }

    #[tokio::test]
    async fn echo_returns_stdout() {
        let c = ctx();
        let cmd = if cfg!(windows) { "echo hello" } else { "echo hello" };
        let out = BashExec.execute(serde_json::json!({"command": cmd}), &c).await.unwrap();
        assert!(out.contains("hello"), "实际: {out}");
    }

    #[tokio::test]
    async fn nonzero_exit_includes_status_not_err() {
        let c = ctx();
        let cmd = if cfg!(windows) { "cmd /C exit 3" } else { "exit 3" };
        let out = BashExec.execute(serde_json::json!({"command": cmd}), &c).await.unwrap();
        assert!(out.contains("exit code: 3"), "实际: {out}");
    }

    #[tokio::test]
    async fn timeout_is_tool_error() {
        let c = ctx();
        let cmd = if cfg!(windows) { "ping -n 5 127.0.0.1 > NUL" } else { "sleep 5" };
        let err = BashExec.execute(serde_json::json!({"command": cmd, "timeout_secs": 1}), &c).await.unwrap_err();
        assert!(matches!(err, LexError::Tool(_)));
    }

    #[test]
    fn platform_default_matches_os() {
        let d = ShellCommand::platform_default();
        if cfg!(windows) {
            assert_eq!(d.command, "cmd");
            assert_eq!(d.args, vec!["/C"]);
        } else {
            assert_eq!(d.command, "/bin/sh");
            assert_eq!(d.args, vec!["-c"]);
        }
    }
}
