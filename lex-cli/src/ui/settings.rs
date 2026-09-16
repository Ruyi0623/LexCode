use lex_core::agent::AgentLoop;
use lex_core::config::Config;
use lex_core::context::compress;

use crate::ui::theme;

// ---------- 数据快照 ----------

/// 会话运行时快照来源(interactive_session 组装)
pub struct SettingsRuntime {
    pub compress_attempted: bool,
    pub token_estimate: u64,
    pub forbidden_count: usize,
    pub confirm_count: usize,
    pub auto_count: usize,
    pub log_level: String,
    pub project_type: String,
    pub cwd: String,
}

pub struct SettingsView {
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub max_tokens: String,
    pub openai_thinking: Option<String>,
    pub openai_effort: Option<String>,
    pub api_key_source: String,
    pub forbidden_count: usize,
    pub confirm_count: usize,
    pub auto_count: usize,
    pub context_limit: u32,
    pub context_enabled: bool,
    pub compress_attempted: bool,
    pub token_estimate: u64,
    pub log_level: String,
    pub version: String,
    pub project_type: String,
    pub cwd: String,
}

pub const MODULE_TITLES: [&str; 4] =
    ["模型与 Provider", "权限与安全", "上下文与压缩", "外观·日志·关于"];

const PENDING_LINE: &str = "⚙ 该模块的编辑能力待实现(当前只读展示)";

impl SettingsView {
    pub fn from(config: &Config, runtime: SettingsRuntime) -> Self {
        let (model, base_url, max_tokens, openai_thinking, openai_effort, api_key_source) =
            if config.provider == "openai" {
                (
                    config.openai.model.clone(),
                    config.openai.base_url.clone(),
                    match config.openai.max_tokens {
                        Some(t) => t.to_string(),
                        None => "未设置(服务端默认)".into(),
                    },
                    config.openai.thinking.clone(),
                    config.openai.reasoning_effort.clone(),
                    "环境变量 LEX_OPENAI_API_KEY".to_string(),
                )
            } else {
                (
                    config.anthropic.model.clone(),
                    config.anthropic.base_url.clone(),
                    config.anthropic.max_tokens.to_string(),
                    None,
                    None,
                    "环境变量 LEX_ANTHROPIC_API_KEY".to_string(),
                )
            };
        SettingsView {
            provider: config.provider.clone(),
            model,
            base_url,
            max_tokens,
            openai_thinking,
            openai_effort,
            api_key_source,
            forbidden_count: runtime.forbidden_count,
            confirm_count: runtime.confirm_count,
            auto_count: runtime.auto_count,
            context_limit: config.context.limit,
            context_enabled: config.context.enabled,
            compress_attempted: runtime.compress_attempted,
            token_estimate: runtime.token_estimate,
            log_level: runtime.log_level,
            version: env!("CARGO_PKG_VERSION").to_string(),
            project_type: runtime.project_type,
            cwd: runtime.cwd,
        }
    }
}

/// 日志级别优先级:LEX_LOG > RUST_LOG > warn(与 main.rs 初始化逻辑一致)
pub fn resolve_log_level(lex: Option<&str>, rust: Option<&str>) -> String {
    let non_empty = |s: &str| !s.trim().is_empty();
    match lex {
        Some(l) if non_empty(l) => l.to_string(),
        _ => match rust {
            Some(r) if non_empty(r) => r.to_string(),
            _ => "warn".into(),
        },
    }
}

/// 从 AgentLoop 读取会话运行时状态(纯字段组装)
pub fn make_runtime(agent: &AgentLoop, project_type: &str, cwd: &str, log_level: &str) -> SettingsRuntime {
    let (forbidden_count, confirm_count, auto_count) = agent.security.rule_counts();
    SettingsRuntime {
        compress_attempted: agent.compress_attempted,
        token_estimate: compress::estimate_tokens(&agent.history),
        forbidden_count,
        confirm_count,
        auto_count,
        log_level: log_level.to_string(),
        project_type: project_type.to_string(),
        cwd: cwd.to_string(),
    }
}

// ---------- 纯渲染(快照 → 行;颜色只取 theme.rs) ----------

fn field(label: &str, value: &str) -> String {
    format!("{}{}{}{value}", theme::DIM, label, theme::RESET)
}

pub fn render_list(v: &SettingsView, selected: usize) -> Vec<String> {
    let summaries = [
        format!("{} · {}", v.provider, v.model),
        format!("forbidden {}/confirm {}/auto {}", v.forbidden_count, v.confirm_count, v.auto_count),
        format!("limit {} · 压缩 {}", v.context_limit, if v.context_enabled { "开" } else { "关" }),
        format!("日志 {} · v{}", v.log_level, v.version),
    ];
    let mut lines = vec![format!("{}设置(只读快照){}", theme::ACCENT_BOLD, theme::RESET)];
    for (i, (title, summary)) in MODULE_TITLES.iter().zip(summaries).enumerate() {
        let marker = if i == selected { format!("{}>{}", theme::ACCENT, theme::RESET) } else { " ".to_string() };
        lines.push(format!("{marker} {}. {} —— {}", i + 1, title, summary));
    }
    lines.push(String::new());
    lines.push(format!("{}↑/↓ 或数字选择 · Enter 详情 · q/Esc 返回{}", theme::DIM, theme::RESET));
    lines
}

pub fn render_detail(v: &SettingsView, module: usize) -> Vec<String> {
    let title = MODULE_TITLES.get(module).copied().unwrap_or("<未知模块>");
    let mut lines = vec![
        format!("{}{title}{}", theme::ACCENT_BOLD, theme::RESET),
        String::new(),
    ];
    match module {
        0 => {
            lines.push(field("Provider:      ", &v.provider));
            lines.push(field("模型:          ", &v.model));
            lines.push(field("Base URL:      ", &v.base_url));
            lines.push(field("max_tokens:    ", &v.max_tokens));
            if let Some(t) = &v.openai_thinking {
                lines.push(field("thinking:      ", t));
            }
            if let Some(e) = &v.openai_effort {
                lines.push(field("reasoning_effort: ", e));
            }
            lines.push(field("API Key:       ", &v.api_key_source));
        }
        1 => {
            lines.push(field("三级权限:      ", "Forbidden(硬拦截)> Confirm(询问)> Auto(放行)"));
            lines.push(field(
                "规则条数:      ",
                &format!("forbidden {}/confirm {}/auto {}", v.forbidden_count, v.confirm_count, v.auto_count),
            ));
            lines.push(field("敏感文件外发拦截: ", "已启用(同轮启发式)"));
            lines.push(field("规则来源:      ", "内置默认 + lex-code.toml [security] 追加,不可移除内置项"));
        }
        2 => {
            lines.push(field("上下文 limit:  ", &v.context_limit.to_string()));
            lines.push(field("压缩开关:      ", if v.context_enabled { "开" } else { "关" }));
            lines.push(field("本会话已压缩:  ", if v.compress_attempted { "已触发" } else { "未触发" }));
            lines.push(field("历史 token 估算: ", &v.token_estimate.to_string()));
        }
        _ => {
            lines.push(field("版本:          ", &v.version));
            lines.push(field("项目类型:      ", &v.project_type));
            lines.push(field("工作目录:      ", &v.cwd));
            lines.push(field("日志级别:      ", &v.log_level));
            lines.push(field("主题:          ", "蓝色主题(定义于 ui/theme.rs,唯一取色处)"));
        }
    }
    lines.push(String::new());
    lines.push(format!("{}{PENDING_LINE}{}", theme::WARN, theme::RESET));
    lines.push(String::new());
    lines.push(format!("{}q/Esc 返回列表{}", theme::DIM, theme::RESET));
    lines
}

// ---------- 页面状态机(纯函数,单测覆盖) ----------

use crossterm::event::KeyCode;

#[derive(Debug, PartialEq)]
pub enum PageAction {
    None,
    EnterDetail(usize),
    ToList,
    Quit,
}

pub struct PageState {
    pub selected: usize,
    pub in_detail: bool,
}

impl PageState {
    pub fn new() -> Self {
        PageState { selected: 0, in_detail: false }
    }

    pub fn handle_key(&mut self, code: KeyCode) -> PageAction {
        if self.in_detail {
            return match code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    self.in_detail = false;
                    PageAction::ToList
                }
                _ => PageAction::None,
            };
        }
        match code {
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                PageAction::None
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(MODULE_TITLES.len() - 1);
                PageAction::None
            }
            KeyCode::Enter => {
                self.in_detail = true;
                PageAction::EnterDetail(self.selected)
            }
            KeyCode::Char(c @ '1'..='4') => {
                let idx = c as usize - '1' as usize;
                self.selected = idx;
                self.in_detail = true;
                PageAction::EnterDetail(idx)
            }
            KeyCode::Char('q') | KeyCode::Esc => PageAction::Quit,
            _ => PageAction::None,
        }
    }
}

impl Default for PageState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------- 命令分发 ----------

#[derive(Debug, PartialEq)]
pub enum SlashCommand {
    Settings,
    Unknown,
}

/// 已知斜杠命令表(新增命令在此登记;前缀唯一命中即执行)
const KNOWN_COMMANDS: &[&str] = &["/settings"];

/// 交互输入的斜杠命令识别;None = 普通文本(不进分发)。
/// 精确匹配或唯一前缀命中(如 /setting)均执行;歧义/未知前缀返回 Unknown。
pub fn parse_command(line: &str) -> Option<SlashCommand> {
    let t = line.trim();
    if !t.starts_with('/') {
        return None;
    }
    if t == "/" {
        // 空前缀匹配所有命令,视为歧义
        return Some(SlashCommand::Unknown);
    }
    let candidates: Vec<&str> = KNOWN_COMMANDS.iter().copied().filter(|c| c.starts_with(t)).collect();
    match candidates.as_slice() {
        ["/settings"] => Some(SlashCommand::Settings),
        _ => Some(SlashCommand::Unknown),
    }
}

// ---------- 终端集成 ----------

use crossterm::tty::IsTty;
use lex_core::error::{LexError, Result};

use crate::ui::input::{event_bus, RawGuard};

fn emit(lines: &[String]) {
    for l in lines {
        anstream::print!("{l}\r\n"); // raw mode 期间显式 \r\n
    }
    use std::io::Write;
    std::io::stdout().flush().ok();
}

fn repaint_list(v: &SettingsView, page: &PageState) {
    anstream::print!("{}", theme::CLEAR_SCREEN);
    emit(&render_list(v, page.selected));
}

/// 设置页主入口:TTY 全屏交互;非 TTY 降级为一次性线性摘要。
/// 返回后调用方负责清屏/重绘横幅。
pub async fn open_page(v: &SettingsView) -> Result<()> {
    let is_tty = std::io::stdin().is_tty() && std::io::stdout().is_tty();
    if !is_tty {
        // 非交互环境(管道/重定向):无需交互,线性打印即可;anstream 在非 TTY 自动剥离 ANSI
        for m in 0..MODULE_TITLES.len() {
            emit(&render_detail(v, m));
        }
        return Ok(());
    }
    let _raw = RawGuard::new().map_err(LexError::Io)?;
    let (_, rx) = event_bus();
    let mut page = PageState::new();
    repaint_list(v, &page);
    let mut rx = rx.lock().await;
    loop {
        let ev = rx.recv().await;
        let Some(ev) = ev else { return Ok(()) }; // 按键读线程终止:直接回 REPL
        let crossterm::event::Event::Key(key) = ev else { continue };
        if key.kind != crossterm::event::KeyEventKind::Press {
            continue; // Windows 会发 Release 事件
        }
        match page.handle_key(key.code) {
            PageAction::None => {}
            PageAction::EnterDetail(_) => {
                anstream::print!("{}", theme::CLEAR_SCREEN);
                emit(&render_detail(v, page.selected));
            }
            PageAction::ToList => repaint_list(v, &page),
            PageAction::Quit => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lex_core::config::Config;

    fn runtime() -> SettingsRuntime {
        SettingsRuntime {
            compress_attempted: true,
            token_estimate: 1_234,
            forbidden_count: 3,
            confirm_count: 1,
            auto_count: 2,
            log_level: "warn".into(),
            project_type: "Rust".into(),
            cwd: "D:/demo".into(),
        }
    }

    fn anthropic_cfg() -> Config {
        let d = std::env::temp_dir().join("lex-settings-test-a");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("lex-code.toml"),
            "[anthropic]\nbase_url = \"https://api.test\"\nmodel = \"claude-x\"\n",
        )
        .unwrap();
        Config::load(&d).unwrap_or_else(|_| panic!("配置解析失败"))
    }

    fn openai_cfg() -> Config {
        let d = std::env::temp_dir().join("lex-settings-test-o");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("lex-code.toml"),
            "provider = \"openai\"\n[openai]\nbase_url = \"https://api.test\"\nmodel = \"deepseek-x\"\nthinking = \"enabled\"\nreasoning_effort = \"high\"\n",
        )
        .unwrap();
        Config::load(&d).unwrap_or_else(|_| panic!("配置解析失败"))
    }

    #[test]
    fn view_maps_anthropic_config() {
        let v = SettingsView::from(&anthropic_cfg(), runtime());
        assert_eq!(v.provider, "anthropic");
        assert_eq!(v.model, "claude-x");
        assert_eq!(v.max_tokens, "8192");
        assert_eq!(v.openai_thinking, None, "anthropic 时不得显示 openai 思考参数");
        assert!(v.api_key_source.contains("LEX_ANTHROPIC_API_KEY"));
    }

    #[test]
    fn view_maps_openai_config_and_thinking() {
        let v = SettingsView::from(&openai_cfg(), runtime());
        assert_eq!(v.provider, "openai");
        assert_eq!(v.model, "deepseek-x");
        assert_eq!(v.max_tokens, "未设置(服务端默认)");
        assert_eq!(v.openai_thinking.as_deref(), Some("enabled"));
        assert_eq!(v.openai_effort.as_deref(), Some("high"));
        assert!(v.api_key_source.contains("LEX_OPENAI_API_KEY"));
    }

    #[test]
    fn resolve_log_level_precedence() {
        assert_eq!(resolve_log_level(Some("debug"), Some("trace")), "debug");
        assert_eq!(resolve_log_level(None, Some("trace")), "trace");
        assert_eq!(resolve_log_level(Some("  "), Some("trace")), "trace");
        assert_eq!(resolve_log_level(None, None), "warn");
    }

    #[test]
    fn runtime_fixture_holds_session_state() {
        let rt = runtime();
        assert!(rt.compress_attempted);
        assert_eq!(rt.token_estimate, 1_234);
        assert_eq!(rt.forbidden_count, 3);
    }

    #[test]
    fn list_renders_four_modules_with_selection() {
        let v = SettingsView::from(&anthropic_cfg(), runtime());
        let lines = render_list(&v, 1);
        let joined = lines.join("\n");
        for title in MODULE_TITLES {
            assert!(joined.contains(title), "列表应含模块标题: {title}");
        }
        assert!(lines.iter().any(|l| l.contains('>') && l.contains(MODULE_TITLES[1])), "选中行应有 > 标记");
        assert!(joined.contains("返回"), "应提示退出键");
    }

    #[test]
    fn detail_renders_fields_and_pending_marker() {
        let v = SettingsView::from(&anthropic_cfg(), runtime());
        let lines = render_detail(&v, 0);
        let joined = lines.join("\n");
        assert!(joined.contains("claude-x"), "详情应含模型名");
        assert!(joined.contains("https://api.test"), "详情应含 base_url");
        assert!(joined.contains("LEX_ANTHROPIC_API_KEY"));
        assert!(joined.contains("待实现"), "占位模块应有待实现标记");
        let sec = render_detail(&v, 1).join("\n");
        assert!(sec.chars().any(|c| c == '3'), "详情应展示规则数");
        let ctx = render_detail(&v, 2).join("\n");
        assert!(ctx.contains("64000") || ctx.contains("64,000"), "详情应含上下文 limit");
        assert!(ctx.contains("已触发"), "compress_attempted=true 应展示");
    }

    #[test]
    fn parse_command_matches_settings_only() {
        use super::{parse_command, SlashCommand};
        assert!(matches!(parse_command("/settings"), Some(SlashCommand::Settings)));
        assert!(matches!(parse_command("/Settings"), Some(SlashCommand::Unknown)));
        assert!(matches!(parse_command("/foo"), Some(SlashCommand::Unknown)));
        assert!(matches!(parse_command(" /settings  "), Some(SlashCommand::Settings)), "首尾空白容忍");
        assert_eq!(parse_command("你好"), None, "普通文本不误判");
        assert_eq!(parse_command(""), None);
        assert_eq!(parse_command("settings"), None, "缺斜杠不算命令");
    }

    #[test]
    fn parse_command_accepts_unique_prefix() {
        use super::{parse_command, SlashCommand};
        // 唯一前缀命中:少打几个字母也能进(如 /setting)
        assert!(matches!(parse_command("/setting"), Some(SlashCommand::Settings)));
        assert!(matches!(parse_command("/set"), Some(SlashCommand::Settings)));
        assert!(matches!(parse_command("/s"), Some(SlashCommand::Settings)));
        // 歧义/无效前缀不算命中
        assert!(matches!(parse_command("/"), Some(SlashCommand::Unknown)), "空前缀视为歧义");
        assert!(matches!(parse_command("/settingsx"), Some(SlashCommand::Unknown)), "比已知命令更长且不相等不是前缀");
    }

    mod page {
        use super::super::{PageAction, PageState};
        use crossterm::event::KeyCode;

        #[test]
        fn selection_moves_within_bounds() {
            let mut s = PageState::new();
            assert_eq!(s.handle_key(KeyCode::Up), PageAction::None);
            assert_eq!(s.selected, 0, "上移不能越过 0");
            s.handle_key(KeyCode::Down);
            s.handle_key(KeyCode::Down);
            s.handle_key(KeyCode::Down);
            s.handle_key(KeyCode::Down); // 已在 3
            assert_eq!(s.selected, 3, "下移不能越过最后模块");
        }

        #[test]
        fn number_keys_jump_to_module() {
            let mut s = PageState::new();
            assert_eq!(s.handle_key(KeyCode::Char('3')), PageAction::EnterDetail(2));
            assert_eq!(s.selected, 2);
            assert_eq!(s.handle_key(KeyCode::Char('9')), PageAction::None, "越界数字忽略");
        }

        #[test]
        fn enter_and_escape_navigate() {
            let mut s = PageState::new();
            assert_eq!(s.handle_key(KeyCode::Enter), PageAction::EnterDetail(0));
            s.in_detail = true;
            assert_eq!(s.handle_key(KeyCode::Esc), PageAction::ToList);
            assert!(!s.in_detail);
            assert_eq!(s.handle_key(KeyCode::Esc), PageAction::Quit);
            let mut s2 = PageState::new();
            assert_eq!(s2.handle_key(KeyCode::Char('q')), PageAction::Quit);
        }
    }
}
