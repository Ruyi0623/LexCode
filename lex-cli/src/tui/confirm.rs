use lex_core::security::{PendingAction, PermissionHandler};

use super::event::UiEvent;

/// TUI 模式的权限确认:把确认请求转发给 UI 线程弹层,经 oneshot 等待用户裁决。
pub struct TuiConfirm {
    tx: std::sync::mpsc::Sender<UiEvent>,
}

impl TuiConfirm {
    pub fn new(tx: std::sync::mpsc::Sender<UiEvent>) -> Self {
        TuiConfirm { tx }
    }
}

#[async_trait::async_trait]
impl PermissionHandler for TuiConfirm {
    async fn confirm(&self, action: &PendingAction) -> lex_core::error::Result<bool> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(UiEvent::Confirm { action: action.clone(), responder: tx })
            .map_err(|_| lex_core::error::LexError::Io(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "UI 已退出,无法确认")))?;
        // UI 线程退出等价于"拒绝":任何情况下不悬挂
        Ok(rx.await.unwrap_or(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lex_core::security::PendingDetail;

    fn action() -> PendingAction {
        PendingAction {
            tool_name: "bash_exec".into(),
            summary: "执行命令: ls".into(),
            detail: PendingDetail::Bash { command: "ls".into() },
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn confirm_round_trip_true() {
        let (tx, rx) = std::sync::mpsc::channel::<UiEvent>();
        let handler = TuiConfirm::new(tx);
        let h = tokio::spawn(async move { handler.confirm(&action()).await });
        match rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap_or_else(|_| panic!("2 秒内应收到 Confirm 事件")) {
            UiEvent::Confirm { action: a, responder } => {
                assert_eq!(a.tool_name, "bash_exec");
                responder.send(true).unwrap_or(());
            }
            _ => panic!("期望 Confirm,收到其他非 Confirm 事件(UiEvent 不可 Debug,故不打印内容)"),
        }
        assert!(h.await.unwrap_or_else(|_| Err(lex_core::error::LexError::Tool("join 失败".into()))).unwrap_or(false));
    }

    #[tokio::test]
    async fn dropped_ui_means_denied() {
        let (tx, rx) = std::sync::mpsc::channel::<UiEvent>();
        let handler = TuiConfirm::new(tx);
        drop(rx); // 模拟 UI 已退出
        assert!(handler.confirm(&action()).await.is_err(), "UI 退出应返回错误而非悬挂");
    }
}
