use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyEventKind};
use lex_core::agent::AgentLoop;
use lex_core::provider::ProviderEvent;
use lex_core::tools::Todo;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::confirm::TuiConfirm;
use super::draw::draw;
use super::event::{UiCommand, UiEvent};
use super::state::{AppState, KeyOutcome};

/// TUI 主入口:agent 留在 tokio 侧,渲染跑独立线程,双 channel 解耦。
///
/// `todos` **不**作为参数传入:UI 侧的待办句柄一律从 `agent.tool_ctx.todos` 派生。
/// 若两侧各持一个 `Arc`,面板会永远空白且单测发现不了(静默失败),
/// 从 agent 派生使这类失配在结构上不可能发生。
/// 上报当前上下文容量(估算)到 UI:输入栏下方常驻展示
fn send_ctx_usage(agent: &AgentLoop, ev_tx: &std::sync::mpsc::Sender<UiEvent>) {
    let _ = ev_tx.send(UiEvent::ContextUsage {
        used: lex_core::context::compress::estimate_tokens(&agent.history),
        limit: u64::from(agent.context_limit.unwrap_or(0)),
    });
}

pub type ProviderRebuild = fn(&lex_core::config::Config) -> Result<lex_core::provider::throttle::ThrottledProvider>;

pub async fn run_tui(
    mut agent: AgentLoop,
    cwd: &std::path::Path,
    cfg: std::sync::Arc<std::sync::Mutex<lex_core::config::Config>>,
    rebuild: ProviderRebuild,
) -> Result<()> {
    let todos = Arc::clone(&agent.tool_ctx.todos);
    let (ev_tx, ev_rx) = std::sync::mpsc::channel::<UiEvent>();
    // UI → agent:普通命令与打断分两条通道(run_turn 期间用 select! 监听打断)
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<UiCommand>(16);
    let (irq_tx, mut irq_rx) = tokio::sync::mpsc::channel::<UiCommand>(4);

    // 工具结果回调 → UI(⎿ 行 + 待办面板刷新)
    let hook_tx = ev_tx.clone();
    agent.on_tool_result = Some(Arc::new(move |info| {
        let _ = hook_tx.send(UiEvent::ToolResult {
            tool_name: info.tool_name.clone(),
            first_line: info.first_line.clone(),
            is_error: info.is_error,
        });
    }));
    // TUI 模式下权限确认改由弹层裁决:替换共享槽位的当前 handler,
    // 主循环与运行期间派生的所有子 agent 同步生效(子 agent 曾固化为 stdin
    // 确认,在 raw mode 下读行永远等不到换行,一派子 agent 就卡死)。
    agent.handler.set(std::sync::Arc::new(TuiConfirm::new(ev_tx.clone())));

    let project = crate::detect_project_type(cwd);
    let ui_thread = std::thread::spawn(move || ui_loop(ev_rx, cmd_tx, irq_tx, todos, project));

    // —— agent 侧循环(tokio)——
    loop {
        let Some(cmd) = cmd_rx.recv().await else { break };
        match cmd {
            UiCommand::Submit(text) => {
                // /init:替换为内置提示词走正常对话路径(回显仍是用户输入的 /init)
                let text = if matches!(
                    crate::ui::settings::parse_command(&text),
                    Some(crate::ui::settings::SlashCommand::Init)
                ) {
                    crate::ui::settings::init_prompt()
                } else {
                    text
                };
                // 斜杠命令与 REPL 同一分发口径:/settings 在 agent 侧构建快照推给 UI
                // (SettingsView 需要读 agent 统计,只能在 agent 侧构建);
                // 未知 / 前缀回一行提示,不进模型。
                match crate::ui::settings::parse_command(&text) {
                    Some(crate::ui::settings::SlashCommand::Settings) => {
                        let log_level = crate::ui::settings::resolve_log_level(
                            std::env::var("LEX_LOG").ok().as_deref(),
                            std::env::var("RUST_LOG").ok().as_deref(),
                        );
                        let live = cfg.lock().unwrap_or_else(|p| p.into_inner()).clone();
                        let view = crate::ui::settings::SettingsView::from(
                            &live,
                            crate::ui::settings::make_runtime(
                                &agent,
                                &crate::detect_project_type(cwd),
                                &cwd.to_string_lossy(),
                                &log_level,
                            ),
                        );
                        let _ = ev_tx.send(UiEvent::OpenSettings(view));
                        continue;
                    }
                    Some(crate::ui::settings::SlashCommand::Compact) => {
                        // 手动压缩:复用 run_turn 的忙态展示,结果作为一行回复进对话
                        let _ = ev_tx.send(UiEvent::TurnStarted);
                        let outcome = agent.compact_now().await;
                        match &outcome {
                            Ok(msg) => {
                                let _ = ev_tx.send(UiEvent::TextDelta(format!("{msg}\n")));
                            }
                            Err(e) => {
                                let _ = ev_tx.send(UiEvent::TextDelta(format!("压缩失败: {e:#}\n")));
                            }
                        }
                        let _ = ev_tx.send(UiEvent::TurnDone { ok: outcome.is_ok(), message: String::new() });
                        continue;
                    }
                    Some(crate::ui::settings::SlashCommand::Unknown) => {
                        let _ = ev_tx.send(UiEvent::TextDelta("未知命令,可用:/settings /compact /init\n".into()));
                        let _ = ev_tx.send(UiEvent::TurnDone { ok: true, message: String::new() });
                        continue;
                    }
                    // None(含 /init:文本已被替换为内置提示词)与剩余情况:走正常对话路径
                    _ => {}
                }
                let _ = ev_tx.send(UiEvent::TurnStarted);
                send_ctx_usage(&agent, &ev_tx);
                let ev = ev_tx.clone();
                let mut on_event = move |e: &ProviderEvent| {
                    let _ = forward_event(e, &ev);
                };
                // select! 的 future 临时量在本语句结束即析构,故 `&mut agent` 的借用
                // 在下方 match 里已释放 —— 中断分支才能安全调用 recover_interrupt()。
                let outcome = tokio::select! {
                    res = agent.run_turn(&text, &mut on_event) => Some(res),
                    Some(UiCommand::Interrupt) = irq_rx.recv() => None,
                };
                match outcome {
                    None => {
                        // 打断本轮:修复悬空 tool_use 后回输入盒
                        agent.recover_interrupt();
                        send_ctx_usage(&agent, &ev_tx);
                        let _ = ev_tx.send(UiEvent::TurnDone { ok: false, message: "已中断本轮任务,可继续输入".into() });
                        continue;
                    }
                    Some(Ok(_)) => {
                        // 清掉可能残留的迟到打断信号,避免下一轮被误中断
                        while irq_rx.try_recv().is_ok() {}
                        send_ctx_usage(&agent, &ev_tx);
                        let _ = ev_tx.send(UiEvent::TurnDone { ok: true, message: String::new() });
                    }
                    Some(Err(e)) => {
                        while irq_rx.try_recv().is_ok() {}
                        send_ctx_usage(&agent, &ev_tx);
                        let _ = ev_tx.send(UiEvent::TurnDone { ok: false, message: format!("本轮失败: {e:#}") });
                    }
                }
            }
            UiCommand::HotApply(new_cfg) => {
                // 设置页字段保存后的热生效:逐项应用,单项失败保留原值并上报
                let mut msgs: Vec<String> = Vec::new();
                match rebuild(&new_cfg) {
                    Ok(p) => {
                        agent.provider = Box::new(p);
                        msgs.push("Provider/模型已切换".into());
                    }
                    Err(e) => msgs.push(format!("Provider 热切换失败(保留原值): {e:#}")),
                }
                match lex_core::security::SecurityRules::build(&new_cfg.security) {
                    Ok(r) => {
                        agent.security = lex_core::security::SecurityGuard::new(r);
                        msgs.push("安全规则已更新".into());
                    }
                    Err(e) => msgs.push(format!("安全规则更新失败(保留原规则): {e:#}")),
                }
                agent.context_limit = new_cfg.context.enabled.then_some(new_cfg.context.limit);
                msgs.push("上下文限制已更新".into());
                // 改写了历史前缀:缓存链断开
                if let Some(cache) = &agent.cache_strategy {
                    cache.invalidate();
                }
                // 共享配置同步:下一次 /settings 快照反映新值
                if let Ok(mut live) = cfg.lock() {
                    *live = *new_cfg;
                }
                let _ = ev_tx.send(UiEvent::TextDelta(format!("⚙ {}\n", msgs.join(";"))));
                let _ = ev_tx.send(UiEvent::TurnDone { ok: true, message: String::new() });
                continue;
            }
            UiCommand::Quit => break,
            UiCommand::Interrupt => {} // 空闲期打断无意义,忽略
        }
    }
    let _ = ev_tx.send(UiEvent::Exit);
    let _ = ui_thread.join();
    Ok(())
}

fn forward_event(e: &ProviderEvent, tx: &std::sync::mpsc::Sender<UiEvent>) -> Result<()> {
    match e {
        ProviderEvent::TextDelta(t) => tx.send(UiEvent::TextDelta(t.clone())).map_err(mpsc_closed)?,
        ProviderEvent::ThinkingDelta(t) => tx.send(UiEvent::ThinkingDelta(t.clone())).map_err(mpsc_closed)?,
        ProviderEvent::ToolUseStart { name, .. } => tx.send(UiEvent::ToolStart { name: name.clone() }).map_err(mpsc_closed)?,
        // ToolUseComplete 不另发事件:执行中状态由 ToolStart 维持,结果由 ⎿ 回调呈现
        ProviderEvent::ToolUseComplete { .. } => {}
        ProviderEvent::Completed { usage } => tx
            .send(UiEvent::Usage {
                input: usage.input_tokens,
                output: usage.output_tokens,
                cache_hit: usage.cache_hit_tokens,
            })
            .map_err(mpsc_closed)?,
        ProviderEvent::ToolUseDelta { .. } => {}
    }
    Ok(())
}

fn mpsc_closed(_: std::sync::mpsc::SendError<UiEvent>) -> anyhow::Error {
    anyhow::anyhow!("UI 线程已退出")
}

/// 渲染线程主体:约 50Hz 排空事件 + 处理按键 + 重绘
fn ui_loop(
    ev_rx: std::sync::mpsc::Receiver<UiEvent>,
    cmd_tx: tokio::sync::mpsc::Sender<UiCommand>,
    irq_tx: tokio::sync::mpsc::Sender<UiCommand>,
    todos: Arc<Mutex<Vec<Todo>>>,
    project: String,
) {
    let mut terminal = match setup_terminal() {
        Ok(t) => t,
        // 终端初始化失败(如非 TTY):直接退出。cmd_tx 随本函数返回被 drop,
        // agent 侧 cmd_rx.recv() 随即得到 None → 结束循环,不会悬挂。
        Err(_) => return,
    };
    let mut app = AppState::new();
    app.status = format!("就绪 · {project}");

    loop {
        // 1. 按键(非阻塞)
        let has_event = event::poll(Duration::from_millis(20)).unwrap_or(false);
        if has_event {
            if let Ok(ev) = event::read() {
                match ev {
                    Event::Key(key) => {
                        if key.kind == KeyEventKind::Press {
                            match app.handle_key(key) {
                                KeyOutcome::None => {}
                                KeyOutcome::Submit(text) => {
                                    // 先回显再送出:输入盒随后被清空
                                    app.push_user_input(&text);
                                    let _ = cmd_tx.blocking_send(UiCommand::Submit(text));
                                }
                                // 裁决经弹层的 oneshot 回执送达 agent 侧,此处无需额外转发
                                KeyOutcome::Confirm(_) => {}
                                KeyOutcome::Interrupt => {
                                    let _ = irq_tx.blocking_send(UiCommand::Interrupt);
                                }
                                KeyOutcome::Quit => {
                                    let _ = cmd_tx.blocking_send(UiCommand::Quit);
                                    break;
                                }
                            }
                        }
                    }
                    Event::Mouse(m) => match m.kind {
                        // 滚轮:确认弹层为最上层时只滚弹层内容,否则回看主对话(每格 3 行)
                        crossterm::event::MouseEventKind::ScrollUp => {
                            if app.pending_confirm.is_some() {
                                app.confirm_scroll = app.confirm_scroll.saturating_add(3);
                            } else {
                                app.scroll = app.scroll.saturating_add(3);
                            }
                        }
                        crossterm::event::MouseEventKind::ScrollDown => {
                            if app.pending_confirm.is_some() {
                                app.confirm_scroll = app.confirm_scroll.saturating_sub(3);
                            } else {
                                app.scroll = app.scroll.saturating_sub(3);
                            }
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
        // 1.5 设置页字段保存:热生效命令发往 agent 侧
        if let Some(new_cfg) = app.take_settings_commit() {
            let _ = cmd_tx.blocking_send(UiCommand::HotApply(new_cfg));
        }
        // 2. 排空 agent 事件
        while let Ok(ev) = ev_rx.try_recv() {
            app.apply(ev, &todos);
            if app.quit {
                break;
            }
        }
        if app.quit {
            break;
        }
        // 3. 重绘
        if terminal.draw(|f| draw(&mut app, f)).is_err() {
            break;
        }
    }
    let _ = restore_terminal(&mut terminal);
}

type TuiTerminal = ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

fn setup_terminal() -> Result<TuiTerminal> {
    crossterm::terminal::enable_raw_mode().context("启用 raw mode 失败")?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )
    .context("进入备用屏幕失败")?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    ratatui::Terminal::new(backend).context("初始化 ratatui 终端失败")
}

fn restore_terminal(terminal: &mut TuiTerminal) -> Result<()> {
    // 顺序与启用时相反:先关鼠标捕获再退备用屏
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::event::DisableMouseCapture,
        crossterm::terminal::LeaveAlternateScreen
    )
    .context("退出备用屏幕失败")?;
    crossterm::terminal::disable_raw_mode().context("关闭 raw mode 失败")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 限时收一条事件:桩未发送时必须**失败**,而不是像 recv() 那样永久挂起
    /// (本项目据此出过一次「cargo test 不返回」的假死)
    fn recv_one(rx: &std::sync::mpsc::Receiver<UiEvent>) -> UiEvent {
        rx.recv_timeout(Duration::from_secs(2)).unwrap_or_else(|_| panic!("2 秒内未收到 UI 事件"))
    }

    #[test]
    fn forward_event_maps_text_delta_to_ui_event() {
        let (tx, rx) = std::sync::mpsc::channel::<UiEvent>();
        forward_event(&ProviderEvent::TextDelta("hello".into()), &tx).unwrap();
        match recv_one(&rx) {
            UiEvent::TextDelta(ref s) => assert_eq!(s, "hello"),
            _ => panic!("期望 TextDelta 事件"),
        }
    }

    #[test]
    fn forward_event_maps_usage_to_ui_event() {
        let (tx, rx) = std::sync::mpsc::channel::<UiEvent>();
        let usage = lex_core::message::Usage { input_tokens: 1, output_tokens: 2, cache_hit_tokens: 3, cache_miss_tokens: 4 };
        forward_event(&ProviderEvent::Completed { usage }, &tx).unwrap();
        match recv_one(&rx) {
            UiEvent::Usage { input, output, cache_hit } => {
                assert_eq!(input, 1);
                assert_eq!(output, 2);
                assert_eq!(cache_hit, 3);
            }
            _ => panic!("期望 Usage 事件"),
        }
    }

    #[test]
    fn forward_event_maps_tool_start_and_ignores_deltas() {
        let (tx, rx) = std::sync::mpsc::channel::<UiEvent>();
        forward_event(&ProviderEvent::ToolUseStart { id: "t1".into(), name: "file_read".into() }, &tx).unwrap();
        forward_event(&ProviderEvent::ToolUseDelta { id: "t1".into(), partial_json: "{}".into() }, &tx).unwrap();
        forward_event(&ProviderEvent::ToolUseComplete { id: "t1".into(), name: "file_read".into(), input: serde_json::json!({}) }, &tx)
            .unwrap();
        match recv_one(&rx) {
            UiEvent::ToolStart { ref name } => assert_eq!(name, "file_read"),
            _ => panic!("期望 ToolStart 事件"),
        }
        // ToolUseDelta / ToolUseComplete 不产生 UI 事件
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err(), "增量与完成事件不应转发到 UI");
    }
}
