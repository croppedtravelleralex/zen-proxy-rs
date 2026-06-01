use bytes::BytesMut;
use futures::stream::StreamExt;
use rand::Rng;

use reqwest::Client;
use serde::Deserialize;
use std::hash::{Hash, Hasher};
use std::time::{SystemTime, UNIX_EPOCH};

const UA: &str = "opencode/1.15.5 ai-sdk/provider-utils/4.0.23 runtime/bun/1.3.14";

#[derive(Debug, Deserialize)]
pub struct ZenSseEvent {
    pub id: Option<String>,
    pub object: Option<String>,
    pub created: Option<u64>,
    pub model: Option<String>,
    pub choices: Option<Vec<ZenChoice>>,
    pub usage: Option<ZenUsage>,
}

#[derive(Debug, Deserialize)]
pub struct ZenChoice {
    pub index: Option<u64>,
    pub delta: Option<ZenDelta>,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ZenDelta {
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<ZenToolCallDelta>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ZenToolCallDelta {
    pub index: Option<i64>,
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub call_type: Option<String>,
    pub function: Option<ZenFunctionDelta>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ZenFunctionDelta {
    pub name: Option<String>,
    pub arguments: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ZenUsage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub prompt_tokens_details: Option<serde_json::Value>,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
}

#[derive(Debug, Default)]
pub struct CollectedStream {
    pub content: String,
    pub reasoning: String,
    pub usage: Option<ZenUsage>,
    pub tool_calls: Vec<CollectedToolCall>,
    pub saw_done: bool,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct CollectedToolCall {
    pub index: i64,
    pub id: Option<String>,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SseFrame {
    pub event: Option<String>,
    pub id: Option<String>,
    pub retry: Option<u64>,
    pub data: String,
}

fn make_id(prefix: &str) -> String {
    let alphabet: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut rng = rand::thread_rng();
    let tail: String = (0..26)
        .map(|_| {
            let idx = rng.gen_range(0..alphabet.len());
            alphabet[idx] as char
        })
        .collect();
    format!("{}_{}", prefix, tail)
}

fn short_hash(input: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn stable_session_id(api_key: &str, body: &serde_json::Value) -> String {
    let model = body
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let ttl_secs = std::env::var("ZEN_UPSTREAM_SESSION_TTL_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3600)
        .max(1);
    let bucket = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / ttl_secs;
    let scope = session_scope(body);
    format!(
        "ses_{}",
        short_hash(&format!(
            "{}:{}:{}:{}",
            short_hash(api_key),
            model,
            bucket,
            scope
        ))
    )
}

fn stable_project_id(body: &serde_json::Value) -> String {
    let scope = session_scope(body);
    if scope == "normal" {
        "global".to_string()
    } else {
        format!("proj_{}", short_hash(&scope))
    }
}

fn session_scope(body: &serde_json::Value) -> String {
    let material = serde_json::to_string(&body.get("messages")).unwrap_or_default();
    let estimated_tokens = material.len() / 4;
    let compacted = material.contains("free-model-client-rs context compactor");
    if compacted || estimated_tokens >= 10_000 {
        return format!("large:{}", short_hash(&material));
    }
    "normal".to_string()
}

pub fn zen_headers(api_key: &str, body: &serde_json::Value) -> Vec<(String, String)> {
    vec![
        ("authorization".into(), format!("Bearer {}", api_key)),
        ("user-agent".into(), UA.into()),
        ("x-opencode-client".into(), "cli".into()),
        ("x-opencode-project".into(), stable_project_id(body)),
        ("x-opencode-request".into(), make_id("msg")),
        (
            "x-opencode-session".into(),
            stable_session_id(api_key, body),
        ),
    ]
}

pub async fn fetch_zen_stream(
    client: &Client,
    zen_url: &str,
    api_key: &str,
    body: &serde_json::Value,
) -> Result<reqwest::Response, crate::error::AppError> {
    fetch_zen_stream_with_headers(client, zen_url, api_key, body, &[]).await
}

pub async fn fetch_zen_stream_with_headers(
    client: &Client,
    zen_url: &str,
    api_key: &str,
    body: &serde_json::Value,
    extra_headers: &[(String, String)],
) -> Result<reqwest::Response, crate::error::AppError> {
    let mut req = client.post(zen_url).json(body);
    for (k, v) in zen_headers(api_key, body) {
        req = req.header(k, v);
    }
    for (k, v) in extra_headers {
        req = req.header(k, v);
    }
    let resp = req.send().await.map_err(|e| {
        if e.is_timeout() {
            crate::error::AppError::new(axum::http::StatusCode::GATEWAY_TIMEOUT, "upstream timeout")
        } else {
            crate::error::AppError::new(
                axum::http::StatusCode::BAD_GATEWAY,
                format!("upstream connection error: {e}"),
            )
        }
    })?;
    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let body_text = resp.text().await.unwrap_or_default();
        return Err(crate::error::AppError::upstream(
            status,
            body_text,
            retry_after,
        ));
    }
    Ok(resp)
}

pub async fn collect_stream_text(
    resp: reqwest::Response,
) -> Result<(String, String, Option<ZenUsage>), crate::error::AppError> {
    let collected = collect_stream_parts(resp).await?;
    Ok((collected.content, collected.reasoning, collected.usage))
}

pub async fn collect_stream_parts(
    resp: reqwest::Response,
) -> Result<CollectedStream, crate::error::AppError> {
    let mut stream = resp.bytes_stream();
    let mut parser = SseParser::default();
    let mut collected = CollectedStream::default();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            crate::error::AppError::new(
                axum::http::StatusCode::BAD_GATEWAY,
                format!("stream error: {e}"),
            )
        })?;
        parser.push(&chunk);
        while let Some(frame) = parser.next_frame()? {
            apply_sse_frame_to_collection(frame, &mut collected)?;
        }
    }
    parser.finish()?;
    if !collected.saw_done && collected.finish_reason.is_none() {
        return Err(truncated_stream_error());
    }
    Ok(collected)
}

pub fn stream_sse_events(
    byte_stream: impl futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
) -> impl futures::Stream<Item = Result<ZenSseEvent, crate::error::AppError>> {
    use futures::StreamExt;
    async_stream::stream! {
        let mut byte_stream = Box::pin(byte_stream);
        let mut parser = SseParser::default();
        let mut complete = false;
        loop {
            match byte_stream.next().await {
                Some(Ok(chunk)) => {
                    parser.push(&chunk);
                    loop {
                        let frame = match parser.next_frame() {
                            Ok(Some(frame)) => frame,
                            Ok(None) => break,
                            Err(err) => {
                                yield Err(err);
                                return;
                            }
                        };
                        match parse_zen_frame(frame) {
                            Ok(Some(ParsedZenFrame::Done)) => complete = true,
                            Ok(Some(ParsedZenFrame::Event(event))) => {
                                if event_has_finish_reason(&event) {
                                    complete = true;
                                }
                                yield Ok(*event);
                            }
                            Ok(None) => {}
                            Err(err) => {
                                yield Err(err);
                                return;
                            }
                        }
                    }
                }
                Some(Err(e)) => {
                    yield Err(crate::error::AppError::new(
                        axum::http::StatusCode::BAD_GATEWAY,
                        format!("stream error: {e}"),
                    ));
                    return;
                }
                None => {
                    if let Err(err) = parser.finish() {
                        yield Err(err);
                        return;
                    }
                    if !complete {
                        yield Err(truncated_stream_error());
                    }
                    return;
                }
            }
        }
    }
}

#[derive(Default)]
struct SseParser {
    buffer: BytesMut,
}

impl SseParser {
    fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    fn next_frame(&mut self) -> Result<Option<SseFrame>, crate::error::AppError> {
        let Some((pos, delimiter_len)) = next_sse_delimiter(&self.buffer) else {
            return Ok(None);
        };
        let frame_bytes = self.buffer.split_to(pos);
        let _ = self.buffer.split_to(delimiter_len);
        parse_sse_frame(&frame_bytes).map(Some)
    }

    fn finish(&self) -> Result<(), crate::error::AppError> {
        if self.buffer.is_empty() || self.buffer.iter().all(|byte| byte.is_ascii_whitespace()) {
            Ok(())
        } else {
            Err(truncated_stream_error())
        }
    }
}

fn next_sse_delimiter(buffer: &BytesMut) -> Option<(usize, usize)> {
    let lf = buffer
        .windows(2)
        .position(|w| w == b"\n\n")
        .map(|pos| (pos, 2));
    let crlf = buffer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|pos| (pos, 4));
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(found), None) | (None, Some(found)) => Some(found),
        (None, None) => None,
    }
}

fn parse_sse_frame(bytes: &[u8]) -> Result<SseFrame, crate::error::AppError> {
    let text = std::str::from_utf8(bytes).map_err(|e| {
        crate::error::AppError::new(
            axum::http::StatusCode::BAD_GATEWAY,
            format!("stream utf8 error: {e}"),
        )
    })?;
    let mut frame = SseFrame::default();
    let mut data_lines = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => {
                let value = value.strip_prefix(' ').unwrap_or(value);
                (field, value)
            }
            None => (line, ""),
        };
        match field {
            "data" => data_lines.push(value.to_string()),
            "event" => frame.event = Some(value.to_string()),
            "id" => frame.id = Some(value.to_string()),
            "retry" => frame.retry = value.parse::<u64>().ok(),
            _ => {}
        }
    }
    frame.data = data_lines.join("\n");
    Ok(frame)
}

enum ParsedZenFrame {
    Done,
    Event(Box<ZenSseEvent>),
}

fn parse_zen_frame(frame: SseFrame) -> Result<Option<ParsedZenFrame>, crate::error::AppError> {
    if frame.data.is_empty() {
        return Ok(None);
    }
    if frame.data.trim() == "[DONE]" {
        return Ok(Some(ParsedZenFrame::Done));
    }
    serde_json::from_str::<ZenSseEvent>(&frame.data)
        .map(|event| Some(ParsedZenFrame::Event(Box::new(event))))
        .map_err(|e| {
            crate::error::AppError::new(
                axum::http::StatusCode::BAD_GATEWAY,
                format!("stream parse error: {e}"),
            )
        })
}

fn apply_sse_frame_to_collection(
    frame: SseFrame,
    collected: &mut CollectedStream,
) -> Result<(), crate::error::AppError> {
    let Some(parsed) = parse_zen_frame(frame)? else {
        return Ok(());
    };
    match parsed {
        ParsedZenFrame::Done => {
            collected.saw_done = true;
        }
        ParsedZenFrame::Event(event) => {
            if event.usage.is_some() {
                collected.usage = event.usage;
            }
            if let Some(choices) = event.choices {
                for choice in choices {
                    if let Some(finish_reason) = choice.finish_reason {
                        collected.finish_reason = Some(finish_reason);
                    }
                    if let Some(delta) = choice.delta {
                        if let Some(c) = delta.content {
                            collected.content.push_str(&c);
                        }
                        if let Some(r) = delta.reasoning_content {
                            collected.reasoning.push_str(&r);
                        }
                        if let Some(tool_calls) = delta.tool_calls {
                            merge_collected_tool_deltas(&mut collected.tool_calls, tool_calls);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn event_has_finish_reason(event: &ZenSseEvent) -> bool {
    event
        .choices
        .as_ref()
        .is_some_and(|choices| choices.iter().any(|choice| choice.finish_reason.is_some()))
}

pub fn merge_collected_tool_deltas(
    tool_calls: &mut Vec<CollectedToolCall>,
    deltas: Vec<ZenToolCallDelta>,
) {
    for tc in deltas {
        let index = tc.index.unwrap_or(0);
        let existing = tool_calls.iter_mut().find(|item| item.index == index);
        let item = if let Some(item) = existing {
            item
        } else {
            tool_calls.push(CollectedToolCall {
                index,
                id: tc.id.clone(),
                ..CollectedToolCall::default()
            });
            tool_calls.last_mut().unwrap()
        };
        if item.id.is_none() {
            item.id = tc.id.clone();
        }
        if let Some(function) = tc.function {
            if let Some(name) = function.name {
                if !name.is_empty() {
                    item.name = name;
                }
            }
            if let Some(arguments) = function.arguments {
                item.arguments.push_str(&arguments);
            }
        }
    }
}

fn truncated_stream_error() -> crate::error::AppError {
    crate::error::AppError::new(
        axum::http::StatusCode::BAD_GATEWAY,
        "stream truncated before DONE or finish_reason",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn header_value(headers: &[(String, String)], name: &str) -> String {
        headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .unwrap()
    }

    #[test]
    fn opencode_session_is_stable_for_same_key_and_model() {
        let body = json!({"model":"deepseek-v4-flash-free"});
        let first = zen_headers("sk-test", &body);
        let second = zen_headers("sk-test", &body);

        assert_eq!(
            header_value(&first, "x-opencode-session"),
            header_value(&second, "x-opencode-session")
        );
        assert_ne!(
            header_value(&first, "x-opencode-request"),
            header_value(&second, "x-opencode-request")
        );
    }

    #[test]
    fn opencode_session_changes_by_model() {
        let first = zen_headers("sk-test", &json!({"model":"deepseek-v4-flash-free"}));
        let second = zen_headers("sk-test", &json!({"model":"big pickle"}));

        assert_ne!(
            header_value(&first, "x-opencode-session"),
            header_value(&second, "x-opencode-session")
        );
    }

    #[test]
    fn opencode_session_changes_by_large_prompt_hash() {
        let first = zen_headers(
            "sk-test",
            &json!({"model":"deepseek-v4-flash-free","messages":[{"role":"user","content":"a".repeat(50_000)}]}),
        );
        let second = zen_headers(
            "sk-test",
            &json!({"model":"deepseek-v4-flash-free","messages":[{"role":"user","content":"b".repeat(50_000)}]}),
        );
        let third = zen_headers(
            "sk-test",
            &json!({"model":"deepseek-v4-flash-free","messages":[{"role":"user","content":"a".repeat(50_000)}]}),
        );

        assert_ne!(
            header_value(&first, "x-opencode-session"),
            header_value(&second, "x-opencode-session")
        );
        assert_eq!(
            header_value(&first, "x-opencode-session"),
            header_value(&third, "x-opencode-session")
        );
        assert_ne!(header_value(&first, "x-opencode-project"), "global");
    }

    #[test]
    fn sse_parser_accepts_protocol_fields_and_multiline_data() {
        let mut parser = SseParser::default();
        parser.push(b": comment\r\nevent: completion\r\nid: evt_1\r\nretry: 250\r\ndata:first\r\ndata: second\r\n\r\n");

        let frame = parser.next_frame().unwrap().unwrap();
        assert_eq!(frame.event.as_deref(), Some("completion"));
        assert_eq!(frame.id.as_deref(), Some("evt_1"));
        assert_eq!(frame.retry, Some(250));
        assert_eq!(frame.data, "first\nsecond");
        assert!(parser.next_frame().unwrap().is_none());
        assert!(parser.finish().is_ok());
    }

    #[test]
    fn sse_parser_rejects_unterminated_frame() {
        let mut parser = SseParser::default();
        parser.push(b"data: {\"choices\":[]}");

        assert!(parser.next_frame().unwrap().is_none());
        let err = parser.finish().unwrap_err();
        assert!(err.message.contains("stream truncated"));
    }
}
