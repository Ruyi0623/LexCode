use super::file_read::{require_str, resolve_path};
use super::{Tool, ToolContext};
use crate::error::{LexError, Result};
use serde_json::Value;

pub struct FileEdit;

fn align_line_endings(text: &str, target_crlf: bool) -> String {
    if target_crlf {
        text.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        text.replace("\r\n", "\n")
    }
}

#[async_trait::async_trait]
impl Tool for FileEdit {
    fn name(&self) -> &str { "file_edit" }
    fn description(&self) -> &str { "以最小化定位替换修改文件:old_string 必须在文件中唯一命中,不做整体覆盖" }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "文件路径" },
                "old_string": { "type": "string", "description": "要被替换的精确原文(必须唯一命中)" },
                "new_string": { "type": "string", "description": "替换后的内容" }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }
    fn read_only(&self) -> bool { false }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<String> {
        let raw = require_str(&input, "path")?;
        let old = require_str(&input, "old_string")?;
        let new = require_str(&input, "new_string")?;
        let path = resolve_path(ctx, &raw);
        let content = std::fs::read_to_string(&path)
            .map_err(|e| LexError::Tool(format!("读取 {} 失败: {e}", path.display())))?;

        let crlf = content.contains("\r\n");
        let (old_aligned, new_aligned) = if crlf && !old.contains("\r\n") {
            (align_line_endings(&old, true), align_line_endings(&new, true))
        } else {
            (old.clone(), new.clone())
        };

        let hits = content.matches(&old_aligned).count();
        match hits {
            0 => Err(LexError::Tool(format!("在 {} 中未找到 old_string(若文件为 CRLF 已自动对齐仍失败,请先用 file_read 确认原文)", path.display()))),
            1 => {
                let updated = content.replacen(&old_aligned, &new_aligned, 1);
                std::fs::write(&path, &updated)
                    .map_err(|e| LexError::Tool(format!("写入 {} 失败: {e}", path.display())))?;
                Ok(format!("已修改 {}", path.display()))
            }
            n => Err(LexError::Tool(format!(
                "old_string 在 {} 中命中 {n} 处,要求唯一命中;请扩大上下文使其唯一",
                path.display()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn ctx() -> ToolContext {
        ToolContext { cwd: PathBuf::from(std::env::temp_dir().join("lex-fileedit-test")), shell: None }
    }

    #[tokio::test]
    async fn replaces_unique_match() {
        let c = ctx();
        std::fs::create_dir_all(&c.cwd).unwrap();
        std::fs::write(c.cwd.join("f.rs"), "fn main() {}\n").unwrap();
        FileEdit.execute(json!({"path":"f.rs","old_string":"fn main() {}","new_string":"fn main() { println!(\"hi\"); }"}), &c).await.unwrap();
        assert_eq!(std::fs::read_to_string(c.cwd.join("f.rs")).unwrap(), "fn main() { println!(\"hi\"); }\n");
    }

    #[tokio::test]
    async fn non_unique_match_is_error() {
        let c = ctx();
        std::fs::create_dir_all(&c.cwd).unwrap();
        std::fs::write(c.cwd.join("g.txt"), "x\nx\n").unwrap();
        let err = FileEdit.execute(json!({"path":"g.txt","old_string":"x","new_string":"y"}), &c).await.unwrap_err();
        assert!(err.to_string().contains("2"), "错误应包含命中次数: {err}");
    }

    #[tokio::test]
    async fn missing_match_is_error() {
        let c = ctx();
        std::fs::create_dir_all(&c.cwd).unwrap();
        std::fs::write(c.cwd.join("h.txt"), "abc\n").unwrap();
        let err = FileEdit.execute(json!({"path":"h.txt","old_string":"zzz","new_string":"y"}), &c).await.unwrap_err();
        assert!(matches!(err, LexError::Tool(_)));
    }

    #[tokio::test]
    async fn crlf_file_preserved_with_lf_pattern() {
        let c = ctx();
        std::fs::create_dir_all(&c.cwd).unwrap();
        std::fs::write(c.cwd.join("w.txt"), "a\r\nb\r\nc\r\n").unwrap();
        FileEdit.execute(json!({"path":"w.txt","old_string":"a\nb","new_string":"A\nB"}), &c).await.unwrap();
        let out = std::fs::read_to_string(c.cwd.join("w.txt")).unwrap();
        assert_eq!(out, "A\r\nB\r\nc\r\n", "行尾必须是 CRLF");
    }
}
