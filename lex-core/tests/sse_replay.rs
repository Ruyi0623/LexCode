use futures::StreamExt;
use lex_core::provider::anthropic::AnthropicProvider;
use lex_core::provider::{Provider, ProviderEvent, RequestContext};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// 用本地 TCP 服务器回放真实抓包字节,按给定 chunk 大小切分发送。
async fn replay(body: Vec<u8>, chunk_size: usize) -> (Vec<ProviderEvent>, Vec<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 65536];
        loop {
            match sock.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") { break; }
                }
            }
        }
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        sock.write_all(head.as_bytes()).await.unwrap();
        for c in body.chunks(chunk_size) {
            sock.write_all(c).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        sock.shutdown().await.ok();
    });

    let p = AnthropicProvider::new(reqwest::Client::new(), format!("http://{addr}"), "m".into(), 8, "k".into());
    let ctx = RequestContext { system: "s".into(), tools: vec![], messages: vec![lex_core::message::Message::user_text("hi")] };
    let stream = p.send(ctx).await.unwrap();
    let (oks, errs): (Vec<_>, Vec<_>) = stream.map(|r| r).collect::<Vec<_>>().await.into_iter().partition(|r| r.is_ok());
    let events: Vec<ProviderEvent> = oks.into_iter().map(|r| r.unwrap()).collect();
    (events, errs.into_iter().map(|r| r.unwrap_err().to_string()).collect())
}

#[tokio::test]
async fn no_duplicate_events_across_chunk_sizes() {
    let raw = std::fs::read("D:/lexcode-smoke/raw-turn.bin").expect("raw capture exists");
    // 回归:真实 DeepSeek 抓包流(6401 字节)在不同 chunk 切分下,
    // 每个事件必须恰好出现一次(曾经因 UTF-8 尾字节处理错误导致整段缓冲被重复解析)
    for chunk_size in [64usize, 256, 1024, 4096] {
        let (events, errs) = replay(raw.clone(), chunk_size).await;
        assert!(errs.is_empty(), "chunk_size={chunk_size} 有错误: {errs:?}");
        let count = |pred: fn(&ProviderEvent) -> bool| events.iter().filter(|e| pred(e)).count();
        assert_eq!(count(|e| matches!(e, ProviderEvent::ToolUseStart { .. })), 2, "chunk_size={chunk_size}");
        assert_eq!(count(|e| matches!(e, ProviderEvent::ToolUseComplete { .. })), 2, "chunk_size={chunk_size}");
        assert_eq!(count(|e| matches!(e, ProviderEvent::Completed { .. })), 1, "chunk_size={chunk_size}");
    }
}
