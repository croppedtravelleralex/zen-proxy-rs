use crate::config::Config;
use crate::error::AppError;
use crate::protocol::types::{ChatRequest, OpenAIResponse, OpenAIChoice, OpenAIResponseMessage, Usage, ToolCall};
use crate::proxy::sse;
use crate::routes::AppState;
use crate::synthesis;
use crate::zen;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures::StreamExt;
use reqwest::Client;
use serde_json::Value;
use std::sync::Arc;

pub async fn handle_openai_chat(
    state: &AppState,
    body: ChatRequest,
) -> Result<Response, AppError> {
    if body.stream.unwrap_or(false) {
        handle_stream(state, body).await
    } else {
        handle_non_stream(state, body).await
    }
}

async fn handle_non_stream(state: &AppState, body: ChatRequest) -> Result<Response, AppError> {
    let client = &state.http_client;
    let config = &state.config;
    let model = body.model.clone();

    match zen::fetch_zen(client, config, &body).await {
        Ok((response, _retry_after)) => {
            let collected = collect_stream_events(response).await?;
            let (content, reasoning, usage) = collected;

            if content.trim().is_empty() && !reasoning.trim().is_empty() {
                return build_fallback_response(&model, &body, usage);
            }

            let text = content.trim().to_string();
            let id = chatcmpl_id();

            let response_body = OpenAIResponse {
                id,
                object: "chat.completion".to_string(),
                created: unix_ts(),
                model: model.clone(),
                choices: vec![OpenAIChoice {
                    index: 0,
                    message: OpenAIResponseMessage {
                        role: "assistant".to_string(),
                        content: if text.is_empty() { None } else { Some(text) },
                        tool_calls: None,
                    },
                    finish_reason: "stop".to_string(),
                }],
                usage: usage.unwrap_or(Usage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                }),
                cost: "0".to_string(),
            };

            Ok((StatusCode::OK, axum::Json(response_body)).into_response())
        }
        Err(e) => {
            if e.message.contains("reasoning_content without final content")
                || e.message.contains("reasoning_content")
            {
                return build_fallback_response(&model, &body, None);
            }
            Err(e)
        }
    }
}

async fn handle_stream(state: &AppState, body: ChatRequest) -> Result<Response, AppError> {
    let client = &state.http_client;
    let config = &state.config;
    let model = body.model.clone();

    let (zen_response, _retry_after) = zen::fetch_zen(client, config, &body).await?;
    let id = chatcmpl_id();
    let model_clone = model.clone();

    let stream = zen_response
        .bytes_stream()
        .map(move |result| {
            let chunk = match result {
                Ok(bytes) => process_stream_chunk(&id, &model_clone, &bytes),
                Err(_e) => return Ok::<Bytes, axum::Error>(sse::openai_sse_done().into()),
            };
            match chunk {
                Ok(data) => Ok(data),
                Err(_e) => Ok(sse::openai_sse_done().into()),
            }
        })
        .chain(futures::stream::once(async {
            Ok::<Bytes, axum::Error>(sse::openai_sse_done().into())
        }));

    let response = axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream; charset=utf-8")
        .header("cache-control", "no-cache")
        .header("access-control-allow-origin", "*")
        .body(axum::body::Body::from_stream(stream))
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(response)
}

fn process_stream_chunk(id: &str, model: &str, raw: &[u8]) -> Result<Bytes, serde_json::Error> {
    let text = String::from_utf8_lossy(raw);
    let mut output = String::new();

    for part in text.split("\n\n") {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        for line in trimmed.lines() {
            let data = match line.strip_prefix("data: ") {
                Some(d) => d,
                None => {
                    if line == "data:" { "" } else { continue }
                }
            };
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let event: Value = serde_json::from_str(data)?;
            let choice = event["choices"].get(0);

            let delta_content = choice
                .and_then(|c| c["delta"]["content"].as_str())
                .filter(|s| !s.is_empty());

            let finish_reason = choice
                .and_then(|c| c["finish_reason"].as_str());

            let usage = event.get("usage");

            let chunk_str = if finish_reason.is_some() || usage.is_some() {
                let usage_obj = usage.map(|u| {
                    Usage {
                        prompt_tokens: u["prompt_tokens"].as_u64().unwrap_or(0),
                        completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0),
                        total_tokens: u["total_tokens"].as_u64().unwrap_or(0),
                    }
                });
                sse::openai_sse_chunk(
                    id, model, delta_content, None,
                    Some(finish_reason.unwrap_or("stop")), usage_obj.as_ref(),
                )
            } else if let Some(content) = delta_content {
                sse::openai_sse_chunk(id, model, Some(content), None, None, None)
            } else {
                continue
            };
            output.push_str(&chunk_str);
        }
    }

    Ok(Bytes::from(output))
}

async fn collect_stream_events(
    response: reqwest::Response,
) -> Result<(String, String, Option<Usage>), AppError> {
    let mut stream = response.bytes_stream();
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut usage: Option<Usage> = None;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|e| AppError::new(StatusCode::BAD_GATEWAY, format!("stream read error: {e}")))?;
        let text = String::from_utf8_lossy(&chunk);

        for part in text.split("\n\n") {
            let trimmed = part.trim();
            if trimmed.is_empty() { continue; }
            for line in trimmed.lines() {
                let data = match line.strip_prefix("data: ") {
                    Some(d) => d,
                    None => {
                        if line == "data:" { "" } else { continue }
                    }
                };
                if data.is_empty() || data == "[DONE]" { continue; }
                if let Ok(event) = serde_json::from_str::<Value>(data) {
                    if let Some(c) = event["choices"][0]["delta"]["content"].as_str() {
                        content.push_str(c);
                    }
                    if let Some(r) = event["choices"][0]["delta"]["reasoning_content"].as_str() {
                        reasoning.push_str(r);
                    }
                    if let Some(u) = event.get("usage") {
                        usage = Some(Usage {
                            prompt_tokens: u["prompt_tokens"].as_u64().unwrap_or(0),
                            completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0),
                            total_tokens: u["total_tokens"].as_u64().unwrap_or(0),
                        });
                    }
                }
            }
        }
    }

    Ok((content, reasoning, usage))
}

fn build_fallback_response(
    model: &str,
    body: &ChatRequest,
    usage: Option<Usage>,
) -> Result<Response, AppError> {
    let has_tools = body.tools.as_ref().map_or(false, |t| !t.is_empty());

    if has_tools {
        if let Some(tool_call) = synthesis::synthesize_tool_call(body) {
            let response_body = OpenAIResponse {
                id: chatcmpl_id(),
                object: "chat.completion".to_string(),
                created: unix_ts(),
                model: model.to_string(),
                choices: vec![OpenAIChoice {
                    index: 0,
                    message: OpenAIResponseMessage {
                        role: "assistant".to_string(),
                        content: None,
                        tool_calls: Some(vec![tool_call]),
                    },
                    finish_reason: "tool_calls".to_string(),
                }],
                usage: usage.unwrap_or(Usage { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 }),
                cost: "0".to_string(),
            };
            return Ok((StatusCode::OK, axum::Json(response_body)).into_response());
        }
    }

    let fallback_text = synthesis::synthesize_text_fallback(body);
    let response_body = OpenAIResponse {
        id: chatcmpl_id(),
        object: "chat.completion".to_string(),
        created: unix_ts(),
        model: model.to_string(),
        choices: vec![OpenAIChoice {
            index: 0,
            message: OpenAIResponseMessage {
                role: "assistant".to_string(),
                content: Some(fallback_text),
                tool_calls: None,
            },
            finish_reason: "stop".to_string(),
        }],
        usage: usage.unwrap_or(Usage { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 }),
        cost: "0".to_string(),
    };

    Ok((StatusCode::OK, axum::Json(response_body)).into_response())
}

fn chatcmpl_id() -> String {
    format!("chatcmpl_{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis())
}

fn unix_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
