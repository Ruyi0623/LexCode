use std::path::Path;

/// AGENTS.md 注入标题(一次组装进入缓存前缀,此后逐字节不变)
pub const INJECTION_HEADER: &str = "# 项目指引(AGENTS.md)";

/// 会话启动检测项目根目录的 AGENTS.md,存在则读取(设计文档第 9 节)。
/// 读取失败(权限等)按不存在处理,不阻断会话。
pub fn load_agents_md(cwd: &Path) -> Option<String> {
    let text = std::fs::read_to_string(cwd.join("AGENTS.md")).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// 注入到系统提示词之后(一次组装,进入缓存前缀)。
/// 除追加段外,基础提示词逐字节不变。
pub fn assemble_system_prompt(base: &str, agents_md: Option<&str>) -> String {
    match agents_md {
        Some(md) => format!("{base}\n\n{INJECTION_HEADER}\n\n{md}"),
        None => base.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lex-agents-md-test-{tag}"));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn loads_agents_md_from_cwd() {
        let d = temp_dir("present");
        fs::write(d.join("AGENTS.md"), "# 指引\n用中文回复\n").unwrap();
        let md = load_agents_md(&d).unwrap();
        assert!(md.contains("用中文回复"));
    }

    #[test]
    fn missing_or_empty_agents_md_is_none() {
        let d = temp_dir("absent");
        assert!(load_agents_md(&d).is_none());
        fs::write(d.join("AGENTS.md"), "   \n").unwrap();
        assert!(load_agents_md(&d).is_none());
    }

    #[test]
    fn assemble_appends_after_base_byte_identical_prefix() {
        let base = "系统提示词";
        let out = assemble_system_prompt(base, Some("指引内容"));
        assert!(out.starts_with(base));
        assert!(out.contains(INJECTION_HEADER));
        assert!(out.ends_with("指引内容"));
        // 无 AGENTS.md 时逐字节不变
        assert_eq!(assemble_system_prompt(base, None), base);
    }
}
