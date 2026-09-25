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
    fn description(&self) -> &str { "以最小化定位替换修改文件:old_string 必须在文件中唯一命中;新建文件或写入空文件时 old_string 置空、new_string 传完整内容,不支持覆盖非空文件" }
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

        // 空 old_string = 新建/写空文件语义:唯一能"从零创建内容"的工具路径,
        // 非空文件必须走唯一命中替换,避免空 old_string 误匹配任意位置造成整体覆盖
        if old.is_empty() {
            if path.exists() {
                let existing = std::fs::read_to_string(&path)
                    .map_err(|e| LexError::Tool(format!("读取 {} 失败: {e}", path.display())))?;
                if !existing.is_empty() {
                    return Err(LexError::Tool(format!(
                        "空 old_string 仅支持新建文件或写入空文件;{} 已有内容({} 字节),请提供唯一的 old_string 做替换",
                        path.display(),
                        existing.len()
                    )));
                }
            } else if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| LexError::Tool(format!("创建目录 {} 失败: {e}", dir.display())))?;
            }
            std::fs::write(&path, new.as_bytes())
                .map_err(|e| LexError::Tool(format!("写入 {} 失败: {e}", path.display())))?;
            return Ok(format!("已写入 {}", path.display()));
        }

        let content = std::fs::read_to_string(&path).map_err(|e| {
            if !path.exists() {
                LexError::Tool(format!(
                    "读取 {} 失败:文件不存在;新建文件请把 old_string 置空、new_string 传完整内容",
                    path.display()
                ))
            } else {
                LexError::Tool(format!("读取 {} 失败: {e}", path.display()))
            }
        })?;

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
        ToolContext { cwd: PathBuf::from(std::env::temp_dir().join("lex-fileedit-test")), shell: None, todos: Default::default(), spawner: None }
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

    /// 空 old_string = 新建语义:文件不存在时创建(含父目录)。
    /// 共享临时目录可能残留上次运行的产物,先清理避免固定文件名互相污染
    #[tokio::test]
    async fn empty_old_string_creates_new_file() {
        let c = ctx();
        let target = c.cwd.join("sub").join("dir").join("poem.txt");
        let _ = std::fs::remove_file(&target);
        FileEdit.execute(json!({"path":"sub/dir/poem.txt","old_string":"","new_string":"第一行\n第二行\n"}), &c).await.unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "第一行\n第二行\n");
    }

    /// 空 old_string 可写入已存在的空文件(修复"echo 建了空文件却没工具能填内容"的缺口)
    #[tokio::test]
    async fn empty_old_string_writes_empty_file() {
        let c = ctx();
        std::fs::create_dir_all(&c.cwd).unwrap();
        std::fs::write(c.cwd.join("empty-write.txt"), "").unwrap();
        FileEdit.execute(json!({"path":"empty-write.txt","old_string":"","new_string":"内容\n"}), &c).await.unwrap();
        assert_eq!(std::fs::read_to_string(c.cwd.join("empty-write.txt")).unwrap(), "内容\n");
    }

    /// 回归:空 old_string 对非空文件必须拒绝——曾因 "" 命中 N+1 处报"命中 5 处"误导模型死循环
    #[tokio::test]
    async fn empty_old_string_rejected_on_non_empty_file() {
        let c = ctx();
        std::fs::create_dir_all(&c.cwd).unwrap();
        std::fs::write(c.cwd.join("nonempty-reject.txt"), "已有内容\n").unwrap();
        let err = FileEdit.execute(json!({"path":"nonempty-reject.txt","old_string":"","new_string":"覆盖"}), &c).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("唯一"), "应指引走唯一替换: {msg}");
        assert!(msg.contains("已有内容"), "报错应带文件字节数,实际: {msg}");
    }

    /// 不存在的文件 + 非空 old_string:报错应指引"置空 old_string 新建"
    #[tokio::test]
    async fn missing_file_with_non_empty_old_gives_creation_hint() {
        let c = ctx();
        let err = FileEdit.execute(json!({"path":"no-such-hint.txt","old_string":"x","new_string":"y"}), &c).await.unwrap_err();
        assert!(err.to_string().contains("置空"), "报错应包含新建指引: {err}");
    }
}
