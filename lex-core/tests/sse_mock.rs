use futures::StreamExt;
use lex_core::message::{Block, Message, Usage};
use lex_core::provider::anthropic::AnthropicProvider;
use lex_core::provider::{Provider, ProviderEvent, RequestContext};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// 起一个本地 TCP 服务器,按顺序对每个连接回一个 SSE 响应体;返回 base_url。
async fn spawn_sse_server(bodies: Vec<String>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for body in bodies {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 16384];
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                if n == 0 || find_header_end(&buf[..n]) { break; }
            }
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            sock.flush().await.unwrap();
        }
    });
    format!("http://{addr}")
}

fn find_header_end(b: &[u8]) -> bool {
    b.windows(4).any(|w| w == b"\r\n\r\n")
}

/// Mock 服务器必须直连:环境若设置 http_proxy,reqwest 默认会把回环请求转发给代理导致偶发 502。
fn http_client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

fn ctx_with(messages: Vec<Message>) -> RequestContext {
    RequestContext { system: "sys".into(), tools: vec![], messages }
}

fn text_sse(text: &str) -> String {
    format!(
        "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"usage\":{{\"input_tokens\":10,\"output_tokens\":1}}}}}}\n\n\
         event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n\
         event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{text}\"}}}}\n\n\
         event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
         event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":7}}}}\n\n\
         event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
    )
}

#[tokio::test]
async fn streams_text_events_and_usage() {
    let url = spawn_sse_server(vec![text_sse("你好")]).await;
    let p = AnthropicProvider::new(http_client(), url, "m".into(), 8, "k".into());
    let stream = p.send(ctx_with(vec![Message::user_text("hi")])).await.unwrap();
    let events: Vec<ProviderEvent> = stream.map(|r| r.unwrap()).collect().await;

    assert_eq!(events[0], ProviderEvent::TextDelta("你好".into()));
    let last = events.last().unwrap();
    match last {
        ProviderEvent::Completed { usage } => assert_eq!(*usage, Usage { input_tokens: 10, output_tokens: 7, ..Usage::default() }),
        other => panic!("期望 Completed,实际 {other:?}"),
    }
}

#[tokio::test]
async fn accumulates_tool_use_json_across_deltas() {
    let body = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu1\",\"name\":\"file_read\",\"input\":{}}}\n\n\
                event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n\
                event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"a.txt\\\"}\"}}\n\n\
                event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
                event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":20}}\n\n\
                event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    let url = spawn_sse_server(vec![body.to_string()]).await;
    let p = AnthropicProvider::new(http_client(), url, "m".into(), 8, "k".into());
    let stream = p.send(ctx_with(vec![Message::user_text("hi")])).await.unwrap();
    let events: Vec<ProviderEvent> = stream.map(|r| r.unwrap()).collect().await;

    assert!(events.iter().any(|e| matches!(e, ProviderEvent::ToolUseStart { id, name } if id == "tu1" && name == "file_read")));
    let completed = events.iter().find_map(|e| match e {
        ProviderEvent::ToolUseComplete { id, input, .. } => Some((id.clone(), input.clone())),
        _ => None,
    }).expect("应有 ToolUseComplete");
    assert_eq!(completed.0, "tu1");
    assert_eq!(completed.1, serde_json::json!({"path": "a.txt"}));
}

#[tokio::test]
async fn sse_error_event_yields_err() {
    let body = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"bad\"}}\n\n";
    let url = spawn_sse_server(vec![body.to_string()]).await;
    let p = AnthropicProvider::new(http_client(), url, "m".into(), 8, "k".into());
    let stream = p.send(ctx_with(vec![Message::user_text("hi")])).await.unwrap();
    let items: Vec<_> = stream.collect().await;
    assert!(items.iter().any(|r| r.is_err()), "error 事件必须产生 Err 项");
}

#[tokio::test]
async fn http_error_status_yields_err() {
    // 4xx 服务器:返回非 200
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 8192];
        let _ = sock.read(&mut buf).await;
        sock.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
    });
    let p = AnthropicProvider::new(http_client(), format!("http://{addr}"), "m".into(), 8, "k".into());
    let stream = p.send(ctx_with(vec![Message::user_text("hi")])).await.unwrap();
    let items: Vec<_> = stream.collect().await;
    assert!(items.iter().any(|r| r.is_err()));
    let _ = Duration::from_secs(1); // 保持 import 最小化
    let _ = Block::Text { text: String::new() }; // 保持 message import 使用
}
