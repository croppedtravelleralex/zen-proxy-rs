use bytes::BytesMut;
use futures::stream::StreamExt;

use reqwest::header::HeaderMap;
use reqwest::Client;
use serde::Deserialize;
use std::error::Error as StdError;
use std::time::{SystemTime, UNIX_EPOCH};

const UA: &str = "opencode/1.15.5 ai-sdk/provider-utils/4.0.23 runtime/bun/1.3.14";
const DEFAULT_STABLE_SESSION_PREFIX_BYTES: usize = 256 * 1024;
const DEFAULT_MEDIUM_STABLE_SESSION_PREFIX_BYTES: usize = 32 * 1024;
const MIN_STABLE_SESSION_PREFIX_BYTES: usize = 4 * 1024;
const MAX_STABLE_SESSION_PREFIX_BYTES: usize = 1024 * 1024;

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
    pub prompt_cache_hit_tokens: Option<u64>,
    pub prompt_cache_miss_tokens: Option<u64>,
}

impl ZenUsage {
    pub fn prompt_cached_tokens(&self) -> Option<u64> {
        self.prompt_tokens_details
            .as_ref()
            .and_then(|details| details.get("cached_tokens"))
            .and_then(serde_json::Value::as_u64)
            .or(self.prompt_cache_hit_tokens)
    }

    pub fn cache_read_tokens(&self) -> Option<u64> {
        self.cache_read_input_tokens
            .or_else(|| self.prompt_cached_tokens())
    }

    pub fn has_body_cache_usage_signal(&self) -> bool {
        self.cache_creation_input_tokens.is_some()
            || self.cache_read_input_tokens.is_some()
            || self.prompt_cached_tokens().is_some()
            || self.prompt_cache_miss_tokens.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCacheObservationStatus {
    Ignored,
    Attempted,
    Accepted,
    Rejected,
}

impl ProviderCacheObservationStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ignored => "ignored",
            Self::Attempted => "attempted",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProviderCacheSignals {
    pub response_seen: bool,
    pub header_usage_signal: bool,
    pub header_cache_hit: Option<bool>,
    pub header_cache_read_input_tokens: Option<u64>,
    pub header_cache_creation_input_tokens: Option<u64>,
    pub header_cached_tokens: Option<u64>,
    pub body_usage_signal: bool,
    pub body_cache_read_input_tokens: Option<u64>,
    pub body_cache_creation_input_tokens: Option<u64>,
    pub body_cached_tokens: Option<u64>,
    pub body_cache_miss_input_tokens: Option<u64>,
}

impl ProviderCacheSignals {
    pub fn ignored() -> Self {
        Self::default()
    }

    pub fn from_response_headers(headers: &HeaderMap) -> Self {
        let header_cache_hit = parse_header_bool_any(
            headers,
            &[
                "x-provider-cache-hit",
                "x-prompt-cache-hit",
                "x-litellm-cache-hit",
                "x-cache-hit",
                "x-cache",
                "x-cache-status",
                "cf-cache-status",
            ],
        );
        let header_cache_read_input_tokens = parse_header_u64_any(
            headers,
            &[
                "x-cache-read-input-tokens",
                "x-prompt-cache-read-input-tokens",
                "x-prompt-cache-hit-tokens",
                "x-provider-cache-read-input-tokens",
                "x-provider-prompt-cache-hit-tokens",
                "x-litellm-cache-read-input-tokens",
                "x-litellm-prompt-cache-hit-tokens",
            ],
        );
        let header_cache_creation_input_tokens = parse_header_u64_any(
            headers,
            &[
                "x-cache-creation-input-tokens",
                "x-prompt-cache-creation-input-tokens",
                "x-provider-cache-creation-input-tokens",
                "x-litellm-cache-creation-input-tokens",
            ],
        );
        let header_cached_tokens = parse_header_u64_any(
            headers,
            &[
                "x-cached-tokens",
                "x-prompt-cached-tokens",
                "x-provider-cached-tokens",
                "x-litellm-cached-tokens",
            ],
        );
        let header_usage_signal = header_cache_hit.is_some()
            || header_cache_read_input_tokens.is_some()
            || header_cache_creation_input_tokens.is_some()
            || header_cached_tokens.is_some();

        Self {
            response_seen: true,
            header_usage_signal,
            header_cache_hit,
            header_cache_read_input_tokens,
            header_cache_creation_input_tokens,
            header_cached_tokens,
            ..Self::default()
        }
    }

    pub fn with_body_usage(mut self, usage: Option<&ZenUsage>) -> Self {
        let Some(usage) = usage else {
            return self;
        };
        self.body_usage_signal = true;
        self.body_cache_read_input_tokens = usage.cache_read_tokens();
        self.body_cache_creation_input_tokens = usage.cache_creation_input_tokens;
        self.body_cached_tokens = usage.prompt_cached_tokens();
        self.body_cache_miss_input_tokens = usage.prompt_cache_miss_tokens;
        self
    }

    pub fn from_response(headers: &HeaderMap, usage: Option<&ZenUsage>) -> Self {
        Self::from_response_headers(headers).with_body_usage(usage)
    }

    pub fn status(&self) -> ProviderCacheObservationStatus {
        if !self.response_seen {
            return ProviderCacheObservationStatus::Ignored;
        }
        if self.has_positive_cache_signal() {
            return ProviderCacheObservationStatus::Accepted;
        }
        if self.has_explicit_negative_cache_signal() {
            return ProviderCacheObservationStatus::Rejected;
        }
        ProviderCacheObservationStatus::Attempted
    }

    fn has_positive_cache_signal(&self) -> bool {
        self.header_cache_hit == Some(true)
            || is_positive(self.header_cache_read_input_tokens)
            || is_positive(self.header_cache_creation_input_tokens)
            || is_positive(self.header_cached_tokens)
            || is_positive(self.body_cache_read_input_tokens)
            || is_positive(self.body_cache_creation_input_tokens)
            || is_positive(self.body_cached_tokens)
    }

    fn has_explicit_negative_cache_signal(&self) -> bool {
        self.header_cache_hit == Some(false)
            || self.header_usage_signal
                && (is_zero(self.header_cache_read_input_tokens)
                    || is_zero(self.header_cache_creation_input_tokens)
                    || is_zero(self.header_cached_tokens))
            || self.body_usage_signal
                && (is_zero(self.body_cache_read_input_tokens)
                    || is_zero(self.body_cache_creation_input_tokens)
                    || is_zero(self.body_cached_tokens)
                    || is_positive(self.body_cache_miss_input_tokens))
    }
}

fn is_positive(value: Option<u64>) -> bool {
    value.is_some_and(|value| value > 0)
}

fn is_zero(value: Option<u64>) -> bool {
    value == Some(0)
}

fn parse_header_bool_any(headers: &HeaderMap, names: &[&str]) -> Option<bool> {
    names
        .iter()
        .find_map(|name| headers.get(*name).and_then(|value| value.to_str().ok()))
        .and_then(parse_cache_hit_value)
}

fn parse_cache_hit_value(value: &str) -> Option<bool> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "1" | "true" | "yes" | "hit" | "cache-hit" | "cached" => Some(true),
        "0" | "false" | "no" | "miss" | "cache-miss" | "bypass" | "dynamic" | "expired"
        | "stale" => Some(false),
        value if value.contains("hit") && !value.contains("miss") => Some(true),
        value
            if value.contains("miss")
                || value.contains("bypass")
                || value.contains("dynamic")
                || value.contains("expired") =>
        {
            Some(false)
        }
        _ => None,
    }
}

fn parse_header_u64_any(headers: &HeaderMap, names: &[&str]) -> Option<u64> {
    names
        .iter()
        .find_map(|name| headers.get(*name).and_then(|value| value.to_str().ok()))
        .and_then(|value| value.trim().parse::<u64>().ok())
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

fn short_hash(input: &str) -> String {
    short_hash_bytes(input.as_bytes())
}

fn short_hash_bytes(input: &[u8]) -> String {
    format!("{:016x}", stable_hash64(input))
}

fn stable_hash64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn stable_id(prefix: &str, material: &str) -> String {
    let first = stable_hash64(material.as_bytes());
    let second = stable_hash64(format!("{material}\x1frequest").as_bytes());
    let tail = format!("{first:016x}{second:016x}");
    format!("{}_{}", prefix, &tail[..26])
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

fn stable_request_id(body: &serde_json::Value) -> String {
    stable_id("msg", &component_string(Some(body)))
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
    let material = session_material(body);
    let estimated_tokens = material.len() / 4;
    let compacted = material.contains("free-model-client-rs context compactor");
    if compacted || estimated_tokens >= 10_000 {
        let prefix_bytes = stable_session_prefix_bytes(material.len());
        let prefix_len = material.len().min(prefix_bytes);
        let prefix_hash = short_hash_bytes(&material.as_bytes()[..prefix_len]);
        let tools_hash = component_hash(body.get("tools"));
        let tool_choice_hash = component_hash(body.get("tool_choice"));
        return format!(
            "large_prefix_v4106:p{}:{}:tools{}:choice{}",
            prefix_bytes, prefix_hash, tools_hash, tool_choice_hash
        );
    }
    "normal".to_string()
}

fn stable_session_prefix_bytes(material_bytes: usize) -> usize {
    let large_prefix_bytes = env_usize_clamped(
        "ZEN_UPSTREAM_SESSION_PREFIX_BYTES",
        DEFAULT_STABLE_SESSION_PREFIX_BYTES,
        MIN_STABLE_SESSION_PREFIX_BYTES,
        MAX_STABLE_SESSION_PREFIX_BYTES,
    );
    if material_bytes <= large_prefix_bytes {
        return env_usize_clamped(
            "ZEN_UPSTREAM_MEDIUM_SESSION_PREFIX_BYTES",
            DEFAULT_MEDIUM_STABLE_SESSION_PREFIX_BYTES,
            MIN_STABLE_SESSION_PREFIX_BYTES,
            large_prefix_bytes,
        );
    }
    large_prefix_bytes
}

fn env_usize_clamped(name: &str, default: usize, min: usize, max: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

fn session_material(body: &serde_json::Value) -> String {
    let mut material = String::new();
    material.push_str("messages=");
    material.push_str(&component_string(body.get("messages")));
    material
}

fn component_hash(value: Option<&serde_json::Value>) -> String {
    short_hash(&component_string(value))
}

fn component_string(value: Option<&serde_json::Value>) -> String {
    value
        .map(|value| serde_json::to_string(value).unwrap_or_default())
        .unwrap_or_else(|| "null".to_string())
}

pub fn zen_headers(api_key: &str, body: &serde_json::Value) -> Vec<(String, String)> {
    vec![
        ("authorization".into(), format!("Bearer {}", api_key)),
        ("user-agent".into(), UA.into()),
        ("x-opencode-client".into(), "cli".into()),
        ("x-opencode-project".into(), stable_project_id(body)),
        ("x-opencode-request".into(), stable_request_id(body)),
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
                format!("upstream connection error: {}", reqwest_error_summary(&e)),
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

fn reqwest_error_summary(err: &reqwest::Error) -> String {
    let mut parts = Vec::new();
    push_error_summary_part(&mut parts, &err.to_string());
    let mut source = err.source();
    while let Some(error) = source {
        push_error_summary_part(&mut parts, &error.to_string());
        source = error.source();
    }
    parts.join("; caused by: ")
}

fn push_error_summary_part(parts: &mut Vec<String>, text: &str) {
    let redacted = redact_socks_credentials(text);
    if redacted.trim().is_empty() || parts.iter().any(|part| part == &redacted) {
        return;
    }
    parts.push(redacted);
}

fn redact_socks_credentials(input: &str) -> String {
    let mut output = input.to_string();
    for scheme in ["socks5h://", "socks5://"] {
        output = redact_credentials_for_scheme(&output, scheme);
    }
    output
}

fn redact_credentials_for_scheme(input: &str, scheme: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find(scheme) {
        let (before, after_before) = rest.split_at(pos);
        out.push_str(before);
        out.push_str(scheme);
        let after_scheme = &after_before[scheme.len()..];
        let authority_end = after_scheme
            .find(['/', ' ', ')', '(', ',', ';', '"', '\''])
            .unwrap_or(after_scheme.len());
        let authority = &after_scheme[..authority_end];
        if let Some(at_pos) = authority.rfind('@') {
            out.push_str("***@");
            out.push_str(&authority[at_pos + 1..]);
        } else {
            out.push_str(authority);
        }
        rest = &after_scheme[authority_end..];
    }
    out.push_str(rest);
    out
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

    fn test_headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(*name, value.parse().unwrap());
        }
        headers
    }

    #[test]
    fn provider_cache_signals_are_ignored_without_provider_response() {
        let signals = ProviderCacheSignals::ignored();

        assert_eq!(signals.status(), ProviderCacheObservationStatus::Ignored);
        assert!(!signals.response_seen);
    }

    #[test]
    fn provider_cache_signals_are_attempted_when_response_has_no_cache_signal() {
        let usage = ZenUsage {
            prompt_tokens: Some(30),
            completion_tokens: Some(5),
            total_tokens: Some(35),
            prompt_tokens_details: None,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        };

        let signals = ProviderCacheSignals::from_response(&HeaderMap::new(), Some(&usage));

        assert_eq!(signals.status(), ProviderCacheObservationStatus::Attempted);
        assert!(signals.response_seen);
        assert!(signals.body_usage_signal);
        assert!(!signals.header_usage_signal);
    }

    #[test]
    fn provider_cache_signals_accept_body_cached_tokens() {
        let usage = ZenUsage {
            prompt_tokens: Some(30),
            completion_tokens: Some(5),
            total_tokens: Some(35),
            prompt_tokens_details: Some(json!({"cached_tokens": 22})),
            cache_creation_input_tokens: Some(11),
            cache_read_input_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        };

        let signals = ProviderCacheSignals::from_response(&HeaderMap::new(), Some(&usage));

        assert_eq!(signals.status(), ProviderCacheObservationStatus::Accepted);
        assert_eq!(signals.body_cache_creation_input_tokens, Some(11));
        assert_eq!(signals.body_cached_tokens, Some(22));
    }

    #[test]
    fn provider_cache_signals_accept_header_hit_without_body_usage() {
        let headers = test_headers(&[
            ("x-provider-cache-hit", "true"),
            ("x-provider-cached-tokens", "22"),
        ]);

        let signals = ProviderCacheSignals::from_response(&headers, None);

        assert_eq!(signals.status(), ProviderCacheObservationStatus::Accepted);
        assert!(signals.header_usage_signal);
        assert_eq!(signals.header_cache_hit, Some(true));
        assert_eq!(signals.header_cached_tokens, Some(22));
        assert!(!signals.body_usage_signal);
    }

    #[test]
    fn provider_cache_signals_reject_explicit_miss_or_zero_tokens() {
        let headers = test_headers(&[
            ("x-provider-cache-hit", "miss"),
            ("x-provider-cached-tokens", "0"),
        ]);
        let usage = ZenUsage {
            prompt_tokens: Some(30),
            completion_tokens: Some(5),
            total_tokens: Some(35),
            prompt_tokens_details: Some(json!({"cached_tokens": 0})),
            cache_creation_input_tokens: Some(0),
            cache_read_input_tokens: Some(0),
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        };

        let signals = ProviderCacheSignals::from_response(&headers, Some(&usage));

        assert_eq!(signals.status(), ProviderCacheObservationStatus::Rejected);
        assert_eq!(signals.header_cache_hit, Some(false));
        assert_eq!(signals.body_cache_read_input_tokens, Some(0));
    }

    #[test]
    fn provider_cache_signals_accept_deepseek_prompt_cache_hit_tokens() {
        let usage = ZenUsage {
            prompt_tokens: Some(30),
            completion_tokens: Some(5),
            total_tokens: Some(35),
            prompt_tokens_details: None,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
            prompt_cache_hit_tokens: Some(22),
            prompt_cache_miss_tokens: Some(8),
        };

        let signals = ProviderCacheSignals::from_response(&HeaderMap::new(), Some(&usage));

        assert_eq!(usage.cache_read_tokens(), Some(22));
        assert_eq!(signals.status(), ProviderCacheObservationStatus::Accepted);
        assert_eq!(signals.body_cache_read_input_tokens, Some(22));
        assert_eq!(signals.body_cached_tokens, Some(22));
        assert_eq!(signals.body_cache_miss_input_tokens, Some(8));
    }

    #[test]
    fn provider_cache_signals_reject_deepseek_prompt_cache_miss_only() {
        let usage = ZenUsage {
            prompt_tokens: Some(30),
            completion_tokens: Some(5),
            total_tokens: Some(35),
            prompt_tokens_details: None,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
            prompt_cache_hit_tokens: Some(0),
            prompt_cache_miss_tokens: Some(30),
        };

        let signals = ProviderCacheSignals::from_response(&HeaderMap::new(), Some(&usage));

        assert_eq!(usage.cache_read_tokens(), Some(0));
        assert_eq!(signals.status(), ProviderCacheObservationStatus::Rejected);
        assert_eq!(signals.body_cache_read_input_tokens, Some(0));
        assert_eq!(signals.body_cache_miss_input_tokens, Some(30));
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
        assert_eq!(
            header_value(&first, "x-opencode-request"),
            header_value(&second, "x-opencode-request")
        );
    }

    #[test]
    fn opencode_request_changes_when_request_shape_changes() {
        let first = zen_headers(
            "sk-test",
            &json!({"model":"deepseek-v4-flash-free","messages":[{"role":"user","content":"first"}]}),
        );
        let second = zen_headers(
            "sk-test",
            &json!({"model":"deepseek-v4-flash-free","messages":[{"role":"user","content":"second"}]}),
        );

        assert_ne!(
            header_value(&first, "x-opencode-request"),
            header_value(&second, "x-opencode-request")
        );
    }

    #[test]
    fn opencode_request_changes_when_any_upstream_body_field_changes() {
        let first = zen_headers(
            "sk-test",
            &json!({"model":"deepseek-v4-flash-free","messages":[{"role":"user","content":"same"}],"temperature":0.2}),
        );
        let second = zen_headers(
            "sk-test",
            &json!({"model":"deepseek-v4-flash-free","messages":[{"role":"user","content":"same"}],"temperature":0.7}),
        );

        assert_ne!(
            header_value(&first, "x-opencode-request"),
            header_value(&second, "x-opencode-request")
        );
        assert_eq!(
            header_value(&first, "x-opencode-session"),
            header_value(&second, "x-opencode-session")
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
    fn opencode_session_keeps_large_stable_prefix_when_tail_grows() {
        let prefix = "a".repeat(1_200_000);
        let first = zen_headers(
            "sk-test",
            &json!({"model":"deepseek-v4-flash-free","messages":[{"role":"user","content":prefix}]}),
        );
        let second = zen_headers(
            "sk-test",
            &json!({"model":"deepseek-v4-flash-free","messages":[{"role":"user","content":prefix},{"role":"assistant","content":"done"},{"role":"user","content":"continue"}]}),
        );

        assert_eq!(
            header_value(&first, "x-opencode-session"),
            header_value(&second, "x-opencode-session")
        );
    }

    #[test]
    fn opencode_session_keeps_medium_stable_prefix_when_tail_grows() {
        let prefix = "a".repeat(80_000);
        let first = zen_headers(
            "sk-test",
            &json!({"model":"deepseek-v4-flash-free","messages":[{"role":"user","content":prefix}]}),
        );
        let second = zen_headers(
            "sk-test",
            &json!({"model":"deepseek-v4-flash-free","messages":[{"role":"user","content":prefix},{"role":"assistant","content":"done"},{"role":"user","content":"continue"}]}),
        );

        assert_eq!(
            header_value(&first, "x-opencode-session"),
            header_value(&second, "x-opencode-session")
        );
    }

    #[test]
    fn opencode_session_changes_when_medium_prefix_changes() {
        let first = zen_headers(
            "sk-test",
            &json!({"model":"deepseek-v4-flash-free","messages":[{"role":"user","content":"a".repeat(80_000)}]}),
        );
        let second = zen_headers(
            "sk-test",
            &json!({"model":"deepseek-v4-flash-free","messages":[{"role":"user","content":"b".repeat(80_000)}]}),
        );

        assert_ne!(
            header_value(&first, "x-opencode-session"),
            header_value(&second, "x-opencode-session")
        );
    }

    #[test]
    fn redacts_socks_credentials_in_error_summary() {
        let text = "proxy socks5h://user:secret@127.0.0.1:1080 failed";

        let redacted = redact_socks_credentials(text);

        assert_eq!(redacted, "proxy socks5h://***@127.0.0.1:1080 failed");
        assert!(!redacted.contains("user:secret"));
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
