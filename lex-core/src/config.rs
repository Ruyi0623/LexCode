use crate::error::{LexError, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub provider: String,
    pub system_prompt_path: Option<String>,
    pub anthropic: AnthropicConfig,
    pub shell: ShellConfig,
    pub max_turns: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AnthropicConfig {
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ShellConfig {
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            provider: "anthropic".into(),
            system_prompt_path: None,
            anthropic: AnthropicConfig::default(),
            shell: ShellConfig::default(),
            max_turns: 50,
        }
    }
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        AnthropicConfig { base_url: String::new(), model: String::new(), max_tokens: 8192 }
    }
}

impl Config {
    pub fn load(cwd: &Path) -> Result<Config> {
        Self::load_with_env_prefix(cwd, "LEX")
    }

    pub fn load_with_env_prefix(cwd: &Path, env_prefix: &str) -> Result<Config> {
        let mut cfg: Config = match cwd.join("lex-code.toml").exists() {
            true => {
                let raw = std::fs::read_to_string(cwd.join("lex-code.toml"))?;
                toml::from_str(&raw).map_err(|e| LexError::Config(format!("lex-code.toml 解析失败: {e}")))?
            }
            false => Config::default(),
        };
        if let Some(url) = env_option(&format!("{env_prefix}_ANTHROPIC_BASE_URL")) {
            cfg.anthropic.base_url = url;
        }
        if cfg.anthropic.base_url.is_empty() {
            return Err(LexError::Config(
                "缺少 Anthropic base_url:请在 lex-code.toml 的 [anthropic] 段设置 base_url,或设置环境变量 LEX_ANTHROPIC_BASE_URL(本项目不允许内置默认 URL)".into(),
            ));
        }
        if cfg.anthropic.model.is_empty() {
            return Err(LexError::Config(
                "缺少模型名:请在 lex-code.toml 的 [anthropic] 段设置 model(本项目不允许内置默认模型)".into(),
            ));
        }
        if cfg.provider != "anthropic" {
            return Err(LexError::Config(format!(
                "Phase 1 仅支持 provider = \"anthropic\",当前为 \"{}\"",
                cfg.provider
            )));
        }
        Ok(cfg)
    }

    pub fn api_key_env(provider: &str) -> &'static str {
        match provider {
            "anthropic" => "LEX_ANTHROPIC_API_KEY",
            _ => "LEX_API_KEY",
        }
    }
}

pub fn resolve_api_key(provider: &str) -> Result<String> {
    let name = Config::api_key_env(provider);
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| LexError::Config(format!("缺少 API Key:请设置环境变量 {name}(本项目不从配置文件读取凭证)")))
}

fn env_option(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lex-config-test-{tag}"));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn missing_base_url_is_config_error() {
        let d = temp_dir("no-url");
        let err = Config::load(&d).unwrap_err();
        assert!(err.to_string().contains("base_url"), "实际: {err}");
    }

    #[test]
    fn toml_provides_base_url_and_model() {
        let d = temp_dir("with-url");
        fs::write(
            d.join("lex-code.toml"),
            "[anthropic]\nbase_url = \"http://127.0.0.1:9\"\nmodel = \"claude-test\"\n",
        )
        .unwrap();
        let cfg = Config::load(&d).unwrap();
        assert_eq!(cfg.provider, "anthropic");
        assert_eq!(cfg.anthropic.model, "claude-test");
        assert_eq!(cfg.anthropic.max_tokens, 8192); // 默认采样上限
        assert_eq!(cfg.max_turns, 50);
    }

    #[test]
    fn env_overrides_toml_base_url() {
        let d = temp_dir("env-url");
        fs::write(
            d.join("lex-code.toml"),
            "[anthropic]\nbase_url = \"http://127.0.0.1:9\"\nmodel = \"m\"\n",
        )
        .unwrap();
        unsafe { std::env::set_var("LEX_TEST_ANTHROPIC_BASE_URL", "http://127.0.0.1:8") };
        let cfg = Config::load_with_env_prefix(&d, "LEX_TEST").unwrap();
        assert_eq!(cfg.anthropic.base_url, "http://127.0.0.1:8");
    }
}
