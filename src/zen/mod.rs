use crate::config::Config;
use reqwest::{Client, Response, StatusCode};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
pub(crate) struct ZenRequestBody {
    model: String,
    messages: Vec<Value>,
    stream: bool,
    max_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<Value>,
}

/// Build the Zen upstream request body from an OpenAI chat request.
/// Always sets stream=true since we collect events in both modes.
pub fn build_zen_body(request: &crate::protocol::types::ChatRequest) -> ZenRequestBody {
    ZenRequestBody {
        model: request.model.clone(),
        messages: request
            .messages
            .iter()
            .map(|m| serde_json::to_value(m).unwrap_or_default())
            .collect(),
        stream: true,
        max_tokens: request.max_tokens.unwrap_or(1024).max(32),
        stream_options: Some(serde_json::json!({"include_usage": true})),
        temperature: request.temperature,
        top_p: request.top_p,
        tools: request.tools.as_ref().map(|tools| {
            tools
                .iter()
                .map(|t| serde_json::to_value(t).unwrap_or_default())
                .collect()
        }),
        tool_choice: request.tool_choice.clone(),
    }
}

/// Make a POST request to the Zen chat completions endpoint.
pub async fn fetch_zen(
    client: &Client,
    config: &Config,
    body: &crate::protocol::types::ChatRequest,
) -> Result<(Response, String), crate::error::AppError> {
    let zen_body = build_zen_body(body);
    let url = &config.zen_chat_url;

    let response = client
        .post(url)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", config.zen_api_key))
        .header(
            "user-agent",
            "opencode/1.15.5 ai-sdk/provider-utils/4.0.23 runtime/bun/1.3.14",
        )
        .header("x-opencode-client", "cli")
        .header("x-opencode-project", "global")
        .header("x-opencode-request", random_opencode_id("msg"))
        .header("x-opencode-session", random_opencode_id("ses"))
        .json(&zen_body)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                crate::error::AppError::new(
                    StatusCode::GATEWAY_TIMEOUT,
                    format!("opencode zen timeout: {e}"),
                )
            } else if e.is_connect() {
                crate::error::AppError::new(
                    StatusCode::BAD_GATEWAY,
                    format!("opencode zen connection error: {e}"),
                )
            } else {
                crate::error::AppError::new(
                    StatusCode::BAD_GATEWAY,
                    format!("opencode zen request failed: {e}"),
                )
            }
        })?;

    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    if !status.is_success() {
        let text_body = response.text().await.unwrap_or_default();
        let short = text_body.chars().take(300).collect::<String>();
        return Err(crate::error::AppError::upstream(
            status.as_u16(),
            short,
            retry_after,
        ));
    }

    Ok((response, retry_after.unwrap_or_default()))
}

fn random_opencode_id(prefix: &str) -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut rng = rand::thread_rng();
    let tail: String = (0..26)
        .map(|_| {
            let idx = rng.gen_range(0..ALPHABET.len());
            ALPHABET[idx] as char
        })
        .collect();
    format!("{}_{}", prefix, tail)
}
