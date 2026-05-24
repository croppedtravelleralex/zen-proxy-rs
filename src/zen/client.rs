use bytes::BytesMut;
use futures::stream::StreamExt;
use rand::Rng;

use reqwest::Client;
use serde::Deserialize;

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

#[derive(Debug, Deserialize)]
pub struct ZenUsage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
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

pub fn zen_headers(api_key: &str) -> Vec<(String, String)> {
    vec![
        ("authorization".into(), format!("Bearer {}", api_key)),
        ("user-agent".into(), UA.into()),
        ("x-opencode-client".into(), "cli".into()),
        ("x-opencode-project".into(), "global".into()),
        ("x-opencode-request".into(), make_id("msg")),
        ("x-opencode-session".into(), make_id("ses")),
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
    for (k, v) in zen_headers(api_key) {
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
    let mut stream = resp.bytes_stream();
    let mut buffer = BytesMut::new();
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut usage = None;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            crate::error::AppError::new(
                axum::http::StatusCode::BAD_GATEWAY,
                format!("stream error: {e}"),
            )
        })?;
        buffer.extend_from_slice(&chunk);
        while let Some(pos) = buffer.windows(2).position(|w| w == b"\n\n") {
            let event_bytes = buffer.split_to(pos);
            let _ = buffer.split_to(2); // consume the \n\n
            let s = String::from_utf8_lossy(&event_bytes);
            for line in s.lines() {
                if let Some(data) = line.strip_prefix("data: ") {
                    if data == "[DONE]" {
                        continue;
                    }
                    let event = serde_json::from_str::<ZenSseEvent>(data).map_err(|e| {
                        crate::error::AppError::new(
                            axum::http::StatusCode::BAD_GATEWAY,
                            format!("stream parse error: {e}"),
                        )
                    })?;
                    usage = event.usage.or(usage);
                    if let Some(choices) = event.choices {
                        for choice in choices {
                            if let Some(delta) = choice.delta {
                                if let Some(c) = delta.content {
                                    content.push_str(&c);
                                }
                                if let Some(r) = delta.reasoning_content {
                                    reasoning.push_str(&r);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok((content, reasoning, usage))
}

pub fn stream_sse_events(
    byte_stream: impl futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
) -> impl futures::Stream<Item = Result<ZenSseEvent, crate::error::AppError>> {
    use futures::StreamExt;
    async_stream::stream! {
        let mut byte_stream = Box::pin(byte_stream);
        let mut buffer = BytesMut::new();
        loop {
            match byte_stream.next().await {
                Some(Ok(chunk)) => {
                    buffer.extend_from_slice(&chunk);
                    while let Some(pos) = buffer.windows(2).position(|w| w == b"

") {
                        let event_bytes = buffer.split_to(pos);
                        let _ = buffer.split_to(2);
                        let s = String::from_utf8_lossy(&event_bytes);
                        for line in s.lines() {
                            if let Some(data) = line.strip_prefix("data: ") {
                                if data == "[DONE]" { continue; }
                                match serde_json::from_str::<ZenSseEvent>(data) {
                                    Ok(event) => yield Ok(event),
                                    Err(e) => {
                                        yield Err(crate::error::AppError::new(
                                            axum::http::StatusCode::BAD_GATEWAY,
                                            format!("stream parse error: {e}"),
                                        ));
                                        return;
                                    }
                                }
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
                    // Process remaining buffer
                    if !buffer.is_empty() {
                        let s = String::from_utf8_lossy(&buffer);
                        for line in s.lines() {
                            if let Some(data) = line.strip_prefix("data: ") {
                                if data == "[DONE]" { continue; }
                                match serde_json::from_str::<ZenSseEvent>(data) {
                                    Ok(event) => yield Ok(event),
                                    Err(e) => {
                                        yield Err(crate::error::AppError::new(
                                            axum::http::StatusCode::BAD_GATEWAY,
                                            format!("stream parse error: {e}"),
                                        ));
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    return;
                }
            }
        }
    }
}
