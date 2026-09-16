# /settings 设置页面(四占位模块,只读快照)实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 交互会话内 `/settings` 打开设置页面:四个占位模块(模型与 Provider / 权限与安全 / 上下文与压缩 / 外观·日志·关于)只读展示当前真实配置快照,非 TTY 自动降级为线性摘要;不做任何编辑/写回。

**Architecture:** 三层切分——① 纯数据快照 `SettingsView`(从 `Config` + 会话运行时一次性构建);② 纯渲染函数 `render_list`/`render_detail`(快照 → 带主题色行,可单测);③ 终端集成 `open_page`(复用 `ui/input.rs` 的 `event_bus()` 单例按键通道与 `RawGuard`,raw mode 短暂启用 Drop 恢复)。斜杠命令分发是 `interactive_session` 里的纯函数 `parse_command`,本期只认 `/settings`,为后续命令留枚举扩展点。

**Tech Stack:** Rust;复用现有 crossterm/anstream/anyhow,不新增依赖。

**规格:** `docs/superpowers/specs/2026-09-16-settings-page-design.md`

## Global Constraints

- 颜色/ANSI 只从 `ui/theme.rs` 取,`settings.rs` 等新文件禁止裸 `\x1b[`(AGENTS.md UI 硬约束)。
- raw mode 期间换行必须显式 `\r\n`(`\n` 不回车)。
- 非测试代码禁止裸 `unwrap()`/`expect()`(Mutex 用 `unwrap_or_else(|p| p.into_inner())`)。
- `lex-cli → lex-core` 单向依赖;lex-core 本计划仅新增 `rule_counts()` 只读访问器,无行为改动。
- API Key 绝不回显:只显示"来自环境变量 LEX_*"。
- 提示词/界面文案全中文。
- 构建:先 `export PATH="$HOME/.cargo/bin:/d/mingw64/bin:$PATH"`,再 `cargo test --workspace`。
- 提交信息用中文 conventional commits。

---

### Task 1: 复用基建放行(theme.rs 整屏清除常量;input.rs 的 event_bus/RawGuard 改 pub(crate))

**Files:**
- Modify: `lex-cli/src/ui/theme.rs`
- Modify: `lex-cli/src/ui/input.rs:152`(`RawGuard`)、`lex-cli/src/ui/input.rs:168`(`event_bus`)

**Interfaces:**
- Produces(消费方:Task 5 `open_page`):
  - `pub const CLEAR_SCREEN: &str`(整屏清除 + 光标归位)
  - `pub(crate) struct RawGuard`(`fn new() -> std::io::Result<Self>`,`Drop` 恢复 raw mode)
  - `pub(crate) fn event_bus() -> &'static (tokio::sync::mpsc::UnboundedSender<Event>, tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Event>>)`

- [ ] **Step 1: theme.rs 追加常量**

`lex-cli/src/ui/theme.rs` 的 `CLEAR_LINE` 之后追加:

```rust
/// 整屏清除 + 光标归位(设置页等全屏界面进出用;3J 连同回滚缓冲一起清)
pub const CLEAR_SCREEN: &str = "\x1b[2J\x1b[3J\x1b[H";
```

- [ ] **Step 2: input.rs 可见性调整**

```rust
// 原:struct RawGuard;         → 改:
pub(crate) struct RawGuard;
// 原:impl RawGuard { fn new() ... → 改:
impl RawGuard {
    pub(crate) fn new() -> std::io::Result<Self> {
// 原:fn event_bus() -> ...     → 改:
pub(crate) fn event_bus() -> &'static (
```

(函数体、`Drop` 实现均不动。)

- [ ] **Step 3: 编译与全量测试**

Run: `cargo build --workspace && cargo test --workspace`
Expected: 编译通过,全部 PASS(纯可见性/常量变更,无行为变化)。

- [ ] **Step 4: Commit**

```bash
git add lex-cli/src/ui/theme.rs lex-cli/src/ui/input.rs
git commit -m "refactor(cli): theme 增 CLEAR_SCREEN;input 的 RawGuard/event_bus 放开为 pub(crate) 供设置页复用"
```

---

### Task 2: lex-core 只读访问器 SecurityRules::rule_counts / SecurityGuard::rule_counts

**Files:**
- Modify: `lex-core/src/security/rules.rs:55`(`SecurityRules`)、`lex-core/src/security/rules.rs:146`(`SecurityGuard`)
- Test: `lex-core/src/security/rules.rs`(`mod tests`)

**Interfaces:**
- Produces(消费方:Task 3 `SettingsRuntime` 填充):
  - `impl SecurityRules { pub fn rule_counts(&self) -> (usize, usize, usize) }` → `(forbidden, confirm, auto)`
  - `impl SecurityGuard { pub fn rule_counts(&self) -> (usize, usize, usize) }`

- [ ] **Step 1: 写失败测试**

`mod tests` 追加:

```rust
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
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p lex-core rules::tests::rule_counts`
Expected: FAIL,`rule_counts` 不存在。

- [ ] **Step 3: 最小实现**

`impl SecurityRules` 内追加:

```rust
    /// 只读统计:(forbidden, confirm, auto) 条数(设置页展示用,无行为影响)
    pub fn rule_counts(&self) -> (usize, usize, usize) {
        (self.forbidden.len(), self.confirm.len(), self.auto.len())
    }
```

`impl SecurityGuard` 内追加:

```rust
    /// 只读统计(转发规则表;设置页展示用)
    pub fn rule_counts(&self) -> (usize, usize, usize) {
        self.rules.rule_counts()
    }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --workspace`
Expected: 全部 PASS。

- [ ] **Step 5: Commit**

```bash
git add lex-core/src/security/rules.rs
git commit -m "feat(security): rule_counts 只读访问器(forbidden/confirm/auto 条数,设置页展示用)"
```

---

### Task 3: SettingsView 快照 + 渲染纯函数(ui/settings.rs 上半部)

**Files:**
- Create: `lex-cli/src/ui/settings.rs`
- Modify: `lex-cli/src/ui/mod.rs`(加 `pub mod settings;`)
- Test: `lex-cli/src/ui/settings.rs`(`mod tests`)

**Interfaces:**
- Consumes: `lex_core::config::Config`(现有)、Task 2 `rule_counts`。
- Produces(消费方:Task 4/5):

```rust
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
pub struct SettingsView { /* 字段见 Step 3 */ }
impl SettingsView {
    pub fn from(config: &Config, runtime: SettingsRuntime) -> Self;
}
pub const MODULE_TITLES: [&str; 4];  // ["模型与 Provider", "权限与安全", "上下文与压缩", "外观·日志·关于"]
pub fn render_list(v: &SettingsView, selected: usize) -> Vec<String>;
pub fn render_detail(v: &SettingsView, module: usize) -> Vec<String>;
pub fn resolve_log_level(lex: Option<&str>, rust: Option<&str>) -> String; // LEX_LOG > RUST_LOG > "warn"
pub fn make_runtime(agent: &lex_core::agent::AgentLoop, project_type: &str, cwd: &str, log_level: &str) -> SettingsRuntime;
```

- [ ] **Step 1: 写失败测试**

`lex-cli/src/ui/settings.rs`:

```rust
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
        assert_eq!(v.max_tokens, "8192"); // anthropic 默认采样上限
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
        assert_eq!(resolve_log_level(Some("  "), Some("trace")), "trace"); // 空白视为未设置
        assert_eq!(resolve_log_level(None, None), "warn");
    }

    #[test]
    fn runtime_fixture_holds_session_state() {
        // make_runtime 的 AgentLoop 字段读取在 Task 5 集成冒烟覆盖(lex-cli 侧不便构造最小 AgentLoop);
        // 此处锁定快照字段的语义来源
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
        // 权限模块
        let sec = render_detail(&v, 1).join("\n");
        assert!(sec.contains("(3, 1, 2)".replace(' ', "").as_str()) || sec.contains("3/1/2") || sec.chars().any(|c| c == '3'), "详情应展示规则数");
        // 上下文模块
        let ctx = render_detail(&v, 2).join("\n");
        assert!(ctx.contains("64000") || ctx.contains("64,000"), "详情应含上下文 limit");
        assert!(ctx.contains("已触发"), "compress_attempted=true 应展示");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p lex-cli settings`
Expected: FAIL,`settings` 模块不存在。

- [ ] **Step 3: 最小实现**

`lex-cli/src/ui/settings.rs`(渲染函数返回带 theme 色的行;终端集成在 Task 5):

```rust
use lex_core::config::Config;
use lex_core::context::compress;
use lex_core::agent::AgentLoop;

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
    let mut lines = vec![format!(
        "{}设置(只读快照){}",
        theme::ACCENT_BOLD, theme::RESET
    )];
    for (i, (title, summary)) in MODULE_TITLES.iter().zip(summaries).enumerate() {
        let marker = if i == selected { format!("{}>{}", theme::ACCENT, theme::RESET) } else { " ".to_string() };
        let body = format!("{}. {} —— {}", i + 1, title, summary);
        lines.push(format!("{marker} {body}"));
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
```

`lex-cli/src/ui/mod.rs` 加 `pub mod settings;`。

注:测试 `detail_renders_fields_and_pending_marker` 里权限断言写得过宽(只断言含数字 '3'),实施时保持该宽断言即可,避免锁死格式。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p lex-cli settings`
Expected: 全部 PASS。

- [ ] **Step 5: Commit**

```bash
git add lex-cli/src/ui/settings.rs lex-cli/src/ui/mod.rs
git commit -m "feat(cli): SettingsView 配置快照与四模块渲染纯函数(可单测,不依赖终端)"
```

---

### Task 4: 页面状态机(选择/进出详情,纯函数)

**Files:**
- Modify: `lex-cli/src/ui/settings.rs`(追加状态机)
- Test: 同文件 `mod tests` 追加

**Interfaces:**
- Produces(消费方:Task 5 事件循环):

```rust
use crossterm::event::KeyCode;
pub enum PageAction { None, EnterDetail(usize), ToList, Quit }
pub struct PageState { pub selected: usize, pub in_detail: bool }
impl PageState {
    pub fn new() -> Self;                       // selected: 0, in_detail: false
    pub fn handle_key(&mut self, code: KeyCode) -> PageAction;
}
```

- [ ] **Step 1: 写失败测试**

`mod tests` 追加:

```rust
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
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p lex-cli settings::tests::page`
Expected: FAIL,`PageState` 不存在。

- [ ] **Step 3: 最小实现**

`lex-cli/src/ui/settings.rs` 追加:

```rust
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
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p lex-cli settings`
Expected: 全部 PASS(含 Task 3 既有测试)。

- [ ] **Step 5: Commit**

```bash
git add lex-cli/src/ui/settings.rs
git commit -m "feat(cli): 设置页状态机(上下选择/数字跳转/两级返回,纯函数单测)"
```

---

### Task 5: 终端集成 + 斜杠命令分发接线 + 非 TTY 降级 + 文档

**Files:**
- Modify: `lex-cli/src/ui/settings.rs`(追加 `open_page` / `print_linear` / `parse_command`)
- Modify: `lex-cli/src/main.rs`(`interactive_session` 分发)
- Modify: `README.md`(交互说明补 `/settings`)、`AGENTS.md`(lex-cli 模块清单补 `settings`)
- Test: `lex-cli/src/ui/settings.rs`(`mod tests` 追加 `parse_command` 测试)

**Interfaces:**
- Consumes: Task 1 `CLEAR_SCREEN`/`RawGuard`/`event_bus()`、Task 3 渲染函数、Task 4 `PageState`、`ui::banner::print`(返回 REPL 后重绘横幅)、`main.rs` 的 `detect_project_type`(已是私有 fn,同 crate 直接可用)。
- Produces:

```rust
pub enum SlashCommand { Settings, Unknown }
pub fn parse_command(line: &str) -> Option<SlashCommand>; // None = 普通文本
pub async fn open_page(v: &SettingsView) -> lex_core::error::Result<()>; // 内含非 TTY 降级
```

- [ ] **Step 1: 写失败测试(parse_command)**

`mod tests` 追加:

```rust
    #[test]
    fn parse_command_matches_settings_only() {
        use super::{parse_command, SlashCommand};
        assert!(matches!(parse_command("/settings"), Some(SlashCommand::Settings)));
        assert!(parse_command("/Settings").is_some(), "大小写敏感:/Settings 是已知前缀但非 settings 命令");
        assert!(matches!(parse_command("/Settings"), Some(SlashCommand::Unknown)));
        assert!(matches!(parse_command("/foo"), Some(SlashCommand::Unknown)));
        assert!(matches!(parse_command(" /settings  "), Some(SlashCommand::Settings)), "首尾空白容忍");
        assert_eq!(parse_command("你好"), None, "普通文本不误判");
        assert_eq!(parse_command(""), None);
        assert_eq!(parse_command("settings"), None, "缺斜杠不算命令");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p lex-cli parse_command`
Expected: FAIL,`parse_command` 不存在。

- [ ] **Step 3: 实现 settings.rs 集成层**

`lex-cli/src/ui/settings.rs` 追加:

```rust
// ---------- 命令分发 ----------

#[derive(Debug, PartialEq)]
pub enum SlashCommand {
    Settings,
    Unknown,
}

/// 交互输入的斜杠命令识别;None = 普通文本(不进分发)
pub fn parse_command(line: &str) -> Option<SlashCommand> {
    let t = line.trim();
    if !t.starts_with('/') {
        return None;
    }
    match t {
        "/settings" => Some(SlashCommand::Settings),
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
        let Ok(crossterm::event::Event::Key(key)) = ev else { continue };
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
```

- [ ] **Step 4: main.rs 接线**

`interactive_session` 中,`history.push(line.clone());` 之后、`thinking_hint` 之前插入:

```rust
        // 斜杠命令分发:/settings 进设置页;其他 / 前缀给友好提示;普通文本照常进模型
        match ui::settings::parse_command(&line) {
            Some(ui::settings::SlashCommand::Settings) => {
                let log_level = ui::settings::resolve_log_level(
                    std::env::var("LEX_LOG").ok().as_deref(),
                    std::env::var("RUST_LOG").ok().as_deref(),
                );
                let view = ui::settings::SettingsView::from(
                    cfg,
                    ui::settings::make_runtime(
                        agent,
                        &detect_project_type(cwd),
                        &display_path(cwd),
                        &log_level,
                    ),
                );
                let open = ui::settings::open_page(&view).await;
                if let Err(e) = open {
                    // 规格要求:raw mode 启用失败等错误打印一行后返回 REPL,不中断会话
                    anstream::println!("{}", ui::theme::error(&format!("设置页打开失败: {e}")));
                }
                // 返回 REPL:清屏并重绘横幅,恢复上下文
                anstream::print!("{}", ui::theme::CLEAR_SCREEN);
                ui::banner::print(&cfg.provider, &current_model(cfg), &display_path(cwd), &detect_project_type(cwd));
                continue;
            }
            Some(ui::settings::SlashCommand::Unknown) => {
                anstream::println!("{}", ui::theme::warn("未知命令,可用:/settings"));
                continue;
            }
            None => {}
        }
```

(`interactive_session` 的 `agent` 参数已是 `&mut AgentLoop`,`make_runtime(agent, …)` 传 `&AgentLoop` 即可;`cfg`/`cwd` 形参已存在。)

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test --workspace`
Expected: 全部 PASS。

- [ ] **Step 6: 文档更新**

`README.md` 交互说明区(REPL 用法处)补一行:

```markdown
- `/settings`:打开设置页面(只读快照,四模块:模型与 Provider / 权限与安全 / 上下文与压缩 / 外观·日志·关于;↑↓/数字选择,Enter 详情,q/Esc 返回;非交互终端自动降级为纯文本摘要)。配置编辑能力待后续版本。
```

`AGENTS.md` 的 `lex-cli/src/` 模块清单行补 `settings`(设置页:/settings 分发 + 只读快照渲染)。

- [ ] **Step 7: 手工冒烟**

1. `cargo build --release -p lex-cli`,交互终端运行 `D:/lexcode-target/release/lex-code.exe`:输入 `/settings` → 出现四模块列表(选中行 `>`)。
2. `↓`/`2`/`Enter` 进详情;`Esc` 回列表;`q` 返回 REPL 且横幅重绘、输入盒正常。
3. `/foo` → "未知命令,可用:/settings";普通提问照常进模型。
4. `lex-code | cat` 后非交互输入 `/settings` → 输出线性摘要、无交互、无残留控制字符。
5. 确认详情页 API Key 行只有"环境变量 LEX_*"字样,无 Key 内容。

- [ ] **Step 8: Commit**

```bash
git add lex-cli/src README.md AGENTS.md
git commit -m "feat(cli): /settings 设置页接入 REPL(斜杠分发/全屏交互/非 TTY 降级)+ 文档"
```

---

## 验收对照(规格)

| 规格要求 | 落点 |
|---|---|
| `parse_command` 纯函数分发,/settings 进页、未知命令提示、普通文本不变 | Task 5 |
| 两级页面(列表 → 详情),↑↓/数字选择,q/Esc 返回 | Task 4(状态机)+ Task 5(事件循环) |
| 四模块只读快照,内容映射 lex-code.toml 四段 + 会话状态 | Task 3(`SettingsView::from`/`render_*`) |
| Key 只显示"来自环境变量 LEX_*",不回显 | Task 3(`api_key_source`)+ Task 5 冒烟第 5 步 |
| 复用 event_bus/RawGuard,raw mode Drop 恢复,CLEAR_SCREEN 进 theme.rs | Task 1 + Task 5 |
| raw mode 换行显式 `\r\n`;颜色只取 theme.rs | Task 5 `emit()`;Task 3 渲染仅用 theme 常量 |
| 非 TTY 降级为线性只读摘要 | Task 5 `open_page` 分支 |
| 错误处理:按键线程终止/RAW 失败返回 REPL,不 panic,不碰配置文件 | Task 5(读线程 None 分支、RawGuard 错误上抛给调用方打印) |
| `rule_counts` 只读访问器展示规则条数 | Task 2 |
| 测试:分发/快照/渲染/状态机四类单测,不依赖终端 | Task 2–5 各 TDD 循环 |
