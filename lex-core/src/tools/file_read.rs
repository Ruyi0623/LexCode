use super::{Tool, ToolContext};
use crate::error::{LexError, Result};
use serde_json::Value;
use std::path::PathBuf;

pub struct FileRead;

pub fn require_str(input: &Value, key: &str) -> Result<String> {
    match input.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        _ => Err(LexError::Tool(format!("参数 {key} 必须是字符串"))),
    }
}

pub fn resolve_path(ctx: &ToolContext, raw: &str) -> PathBuf {
    let p = PathBuf::from(raw);
    if p.is_absolute() { p } else { ctx.cwd.join(p) }
}

#[async_trait::async_trait]
impl Tool for FileRead {
    fn name(&self) -> &str { "file_read" }
    fn description(&self) -> &str { "读取指定路径的文本文件内容(相对路径基于当前工作目录)" }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string", "description": "文件路径" } },
            "required": ["path"]
        })
    }
    fn read_only(&self) -> bool { true }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<String> {
        let raw = require_str(&input, "path")?;
        let path = resolve_path(ctx, &raw);
        std::fs::read_to_string(&path).map_err(|e| LexError::Tool(format!("读取 {} 失败: {e}", path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn ctx() -> ToolContext {
        ToolContext { cwd: PathBuf::from(std::env::temp_dir().join("lex-fileread-test")), shell: None, todos: Default::default(), spawner: None }
    }

    #[tokio::test]
    async fn reads_relative_file() {
        let c = ctx();
        std::fs::create_dir_all(&c.cwd).unwrap();
        std::fs::write(c.cwd.join("a.txt"), "hello").unwrap();
        let out = FileRead.execute(json!({"path": "a.txt"}), &c).await.unwrap();
        assert_eq!(out, "hello");
    }

    #[tokio::test]
    async fn missing_path_is_tool_error() {
        let c = ctx();
        let err = FileRead.execute(json!({"path": "no/such.txt"}), &c).await.unwrap_err();
        assert!(matches!(err, crate::error::LexError::Tool(_)));
    }

    #[tokio::test]
    async fn path_must_be_string() {
        let c = ctx();
        let err = FileRead.execute(json!({"path": 1}), &c).await.unwrap_err();
        assert!(matches!(err, crate::error::LexError::Tool(_)));
    }
}
