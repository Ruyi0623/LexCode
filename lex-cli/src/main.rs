mod confirm;
mod tui;
mod ui;

use anyhow::{Context, Result};
use clap::Parser;
use lex_core::agent::{AgentLoop, ToolResultHook};
use lex_core::config::{resolve_api_key, Config};
use lex_core::prompt::{load_system_prompt, render_template, resolve_system_prompt_path};
use lex_core::context::agents_md::{assemble_system_prompt, load_agents_md};
use lex_core::provider::cache::{CacheStrategy, ImplicitPrefixCacheStrategy};
use lex_core::provider::anthropic::AnthropicProvider;
use lex_core::provider::openai_compat::OpenAiCompatProvider;
use lex_core::provider::openai_types::OpenAiParams;
use lex_core::provider::Provider;
use lex_core::security::{SecurityGuard, SecurityRules};
use lex_core::tools::bash_exec::BashExec;
use lex_core::tools::file_edit::FileEdit;
use lex_core::tools::file_read::FileRead;
use lex_core::tools::grep_search::GrepSearch;
use lex_core::tools::todo_write::TodoWrite;
use lex_core::tools::{ShellCommand, ToolContext, ToolRegistry};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use crossterm::tty::IsTty;

#[derive(Parser)]
#[command(name = "lex-code", version, about = "Lex Code — 终端编程 agent")]
struct Cli {
    /// 任务描述(留空进入交互模式)
    task: Vec<String>,
    /// 工作目录
    #[arg(short = 'C', default_value = ".")]
    cwd: PathBuf,
    /// 强制纯文本流模式(跳过 TUI)
    #[arg(long)]
    plain: bool,
}

/// 是否进入 TUI:--plain 强制纯文本;stdin/stdout 任一非 TTY(管道/重定向)自动降级。
/// 与既有降级路径(`ui/input.rs`、`ui/settings.rs`)判定口径一致:两侧都必须是 TTY。
fn use_tui(forced_plain: bool, interactive_tty: bool) -> bool {
    !forced_plain && interactive_tty
}

/// 当前会话是否具备交互式终端(stdin 与 stdout 都是 TTY)
fn interactive_tty() -> bool {
    std::io::stdin().is_tty() && std::io::stdout().is_tty()
}

pub(crate) fn detect_project_type(cwd: &Path) -> String {
    if cwd.join("Cargo.toml").is_file() {
        "Rust".into()
    } else if cwd.join("package.json").is_file() {
        "Node.js".into()
    } else if cwd.join("pyproject.toml").is_file() || cwd.join("requirements.txt").is_file() {
        "Python".into()
    } else if cwd.join("go.mod").is_file() {
        "Go".into()
    } else {
        "未知".into()
    }
}

fn os_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

fn build_system_prompt(cfg: &Config, cwd: &Path) -> Result<String> {
    let path = resolve_system_prompt_path(cwd, cfg.system_prompt_path.as_deref())
        .context("找不到系统提示词文件(assets/coding-agent-system-prompt.md),可用配置 system_prompt_path 指定")?;
    let template = load_system_prompt(&path)?;
    let mut vars: BTreeMap<&str, String> = BTreeMap::new();
    vars.insert("CWD", cwd.display().to_string());
    vars.insert("OS", os_name().to_string());
    vars.insert("PROJECT_TYPE", detect_project_type(cwd));
    vars.insert("TOOL_TODO", "todo_write".to_string());
    Ok(render_template(&template, &vars))
}

/// 从配置构建节流 Provider(/settings 热切换复用同一构建路径)
fn build_throttled_provider(
    cfg: &Config,
) -> Result<lex_core::provider::throttle::ThrottledProvider> {
    let api_key = resolve_api_key(&cfg.provider)?;
    // 切换 provider 只改配置,不改 Agent Loop:两者实现同一个 Provider trait
    let inner: std::sync::Arc<dyn Provider> = match cfg.provider.as_str() {
        "openai" => std::sync::Arc::new(OpenAiCompatProvider::with_defaults(
            cfg.openai.base_url.clone(),
            cfg.openai.model.clone(),
            OpenAiParams {
                max_tokens: cfg.openai.max_tokens,
                thinking: cfg.openai.thinking.clone(),
                reasoning_effort: cfg.openai.reasoning_effort.clone(),
                user_id: cfg.openai.user_id.clone(),
            },
            api_key,
        )?),
        _ => std::sync::Arc::new(AnthropicProvider::with_defaults(
            cfg.anthropic.base_url.clone(),
            cfg.anthropic.model.clone(),
            cfg.anthropic.max_tokens,
            api_key,
        )?),
    };
    // 主循环与所有子 agent 共享同一并发节流(默认 3 条在途流)
    Ok(lex_core::provider::throttle::ThrottledProvider::new(inner, lex_core::provider::throttle::MAX_CONCURRENT_STREAMS))
}

fn build_loop(
    cfg: &Config,
    cwd: PathBuf,
    handler: std::sync::Arc<dyn lex_core::security::PermissionHandler>,
    todos: std::sync::Arc<std::sync::Mutex<Vec<lex_core::tools::Todo>>>,
    on_tool_result: Option<ToolResultHook>,
    on_child_event: Option<lex_core::agent::ChildEventHook>,
) -> Result<AgentLoop> {
    let throttled = build_throttled_provider(cfg)?;

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FileRead));
    registry.register(Box::new(FileEdit));
    registry.register(Box::new(BashExec));
    registry.register(Box::new(GrepSearch));
    registry.register(Box::new(TodoWrite));
    registry.register(Box::new(lex_core::tools::spawn_subagent::SpawnSubagent::new(cfg.agent.max_children_per_turn)));
    let registry = std::sync::Arc::new(registry);

    let shell: Option<ShellCommand> = cfg.shell.command.clone().map(|command| ShellCommand {
        command,
        args: cfg.shell.args.clone().unwrap_or_default(),
    });

    let base_system = build_system_prompt(cfg, &cwd)?;
    // AGENTS.md 一次性注入到系统提示词之后,进入缓存前缀(此后逐字节不变)
    let agents_md = load_agents_md(&cwd);
    let system = assemble_system_prompt(&base_system, agents_md.as_deref());
    if agents_md.is_some() {
        anstream::println!("{}已加载项目指引 AGENTS.md{}", ui::theme::DIM, ui::theme::RESET);
    }
    // 隐式前缀缓存策略:两个 provider 通用(前缀一致性校验 + 命中率遥测)
    let cache_strategy: std::sync::Arc<dyn CacheStrategy> =
        std::sync::Arc::new(ImplicitPrefixCacheStrategy::new());
    // 上下文压缩上限:主循环与其派生的子 agent 用同一个值(enabled=false 时不压缩)
    let context_limit = cfg.context.enabled.then_some(cfg.context.limit);

    let rules = SecurityRules::build(&cfg.security)?;
    // 主循环与整棵子 agent 树共用同一 handler 槽位:TUI 运行期 set() 换弹层时子 agent 同步生效
    let shared_handler = lex_core::agent::SharedHandler::new(handler.clone());
    // depth=0 派生器:与主循环共享节流 provider / 注册表 / 权限配置,子 agent 权限不高于父级
    let spawner: std::sync::Arc<dyn lex_core::tools::SubagentSpawner> = std::sync::Arc::new(
        lex_core::agent::subagent::SubagentRuntime::new(
            throttled.clone(),
            system.clone(),
            shared_handler.clone(),
            rules.clone(),
            cwd.clone(),
            shell.clone(),
            registry.clone(),
            lex_core::agent::subagent::SpawnLimits {
                max_turns: cfg.max_turns,
                context_limit,
                max_children_per_turn: cfg.agent.max_children_per_turn,
            },
            // 子 agent 活动行转发到共享渲染器(带 [子N] 归属)
            lex_core::agent::SubagentHooks { on_child_event },
        ),
    );

    Ok(AgentLoop {
        provider: Box::new(throttled),
        registry: (*registry).clone(),
        handler: shared_handler,
        tool_ctx: ToolContext { cwd, shell, todos, spawner: Some(spawner) },
        security: SecurityGuard::new(rules),
        system,
        history: vec![],
        max_turns: cfg.max_turns,
        cache_strategy: Some(cache_strategy),
        context_limit,
        pending_summary: None,
        compress_attempted: false,
        on_tool_result,
    })
}

/// 工具结果回调:转发到共享渲染器打印 ⎿ 结果行
fn make_result_hook(renderer: &Arc<Mutex<ui::events::Renderer>>) -> ToolResultHook {
    let r = Arc::clone(renderer);
    Arc::new(move |info| {
        r.lock().unwrap_or_else(|p| p.into_inner()).tool_result(info);
    })
}

/// 子 agent 活动行回调:转发到共享渲染器打印带 [子N] 归属的活动行
fn make_child_event_hook(renderer: &Arc<Mutex<ui::events::Renderer>>) -> lex_core::agent::ChildEventHook {
    let r = Arc::clone(renderer);
    Arc::new(move |ev: &lex_core::agent::ChildEvent| {
        r.lock().unwrap_or_else(|p| p.into_inner()).child_event(ev);
    })
}

fn current_model(cfg: &Config) -> String {
    if cfg.provider == "openai" {
        cfg.openai.model.clone()
    } else {
        cfg.anthropic.model.clone()
    }
}

/// canonicalize 产生的 `\\?\` 前缀只用于显示清理(Windows 扩展长度路径标记)
fn display_path(p: &Path) -> String {
    p.to_string_lossy().trim_start_matches(r"\\?\").to_string()
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("\n错误: {e:#}"); // {:#} 输出完整错误链
        std::process::exit(1);
    }
}

/// TUI 模式的日志文件槽(setup 阶段装入; tracing 写入走它而非 stderr)
static TUI_LOG_FILE: std::sync::OnceLock<std::sync::Mutex<std::fs::File>> = std::sync::OnceLock::new();

struct TuiFileWriterGuard;

impl std::io::Write for TuiFileWriterGuard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(m) = TUI_LOG_FILE.get() {
            if let Ok(mut f) = m.lock() {
                return std::io::Write::write(&mut *f, buf);
            }
        }
        // 文件不可用时静默丢弃:日志绝不能污染 TUI 画面
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(m) = TUI_LOG_FILE.get() {
            if let Ok(mut f) = m.lock() {
                return std::io::Write::flush(&mut *f);
            }
        }
        Ok(())
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    // 日志级别:LEX_LOG > RUST_LOG > 默认 warn
    let filter = std::env::var("LEX_LOG")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| std::env::var("RUST_LOG").ok())
        .unwrap_or_else(|| "warn".into());
    // TUI 全屏模式:stderr 会砸坏 ratatui 画面(日志文本画进输入框等区域),
    // 日志改写临时目录文件;其余模式照旧写 stderr
    let tui_planned = cli.task.is_empty() && use_tui(cli.plain, interactive_tty());
    if tui_planned {
        let path = std::env::temp_dir().join("lex-code-tui.log");
        if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = TUI_LOG_FILE.set(std::sync::Mutex::new(f));
        }
    }
    // 统一写入者:装入 TUI 日志文件则写文件,否则回落 stderr(文件不可用的兜底)
    let filter_env = tracing_subscriber::EnvFilter::new(filter);
    if tui_planned && TUI_LOG_FILE.get().is_some() {
        tracing_subscriber::fmt()
            .with_env_filter(filter_env)
            .with_writer(|| TuiFileWriterGuard)
            .with_ansi(false) // 文件不带 ANSI
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter_env)
            .with_writer(std::io::stderr)
            // stderr 非 TTY(管道/重定向)时关 ANSI,避免裸转义码
            .with_ansi(std::io::stderr().is_tty())
            .init();
    }

    let cwd = std::fs::canonicalize(&cli.cwd).context("工作目录不存在")?;
    let cfg = Config::load(&cwd)?;
    let input = std::sync::Arc::new(confirm::CliInput::new());
    let handler: std::sync::Arc<dyn lex_core::security::PermissionHandler> = input.clone();
    // 待办清单在 run() 层建好即交给 build_loop;TUI 侧句柄由 run_tui 从 agent.tool_ctx.todos 派生
    let todos: std::sync::Arc<std::sync::Mutex<Vec<lex_core::tools::Todo>>> = Default::default();
    let renderer = Arc::new(Mutex::new(ui::events::Renderer::new()));
    // 模式在 build_loop 之前判定:TUI 模式下不能注入纯文本渲染钩子,
    // 否则工具结果/子 agent 活动行会往 stdout 打 ANSI,破坏 TUI 画面。
    let tui_mode = cli.task.is_empty() && use_tui(cli.plain, interactive_tty());
    let mut agent = build_loop(
        &cfg,
        cwd.clone(),
        handler,
        todos,
        if tui_mode { None } else { Some(make_result_hook(&renderer)) },
        if tui_mode { None } else { Some(make_child_event_hook(&renderer)) },
    )?;

    if cli.task.is_empty() {
        if tui_mode {
            // agent 所有权移交 TUI(其内部替换 handler 为弹层裁决、重挂工具结果钩子)
            let cfg_shared = std::sync::Arc::new(std::sync::Mutex::new(cfg.clone()));
            tui::run::run_tui(agent, &cwd, cfg_shared, build_throttled_provider).await
        } else {
            interactive_session(&mut agent, &input, &cfg, &cwd).await
        }
    } else {
        let task = cli.task.join(" ");
        let text = agent
            .run_turn(&task, &mut |e| {
                renderer.lock().unwrap_or_else(|p| p.into_inner()).render(e);
            })
            .await?;
        // 收尾正文:上面已随流式事件渲染打印过一次,故
        // - TTY:不再重复输出一遍;
        // - 非 TTY(管道/重定向):补一份**渲染过的**完整正文,便于脚本消费
        //   —— 直接打印原文会让 `**粗体**`、`` `代码` `` 以字面形式出现在末尾。
        if let Some(tail) = ui::markdown::one_shot_tail(&text, std::io::stdout().is_tty()) {
            anstream::print!("{tail}");
        }
        Ok(())
    }
}

async fn interactive_session(
    agent: &mut AgentLoop,
    input: &Arc<confirm::CliInput>,
    cfg: &Config,
    cwd: &Path,
) -> Result<()> {
    ui::banner::print(&cfg.provider, &current_model(cfg), &display_path(cwd), &detect_project_type(cwd));
    let renderer = Arc::new(Mutex::new(ui::events::Renderer::new()));
    let mut history = ui::input::InputHistory::default();

    loop {
        let outcome = ui::input::read_input(&mut history, input).await?;
        let mut line = match outcome {
            ui::input::InputOutcome::Exit => {
                anstream::println!("\n再见");
                return Ok(());
            }
            ui::input::InputOutcome::Submitted(t) => t,
        };
        if line.trim().is_empty() {
            continue;
        }
        history.push(line.clone());

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
                    // raw mode 启用失败等错误打印一行后返回 REPL,不中断会话
                    anstream::println!("{}", ui::theme::error(&format!("设置页打开失败: {e}")));
                }
                // 返回 REPL:清屏并重绘横幅,恢复上下文
                anstream::print!("{}", ui::theme::CLEAR_SCREEN);
                ui::banner::print(&cfg.provider, &current_model(cfg), &display_path(cwd), &detect_project_type(cwd));
                continue;
            }
            Some(ui::settings::SlashCommand::Compact) => {
                anstream::println!("{}", ui::theme::dim("压缩中…"));
                match agent.compact_now().await {
                    Ok(msg) => anstream::println!("{}", ui::theme::success(&msg)),
                    Err(e) => {
                        anstream::println!("{}", ui::theme::error(&format!("压缩失败: {e:#}")));
                        anstream::println!("(历史已保留,可直接继续)");
                    }
                }
                continue;
            }
            Some(ui::settings::SlashCommand::Init) => {
                // 等价于提交一条内置提示词:落到底部正常的模型路径
                line = ui::settings::init_prompt();
            }
            Some(ui::settings::SlashCommand::Unknown) => {
                anstream::println!("{}", ui::theme::warn("未知命令,可用:/settings /compact /init"));
                continue;
            }
            None => {}
        }

        let r = Arc::clone(&renderer);
        r.lock().unwrap_or_else(|p| p.into_inner()).thinking_hint();
        let r2 = Arc::clone(&renderer);
        let mut on_event = |e: &lex_core::provider::ProviderEvent| {
            r2.lock().unwrap_or_else(|p| p.into_inner()).render(e);
        };
        let result = tokio::select! {
            res = agent.run_turn(&line, &mut on_event) => res,
            _ = tokio::signal::ctrl_c() => {
                // 打断本轮:流被 drop(子进程经 kill_on_drop 终止),修复悬空 tool_use 后回输入盒
                agent.recover_interrupt();
                anstream::println!(
                    "\n{}⏹ 已中断本轮任务,可继续输入{}",
                    ui::theme::WARN,
                    ui::theme::RESET
                );
                continue;
            }
        };
        if let Err(e) = result {
            anstream::println!("\n{}", ui::theme::error(&format!("本轮失败: {e}")));
            anstream::println!("(历史已保留,可直接继续描述或纠正)");
        }
    }
}

#[cfg(test)]
mod mode_tests {
    use super::use_tui;

    #[test]
    fn tui_requires_tty_and_absence_of_plain_flag() {
        assert!(use_tui(false, true));
        assert!(!use_tui(false, false), "非 TTY 必须降级纯文本");
        assert!(!use_tui(true, true), "--plain 强制纯文本");
        assert!(!use_tui(true, false));
    }
}
