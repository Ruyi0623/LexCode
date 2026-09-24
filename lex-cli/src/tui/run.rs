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
pub async fn run_tui(mut agent: AgentLoop, cwd: &std::path::Path) -> Result<()> {
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
    // TUI 模式下权限确认改由弹层裁决:替换掉纯文本的 CliInput handler
    agent.handler = Box::new(TuiConfirm::new(ev_tx.clone()));

    let project = crate::detect_project_type(cwd);
    let ui_thread = std::thread::spawn(move || ui_loop(ev_rx, cmd_tx, irq_tx, todos, project));

    // —— agent 侧循环(tokio)——
    loop {
        let Some(cmd) = cmd_rx.recv().await else { break };
        match cmd {
            UiCommand::Submit(text) => {
                let _ = ev_tx.send(UiEvent::TurnStarted);
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
                        let _ = ev_tx.send(UiEvent::TurnDone { ok: false, message: "已中断本轮任务,可继续输入".into() });
                        continue;
                    }
                    Some(Ok(_)) => {
                        // 清掉可能残留的迟到打断信号,避免下一轮被误中断
                        while irq_rx.try_recv().is_ok() {}
                        let _ = ev_tx.send(UiEvent::TurnDone { ok: true, message: String::new() });
                    }
                    Some(Err(e)) => {
                        while irq_rx.try_recv().is_ok() {}
                        let _ = ev_tx.send(UiEvent::TurnDone { ok: false, message: format!("本轮失败: {e:#}") });
                    }
                }
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
                if let Event::Key(key) = ev {
                    if key.kind == KeyEventKind::Press {
                        match app.handle_key(key) {
                            KeyOutcome::None => {}
                            KeyOutcome::Submit(text) => {
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
            }
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
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen).context("进入备用屏幕失败")?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    ratatui::Terminal::new(backend).context("初始化 ratatui 终端失败")
}

fn restore_terminal(terminal: &mut TuiTerminal) -> Result<()> {
    crossterm::execute!(terminal.backend_mut(), crossterm::terminal::LeaveAlternateScreen)
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
