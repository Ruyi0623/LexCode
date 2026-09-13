use crate::error::{LexError, Result};
use crate::tools::ToolContext;
use async_trait::async_trait;
use regex::Regex;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// 内置内容搜索:ignore 遍历(尊重 .gitignore)+ regex 行匹配,跨平台一致。
/// 只读、可并发;不调用外部 grep。
pub struct GrepSearch;

const MAX_MATCH_LINES: usize = 200;

fn require_str<'a>(input: &'a Value, key: &str) -> std::result::Result<&'a str, LexError> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| LexError::Tool(format!("grep_search 缺少必填参数 \"{key}\"(字符串)")))
}

fn resolve_path(cwd: &Path, raw: &str) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

fn search_sync(root: &Path, pattern: &str) -> Result<String> {
    let re = Regex::new(pattern)
        .map_err(|e| LexError::Tool(format!("正则表达式无效: {e}")))?;

    let mut out: Vec<String> = Vec::new();
    let mut truncated = false;
    let mut total: usize = 0;

    let walker = ignore::WalkBuilder::new(root)
        .hidden(true) // 隐藏文件默认跳过(.git 等);显式指定的单文件路径不经过 walker
        .require_git(false) // 非仓库目录也应用 .gitignore(与真实项目目录行为一致)
        .build();
    for entry in walker {
        let entry = entry.map_err(|e| LexError::Tool(format!("遍历失败: {e}")))?;
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        // 二进制/超大文件防御:非 UTF-8 文件直接跳过(read_to_string 失败视为跳过而非错误)
        let Ok(text) = std::fs::read_to_string(path) else { continue };

        for (idx, line) in text.lines().enumerate() {
            if re.is_match(line) {
                total += 1;
                if out.len() >= MAX_MATCH_LINES {
                    truncated = true;
                    break;
                }
                out.push(format!("{}:{}: {}", path.display(), idx + 1, line.trim_end()));
            }
        }
        if truncated {
            break;
        }
    }

    if out.is_empty() {
        return Ok(format!("未找到匹配 \"{pattern}\" 的内容"));
    }
    if truncated {
        out.push(format!("...(结果已截断,最多展示 {MAX_MATCH_LINES} 行,实际匹配 {total}+ 行)"));
    }
    Ok(out.join("\n"))
}

#[async_trait]
impl crate::tools::Tool for GrepSearch {
    fn name(&self) -> &str {
        "grep_search"
    }
    fn description(&self) -> &str {
        "在文件内容中搜索正则表达式(尊重 .gitignore,内置实现,跨平台一致)。返回每个匹配的\"文件路径:行号: 行内容\"。参数:path(可选,目录或文件,默认当前工作目录)"
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Rust regex 语法的搜索模式"},
                "path": {"type": "string", "description": "搜索起点:目录或单个文件,相对路径基于工作目录;缺省为工作目录"}
            },
            "required": ["pattern"]
        })
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<String> {
        let pattern = require_str(&input, "pattern")?;
        let root = match input.get("path").and_then(Value::as_str) {
            Some(p) if !p.is_empty() => resolve_path(&ctx.cwd, p),
            _ => ctx.cwd.clone(),
        };
        if !root.exists() {
            return Err(LexError::Tool(format!("搜索路径不存在: {}", root.display())));
        }
        // 阻塞遍历放到 spawn_blocking,避免卡住异步运行时
        let root = root.to_path_buf();
        let pattern = pattern.to_string();
        tokio::task::spawn_blocking(move || search_sync(&root, &pattern))
            .await
            .map_err(|e| LexError::Tool(format!("搜索任务失败: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{Tool, ToolContext};
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lex-grep-test-{tag}"));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn ctx(cwd: PathBuf) -> ToolContext {
        ToolContext { cwd, shell: None, todos: Default::default() }
    }

    #[tokio::test]
    async fn finds_matches_with_path_line_content() {
        let d = temp_dir("basic");
        fs::write(d.join("a.rs"), "fn main() {}\nlet x = todo!();\n").unwrap();
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("sub").join("b.rs"), "todo!(); // another\n").unwrap();

        let out = GrepSearch
            .execute(serde_json::json!({"pattern": "todo!"}), &ctx(d))
            .await
            .unwrap();
        assert!(out.contains("a.rs:2: let x = todo!();"), "实际: {out}");
        assert!(out.contains("b.rs:1:"), "实际: {out}");
    }

    #[tokio::test]
    async fn respects_gitignore() {
        let d = temp_dir("gitignore");
        fs::write(d.join("keep.rs"), "needle here\n").unwrap();
        fs::write(d.join("ignored.rs"), "needle here too\n").unwrap();
        fs::write(d.join(".gitignore"), "ignored.rs\n").unwrap();

        let out = GrepSearch.execute(serde_json::json!({"pattern": "needle"}), &ctx(d)).await.unwrap();
        assert!(out.contains("keep.rs"), "实际: {out}");
        assert!(!out.contains("ignored.rs"), ".gitignore 文件不应出现在结果: {out}");
    }

    #[tokio::test]
    async fn invalid_regex_is_tool_error() {
        let d = temp_dir("badre");
        let err = GrepSearch
            .execute(serde_json::json!({"pattern": "(["}), &ctx(d))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("正则"), "实际: {err}");
    }

    #[tokio::test]
    async fn missing_pattern_is_error() {
        let d = temp_dir("nopattern");
        let err = GrepSearch.execute(serde_json::json!({}), &ctx(d)).await.unwrap_err();
        assert!(err.to_string().contains("pattern"), "实际: {err}");
    }

    #[tokio::test]
    async fn no_match_reports_cleanly() {
        let d = temp_dir("nomatch");
        fs::write(d.join("a.txt"), "hello\n").unwrap();
        let out = GrepSearch
            .execute(serde_json::json!({"pattern": "zzz-not-there"}), &ctx(d))
            .await
            .unwrap();
        assert!(out.contains("未找到匹配"), "实际: {out}");
    }

    #[tokio::test]
    async fn nonexistent_path_is_error() {
        let d = temp_dir("nopath");
        let err = GrepSearch
            .execute(serde_json::json!({"pattern": "x", "path": "does/not/exist"}), &ctx(d))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("不存在"), "实际: {err}");
    }

    #[test]
    fn is_read_only() {
        assert!(GrepSearch.read_only());
    }
}
