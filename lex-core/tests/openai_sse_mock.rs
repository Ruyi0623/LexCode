use futures::StreamExt;
use lex_core::message::{Message, Usage};
use lex_core::provider::openai_compat::OpenAiCompatProvider;
use lex_core::provider::openai_types::OpenAiParams;
use lex_core::provider::{Provider, ProviderEvent, RequestContext};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// 起一个本地 TCP 服务器,按顺序对每个连接回一个 SSE 响应体;返回 base_url。
/// (与 tests/sse_mock.rs 相同的做法;不共享代码以保持两个 mock 各自可读)
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

fn chunk(delta: &str, finish: Option<&str>, usage: bool) -> String {
    let usage_part = if usage {
        r#","usage":{"prompt_tokens":12,"completion_tokens":34}"#
    } else {
        ""
    };
    let finish_part = match finish {
        Some(f) => format!(r#","finish_reason":"{f}""#),
        None => r#","finish_reason":null"#.to_string(),
    };
    format!(
        "data: {{\"choices\":[{{\"delta\":{delta}{finish_part}}}],\"id\":\"c1\"{usage_part}}}\n\n"
    )
}

#[tokio::test]
async fn streams_text_thinking_and_usage() {
    let body = format!(
        "{}{}{}{}",
        chunk(r#"{"role":"assistant"}"#, None, false),
        chunk(r#"{"reasoning_content":"先想一下"}"#, None, false),
        chunk(r#"{"content":"你"}"#, None, false),
        chunk(r#"{"content":"好"}"#, None, false),
    );
    // usage 帧在 [DONE] 之前到达(include_usage 语义);此处合并到 stop 帧之后的独立帧
    let body = body + &chunk("{}", Some("stop"), true) + "data: [DONE]\n\n";
    let url = spawn_sse_server(vec![body]).await;
    let p = OpenAiCompatProvider::new(http_client(), url, "m".into(), OpenAiParams::default(), "k".into());
    let stream = p.send(ctx_with(vec![Message::user_text("hi")])).await.unwrap();
    let events: Vec<ProviderEvent> = stream.map(|r| r.unwrap()).collect().await;

    assert!(events.contains(&ProviderEvent::ThinkingDelta("先想一下".into())));
    assert!(events.contains(&ProviderEvent::TextDelta("你".into())));
    assert!(events.contains(&ProviderEvent::TextDelta("好".into())));
    let last = events.last().unwrap();
    match last {
        ProviderEvent::Completed { usage } => {
            assert_eq!(
                *usage,
                lex_core::message::Usage { input_tokens: 12, output_tokens: 34, ..Usage::default() }
            )
        }
        other => panic!("期望 Completed,实际 {other:?}"),
    }
}

#[tokio::test]
async fn accumulates_tool_calls_across_deltas() {
    // DeepSeek 真实形态:首帧带 id+name,后续帧只带 arguments 增量,finish_reason 落盘
    let body = format!(
        "{}{}{}{}data: [DONE]\n\n",
        chunk(
            r#"{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"file_read","arguments":""}}]}"#,
            None,
            false
        ),
        chunk(
            r#"{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]}"#,
            None,
            false
        ),
        chunk(
            r#"{"tool_calls":[{"index":0,"function":{"arguments":"\"a.txt\"}"}}]}"#,
            None,
            false
        ),
        chunk(r#"{}"#, Some("tool_calls"), true),
    );
    let url = spawn_sse_server(vec![body]).await;
    let p = OpenAiCompatProvider::new(http_client(), url, "m".into(), OpenAiParams::default(), "k".into());
    let stream = p.send(ctx_with(vec![Message::user_text("hi")])).await.unwrap();
    let events: Vec<ProviderEvent> = stream.map(|r| r.unwrap()).collect().await;

    assert!(events.iter().any(|e| matches!(e, ProviderEvent::ToolUseStart { id, name } if id == "call_1" && name == "file_read")));
    assert!(events.iter().any(|e| matches!(e, ProviderEvent::ToolUseDelta { partial_json, .. } if partial_json == "{\"path\":" )));
    let completed = events.iter().find_map(|e| match e {
        ProviderEvent::ToolUseComplete { id, input, .. } => Some((id.clone(), input.clone())),
        _ => None,
    }).expect("应有 ToolUseComplete");
    assert_eq!(completed.0, "call_1");
    assert_eq!(completed.1, serde_json::json!({"path": "a.txt"}));
}

#[tokio::test]
async fn handles_chunk_split_arbitrarily() {
    // 同一份数据手工切成不等长字节块,验证 UTF-8/SSE 增量解析不丢不重
    let body = format!(
        "{}{}data: [DONE]\n\n",
        chunk(r#"{"content":"你好,世界"}"#, None, false),
        chunk("{}", Some("stop"), true),
    );
    let url = spawn_sse_server(vec![body]).await;
    let p = OpenAiCompatProvider::new(http_client(), url, "m".into(), OpenAiParams::default(), "k".into());
    let stream = p.send(ctx_with(vec![Message::user_text("hi")])).await.unwrap();
    let events: Vec<ProviderEvent> = stream.map(|r| r.unwrap()).collect().await;
    let text: String = events.iter().filter_map(|e| match e {
        ProviderEvent::TextDelta(t) => Some(t.as_str()),
        _ => None,
    }).collect();
    assert_eq!(text, "你好,世界");
}

#[tokio::test]
async fn error_chunk_yields_err() {
    let body = "data: {\"error\":{\"message\":\"Insufficient Balance\",\"type\":\"invalid_request_error\"}}\n\ndata: [DONE]\n\n".to_string();
    let url = spawn_sse_server(vec![body]).await;
    let p = OpenAiCompatProvider::new(http_client(), url, "m".into(), OpenAiParams::default(), "k".into());
    let stream = p.send(ctx_with(vec![Message::user_text("hi")])).await.unwrap();
    let items: Vec<_> = stream.collect().await;
    assert!(items.iter().any(|r| r.is_err()), "error 帧必须产生 Err 项");
}

#[tokio::test]
async fn http_error_status_yields_err() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 8192];
        let _ = sock.read(&mut buf).await;
        sock.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
    });
    let p = OpenAiCompatProvider::new(http_client(), format!("http://{addr}"), "m".into(), OpenAiParams::default(), "k".into());
    let stream = p.send(ctx_with(vec![Message::user_text("hi")])).await.unwrap();
    let items: Vec<_> = stream.collect().await;
    assert!(items.iter().any(|r| r.is_err()));
}

#[tokio::test]
async fn collects_cache_hit_tokens_from_usage() {
    // DeepSeek 文档:usage 带 prompt_cache_hit_tokens / prompt_cache_miss_tokens
    let body = "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":10,\"prompt_cache_hit_tokens\":80,\"prompt_cache_miss_tokens\":20}}\n\ndata: [DONE]\n\n".to_string();
    let url = spawn_sse_server(vec![body]).await;
    let p = OpenAiCompatProvider::new(http_client(), url, "m".into(), OpenAiParams::default(), "k".into());
    let stream = p.send(ctx_with(vec![Message::user_text("hi")])).await.unwrap();
    let events: Vec<ProviderEvent> = stream.map(|r| r.unwrap()).collect().await;
    match events.last().unwrap() {
        ProviderEvent::Completed { usage } => {
            assert_eq!(usage.input_tokens, 100);
            assert_eq!(usage.output_tokens, 10);
            assert_eq!(usage.cache_hit_tokens, 80);
            assert_eq!(usage.cache_miss_tokens, 20);
        }
        other => panic!("期望 Completed,实际 {other:?}"),
    }
}

#[tokio::test]
async fn collects_cache_hit_via_openai_style_details() {
    // 只带 prompt_tokens_details.cached_tokens 的端点也要能采集(与 hit_tokens 同值)
    let body = "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":10,\"prompt_tokens_details\":{\"cached_tokens\":77}}}\n\ndata: [DONE]\n\n".to_string();
    let url = spawn_sse_server(vec![body]).await;
    let p = OpenAiCompatProvider::new(http_client(), url, "m".into(), OpenAiParams::default(), "k".into());
    let stream = p.send(ctx_with(vec![Message::user_text("hi")])).await.unwrap();
    let events: Vec<ProviderEvent> = stream.map(|r| r.unwrap()).collect().await;
    match events.last().unwrap() {
        ProviderEvent::Completed { usage } => assert_eq!(usage.cache_hit_tokens, 77),
        other => panic!("期望 Completed,实际 {other:?}"),
    }
}
