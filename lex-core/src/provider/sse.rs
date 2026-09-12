#[derive(Debug, Clone, PartialEq)]
pub struct RawSseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// 解析 SSE 文本;返回完整事件与未终结残余(残余须与新数据拼接后重新解析)。
pub fn parse(input: &str) -> (Vec<RawSseEvent>, String) {
    let mut events = Vec::new();
    let mut event_lines: Vec<String> = Vec::new();
    let mut current: Vec<String> = Vec::new();

    for line in input.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            if !current.is_empty() {
                event_lines.push(current.join("\n"));
                current = Vec::new();
            }
        } else {
            current.push(line.to_string());
        }
    }

    for block in event_lines {
        let mut event = None;
        let mut data: Vec<String> = Vec::new();
        for l in block.split('\n') {
            if let Some(rest) = l.strip_prefix(':') {
                let _ = rest; // 注释行
                continue;
            }
            if let Some(v) = l.strip_prefix("event:") {
                event = Some(v.trim_start().to_string());
            } else if let Some(v) = l.strip_prefix("data:") {
                data.push(v.strip_prefix(' ').unwrap_or(v).to_string());
            }
        }
        if !data.is_empty() || event.is_some() {
            events.push(RawSseEvent { event, data: data.join("\n") });
        }
    }

    (events, current.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_of(e: &RawSseEvent) -> &str { &e.data }

    #[test]
    fn parses_event_and_data() {
        let (evs, rest) = parse("event: content_block_delta\ndata: {\"a\":1}\n\n");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].event.as_deref(), Some("content_block_delta"));
        assert_eq!(data_of(&evs[0]), r#"{"a":1}"#);
        assert!(rest.is_empty());
    }

    #[test]
    fn keeps_incomplete_tail_as_remainder() {
        let (evs, rest) = parse("event: a\ndata: one\n\nevent: b\ndata: tw");
        assert_eq!(evs.len(), 1);
        assert_eq!(rest, "event: b\ndata: tw");
        let (evs2, rest2) = parse(&format!("{rest}o\n\n"));
        assert_eq!(evs2.len(), 1);
        assert_eq!(data_of(&evs2[0]), "two");
        assert!(rest2.is_empty());
    }

    #[test]
    fn tolerates_crlf_and_comments() {
        let (evs, _) = parse(": ping\r\nevent: x\r\ndata: 1\r\n\r\n");
        assert_eq!(evs.len(), 1);
        assert_eq!(data_of(&evs[0]), "1");
    }

    #[test]
    fn multi_line_data_joins_with_newline() {
        let (evs, _) = parse("data: l1\ndata: l2\n\n");
        assert_eq!(data_of(&evs[0]), "l1\nl2");
    }
}
