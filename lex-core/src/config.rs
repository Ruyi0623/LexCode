use crate::error::{LexError, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub provider: String,
    pub system_prompt_path: Option<String>,
    pub anthropic: AnthropicConfig,
    pub openai: OpenAiConfig,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct OpenAiConfig {
    pub base_url: String,
    pub model: String,
    /// 不设置时不发送 max_tokens,由服务端按模式取默认
    /// (DeepSeek:非思考 8K / 思考 64K,避免固定 8K 截断思维链)
    pub max_tokens: Option<u32>,
    /// DeepSeek 思考模式开关:"enabled" / "disabled";不设置走服务端默认
    pub thinking: Option<String>,
    /// DeepSeek 思考强度:"none" / "low" / "high" / "max";不设置走服务端默认
    pub reasoning_effort: Option<String>,
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
            openai: OpenAiConfig::default(),
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

impl Default for OpenAiConfig {
    fn default() -> Self {
        OpenAiConfig { base_url: String::new(), model: String::new(), max_tokens: None, thinking: None, reasoning_effort: None }
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
        if let Some(url) = env_option(&format!("{env_prefix}_OPENAI_BASE_URL")) {
            cfg.openai.base_url = url;
        }
        if !matches!(cfg.provider.as_str(), "anthropic" | "openai") {
            return Err(LexError::Config(format!(
                "不支持的 provider \"{}\":可选 \"anthropic\" 或 \"openai\"(OpenAI 兼容端点)",
                cfg.provider
            )));
        }
        // 校验当前所选 provider 自己的配置段;另一段允许留空(切换 provider 只改配置)
        let (section, base_url, model) = if cfg.provider == "anthropic" {
            ("[anthropic]", cfg.anthropic.base_url.clone(), cfg.anthropic.model.clone())
        } else {
            ("[openai]", cfg.openai.base_url.clone(), cfg.openai.model.clone())
        };
        if base_url.is_empty() {
            return Err(LexError::Config(format!(
                "缺少 base_url:请在 lex-code.toml 的 {section} 段设置 base_url,或设置对应的环境变量(本项目不允许内置默认 URL)"
            )));
        }
        if model.is_empty() {
            return Err(LexError::Config(format!(
                "缺少模型名:请在 lex-code.toml 的 {section} 段设置 model(本项目不允许内置默认模型)"
            )));
        }
        // DeepSeek 参数取值校验(文档:create-chat-completion)
        if let Some(t) = &cfg.openai.thinking {
            if !matches!(t.as_str(), "enabled" | "disabled") {
                return Err(LexError::Config(format!(
                    "openai.thinking 取值 \"{t}\" 无效:只支持 \"enabled\" / \"disabled\""
                )));
            }
        }
        if let Some(e) = &cfg.openai.reasoning_effort {
            if !matches!(e.as_str(), "none" | "low" | "high" | "max") {
                return Err(LexError::Config(format!(
                    "openai.reasoning_effort 取值 \"{e}\" 无效:只支持 \"none\" / \"low\" / \"high\" / \"max\""
                )));
            }
        }
        Ok(cfg)
    }

    pub fn api_key_env(provider: &str) -> &'static str {
        match provider {
            "anthropic" => "LEX_ANTHROPIC_API_KEY",
            "openai" => "LEX_OPENAI_API_KEY",
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

    #[test]
    fn openai_provider_requires_its_own_base_url_and_model() {
        let d = temp_dir("openai-empty");
        fs::write(d.join("lex-code.toml"), "provider = \"openai\"\n").unwrap();
        let err = Config::load(&d).unwrap_err();
        assert!(err.to_string().contains("base_url"), "实际: {err}");

        let d2 = temp_dir("openai-full");
        fs::write(
            d2.join("lex-code.toml"),
            "provider = \"openai\"\n[openai]\nbase_url = \"http://127.0.0.1:7\"\nmodel = \"deepseek-test\"\n",
        )
        .unwrap();
        let cfg = Config::load(&d2).unwrap();
        assert_eq!(cfg.openai.model, "deepseek-test");
        // max_tokens 未设置 → 不发送,走服务端默认(思考模式 64K)
        assert_eq!(cfg.openai.max_tokens, None);
        assert_eq!(cfg.openai.thinking, None);
        // anthropic 段未配置时不得误用 openai 的 base_url 通过校验
        assert_eq!(cfg.anthropic.base_url, "");
    }

    #[test]
    fn openai_thinking_params_are_parsed_and_validated() {
        let d = temp_dir("openai-thinking");
        fs::write(
            d.join("lex-code.toml"),
            "provider = \"openai\"\n[openai]\nbase_url = \"http://127.0.0.1:7\"\nmodel = \"m\"\nthinking = \"enabled\"\nreasoning_effort = \"high\"\n",
        )
        .unwrap();
        let cfg = Config::load(&d).unwrap();
        assert_eq!(cfg.openai.thinking.as_deref(), Some("enabled"));
        assert_eq!(cfg.openai.reasoning_effort.as_deref(), Some("high"));

        let d2 = temp_dir("openai-thinking-bad");
        fs::write(
            d2.join("lex-code.toml"),
            "provider = \"openai\"\n[openai]\nbase_url = \"http://127.0.0.1:7\"\nmodel = \"m\"\nthinking = \"always\"\n",
        )
        .unwrap();
        let err = Config::load(&d2).unwrap_err();
        assert!(err.to_string().contains("thinking"), "实际: {err}");

        let d3 = temp_dir("openai-effort-bad");
        fs::write(
            d3.join("lex-code.toml"),
            "provider = \"openai\"\n[openai]\nbase_url = \"http://127.0.0.1:7\"\nmodel = \"m\"\nreasoning_effort = \"ultra\"\n",
        )
        .unwrap();
        let err = Config::load(&d3).unwrap_err();
        assert!(err.to_string().contains("reasoning_effort"), "实际: {err}");
    }

    #[test]
    fn env_overrides_openai_base_url() {
        let d = temp_dir("openai-env");
        fs::write(
            d.join("lex-code.toml"),
            "provider = \"openai\"\n[openai]\nbase_url = \"http://127.0.0.1:7\"\nmodel = \"m\"\n",
        )
        .unwrap();
        unsafe { std::env::set_var("LEX_TEST_OPENAI_BASE_URL", "http://127.0.0.1:6") };
        let cfg = Config::load_with_env_prefix(&d, "LEX_TEST").unwrap();
        assert_eq!(cfg.openai.base_url, "http://127.0.0.1:6");
    }

    #[test]
    fn unknown_provider_is_rejected() {
        let d = temp_dir("unknown-provider");
        fs::write(d.join("lex-code.toml"), "provider = \"gemini\"\n").unwrap();
        let err = Config::load(&d).unwrap_err();
        assert!(err.to_string().contains("provider"), "实际: {err}");
    }

    #[test]
    fn api_key_env_per_provider() {
        assert_eq!(Config::api_key_env("anthropic"), "LEX_ANTHROPIC_API_KEY");
        assert_eq!(Config::api_key_env("openai"), "LEX_OPENAI_API_KEY");
    }
}
