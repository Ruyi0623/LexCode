mod confirm;
mod render;

use anyhow::{Context, Result};
use clap::Parser;
use lex_core::agent::AgentLoop;
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

#[derive(Parser)]
#[command(name = "lex-code", version, about = "Lex Code — 终端编程 agent")]
struct Cli {
    /// 任务描述(留空进入交互模式)
    task: Vec<String>,
    /// 工作目录
    #[arg(short = 'C', default_value = ".")]
    cwd: PathBuf,
}

fn detect_project_type(cwd: &Path) -> String {
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

fn build_loop(cfg: &Config, cwd: PathBuf, input: std::sync::Arc<confirm::CliInput>) -> Result<AgentLoop> {
    let api_key = resolve_api_key(&cfg.provider)?;
    // 切换 provider 只改配置,不改 Agent Loop:两者实现同一个 Provider trait
    let provider: Box<dyn Provider> = match cfg.provider.as_str() {
        "openai" => Box::new(OpenAiCompatProvider::with_defaults(
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
        _ => Box::new(AnthropicProvider::with_defaults(
            cfg.anthropic.base_url.clone(),
            cfg.anthropic.model.clone(),
            cfg.anthropic.max_tokens,
            api_key,
        )?),
    };

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FileRead));
    registry.register(Box::new(FileEdit));
    registry.register(Box::new(BashExec));
    registry.register(Box::new(GrepSearch));
    registry.register(Box::new(TodoWrite));

    let shell: Option<ShellCommand> = cfg.shell.command.clone().map(|command| ShellCommand {
        command,
        args: cfg.shell.args.clone().unwrap_or_default(),
    });

    let base_system = build_system_prompt(cfg, &cwd)?;
    // AGENTS.md 一次性注入到系统提示词之后,进入缓存前缀(此后逐字节不变)
    let agents_md = load_agents_md(&cwd);
    let system = assemble_system_prompt(&base_system, agents_md.as_deref());
    if agents_md.is_some() {
        anstream::println!("\x1b[2m已加载项目指引 AGENTS.md\x1b[0m");
    }
    // 隐式前缀缓存策略:两个 provider 通用(前缀一致性校验 + 命中率遥测)
    let cache_strategy: std::sync::Arc<dyn CacheStrategy> =
        std::sync::Arc::new(ImplicitPrefixCacheStrategy::new());
    Ok(AgentLoop {
        provider,
        registry,
        handler: Box::new(input),
        tool_ctx: ToolContext { cwd, shell, todos: Default::default() },
        security: SecurityGuard::new(SecurityRules::build(&cfg.security)?),
        system,
        history: vec![],
        max_turns: cfg.max_turns,
        cache_strategy: Some(cache_strategy),
        context_limit: cfg.context.enabled.then_some(cfg.context.limit),
        pending_summary: None,
        compress_attempted: false,
    })
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("\n错误: {e:#}"); // {:#} 输出完整错误链
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cwd = std::fs::canonicalize(&cli.cwd).context("工作目录不存在")?;
    let cfg = Config::load(&cwd)?;
    let input = std::sync::Arc::new(confirm::CliInput::new());
    let mut agent = build_loop(&cfg, cwd.clone(), input.clone())?;

    if cli.task.is_empty() {
        interactive_session(&mut agent, &input).await
    } else {
        let task = cli.task.join(" ");
        let mut renderer = render::Renderer::new();
        let text = agent.run_turn(&task, &mut |e| renderer.render(e)).await?;
        println!("\n{text}");
        Ok(())
    }
}

async fn interactive_session(agent: &mut AgentLoop, input: &confirm::CliInput) -> Result<()> {
    anstream::println!("Lex Code 交互模式(输入任务,空行取消,Ctrl+C 退出)");

    loop {
        let line = tokio::select! {
            l = input.read_line("\n› ") => l.context("读取输入失败")?,
            _ = tokio::signal::ctrl_c() => {
                anstream::println!("\n再见");
                return Ok(());
            }
        };

        // EOF(如 Ctrl+D / 管道结束):优雅退出
        if line.is_empty() {
            anstream::println!("\n再见");
            return Ok(());
        }
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        let result = {
            let mut renderer = render::Renderer::new();
            agent.run_turn(input, &mut |e| renderer.render(e)).await
        };
        if let Err(e) = result {
            anstream::println!("\n\x1b[31m本轮失败: {e}\x1b[0m");
            anstream::println!("(历史已保留,可直接继续描述或纠正)");
        }
    }
}
