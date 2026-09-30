use super::file_edit::align_line_endings;
use super::file_read::{require_str, resolve_path};
use super::{Tool, ToolContext};
use crate::error::{LexError, Result};
use serde_json::Value;

pub struct FileWrite;

#[async_trait::async_trait]
impl Tool for FileWrite {
    fn name(&self) -> &str { "file_write" }
    fn description(&self) -> &str { "整文件写入:新建文件,或用 content 整体覆盖已有文件(父目录自动创建)。局部修改请用 file_edit 做唯一命中替换,不要用本工具做小改动" }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "文件路径" },
                "content": { "type": "string", "description": "完整文件内容(整体覆盖,未提及的原文内容会丢失)" }
            },
            "required": ["path", "content"]
        })
    }
    fn read_only(&self) -> bool { false }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<String> {
        let raw = require_str(&input, "path")?;
        let content = require_str(&input, "content")?;
        let path = resolve_path(ctx, &raw);

        // 覆盖已有 CRLF 文件且新内容未带 \r\n 时对齐行尾,与 file_edit 的保留原行尾约定一致
        let existed = path.is_file();
        let aligned = if existed {
            let existing = std::fs::read_to_string(&path)
                .map_err(|e| LexError::Tool(format!("读取 {} 失败: {e}", path.display())))?;
            existing.contains("\r\n") && !content.contains("\r\n")
        } else {
            false
        };
        let content = if aligned { align_line_endings(&content, true) } else { content };

        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| LexError::Tool(format!("创建目录 {} 失败: {e}", dir.display())))?;
        }
        std::fs::write(&path, content.as_bytes())
            .map_err(|e| LexError::Tool(format!("写入 {} 失败: {e}", path.display())))?;
        if aligned {
            Ok(format!("已覆盖 {}(已对齐原文件 CRLF 行尾)", path.display()))
        } else if existed {
            Ok(format!("已覆盖 {}", path.display()))
        } else {
            Ok(format!("已创建 {}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn ctx() -> ToolContext {
        ToolContext { cwd: PathBuf::from(std::env::temp_dir().join("lex-filewrite-test")), shell: None, todos: Default::default(), spawner: None }
    }

    #[tokio::test]
    async fn creates_new_file_with_parent_dirs() {
        let c = ctx();
        let target = c.cwd.join("nested").join("dir").join("new.txt");
        let _ = std::fs::remove_file(&target);
        FileWrite.execute(json!({"path":"nested/dir/new.txt","content":"第一行\n第二行\n"}), &c).await.unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "第一行\n第二行\n");
    }

    #[tokio::test]
    async fn overwrites_existing_file_entirely() {
        let c = ctx();
        std::fs::create_dir_all(&c.cwd).unwrap();
        std::fs::write(c.cwd.join("over.txt"), "旧的很长\n的内容\n全部保留\n").unwrap();
        FileWrite.execute(json!({"path":"over.txt","content":"全新内容\n"}), &c).await.unwrap();
        assert_eq!(std::fs::read_to_string(c.cwd.join("over.txt")).unwrap(), "全新内容\n");
    }

    /// 覆盖 CRLF 文件:LF 内容自动对齐为 CRLF(与 file_edit 的保留原行尾约定一致)
    #[tokio::test]
    async fn crlf_existing_file_aligns_lf_content() {
        let c = ctx();
        std::fs::create_dir_all(&c.cwd).unwrap();
        std::fs::write(c.cwd.join("crlf.txt"), "a\r\nb\r\n").unwrap();
        FileWrite.execute(json!({"path":"crlf.txt","content":"x\ny\n"}), &c).await.unwrap();
        assert_eq!(std::fs::read_to_string(c.cwd.join("crlf.txt")).unwrap(), "x\r\ny\r\n", "行尾必须是 CRLF");
    }

    /// 新内容已显式带 CRLF 时不做二次转换
    #[tokio::test]
    async fn explicit_crlf_content_untouched() {
        let c = ctx();
        std::fs::create_dir_all(&c.cwd).unwrap();
        std::fs::write(c.cwd.join("crlf2.txt"), "a\r\nb\r\n").unwrap();
        FileWrite.execute(json!({"path":"crlf2.txt","content":"x\r\ny\r\n"}), &c).await.unwrap();
        assert_eq!(std::fs::read_to_string(c.cwd.join("crlf2.txt")).unwrap(), "x\r\ny\r\n");
    }

    /// LF 文件覆盖后保持 LF,不被误对齐
    #[tokio::test]
    async fn lf_existing_file_stays_lf() {
        let c = ctx();
        std::fs::create_dir_all(&c.cwd).unwrap();
        std::fs::write(c.cwd.join("lf.txt"), "a\nb\n").unwrap();
        FileWrite.execute(json!({"path":"lf.txt","content":"x\ny\n"}), &c).await.unwrap();
        assert_eq!(std::fs::read_to_string(c.cwd.join("lf.txt")).unwrap(), "x\ny\n");
    }

    /// 整文件写入允许清空文件(显式覆盖语义,与 file_edit 的拒绝覆盖不同)
    #[tokio::test]
    async fn empty_content_truncates_file() {
        let c = ctx();
        std::fs::create_dir_all(&c.cwd).unwrap();
        std::fs::write(c.cwd.join("trunc.txt"), "有内容\n").unwrap();
        FileWrite.execute(json!({"path":"trunc.txt","content":""}), &c).await.unwrap();
        assert_eq!(std::fs::read_to_string(c.cwd.join("trunc.txt")).unwrap(), "");
    }

    #[tokio::test]
    async fn missing_content_param_is_error() {
        let c = ctx();
        let err = FileWrite.execute(json!({"path":"x.txt"}), &c).await.unwrap_err();
        assert!(err.to_string().contains("content"), "实际: {err}");
    }
}
