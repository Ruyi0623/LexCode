use crate::error::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const KNOWN_VARS: &[&str] = &["CWD", "OS", "PROJECT_TYPE", "TOOL_TODO"];

pub const DEFAULT_ASSET_NAME: &str = "coding-agent-system-prompt.md";

pub fn render_template(template: &str, vars: &BTreeMap<&str, String>) -> String {
    let mut out = template.to_string();
    for key in KNOWN_VARS {
        if let Some(val) = vars.get(*key) {
            let placeholder = format!("{{{{{key}}}}}"); // 产出 {{CWD}} 形式,与提示词文件中的写法逐字一致
            out = out.replace(&placeholder, val);
        }
    }
    out
}

pub fn resolve_system_prompt_path(cwd: &Path, configured: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = configured {
        let p = PathBuf::from(p);
        if p.is_absolute() {
            if p.is_file() { return Some(p); }
        } else {
            let abs = cwd.join(&p);
            if abs.is_file() { return Some(abs); }
        }
        return None;
    }
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf()));
    let mut candidates = vec![cwd.join("assets").join(DEFAULT_ASSET_NAME)];
    if let Some(dir) = exe_dir {
        candidates.push(dir.join("assets").join(DEFAULT_ASSET_NAME));
    }
    candidates.into_iter().find(|c| c.is_file())
}

pub fn load_system_prompt(path: &Path) -> Result<String> {
    Ok(std::fs::read_to_string(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn replaces_known_vars_only() {
        let mut vars = BTreeMap::new();
        vars.insert("CWD", "/w".to_string());
        vars.insert("OS", "linux".to_string());
        let t = "目录 {{CWD}} 系统 {{OS}} 未知 {{UNKNOWN}} 保留 {{CWD}}";
        let out = render_template(t, &vars);
        assert_eq!(out, "目录 /w 系统 linux 未知 {{UNKNOWN}} 保留 /w");
    }

    #[test]
    fn no_vars_means_byte_identical() {
        let t = "逐字节 #不变! \n\t内容";
        assert_eq!(render_template(t, &BTreeMap::new()), t);
    }

    #[test]
    fn resolve_prefers_configured_then_cwd_then_exe() {
        let d = std::env::temp_dir().join("lex-prompt-test");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("assets")).unwrap();
        std::fs::write(d.join("assets").join("coding-agent-system-prompt.md"), "x").unwrap();
        let hit = resolve_system_prompt_path(&d, None).unwrap();
        assert_eq!(hit, d.join("assets").join("coding-agent-system-prompt.md"));
        assert!(resolve_system_prompt_path(&d, Some("no/such/file.md")).is_none());
    }
}
