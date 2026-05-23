use crate::auth;
use crate::error::AppError;
use crate::protocol::types::{AnthropicRequest, ChatRequest};
use crate::routes::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::sync::Arc;

/// POST /v1/chat/completions and /chat/completions
///
/// Handles OpenAI-format chat completions.
pub async fn chat_handler(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: String,
) -> Response {
    // --- auth check ---
    let auth_header = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if !auth::is_authorized(&state.config, auth_header) {
        return AppError::auth_error().into_response();
    }

    // --- parse JSON ---
    let request: ChatRequest = match serde_json::from_str(&body) {
        Ok(req) => req,
        Err(e) => return AppError::invalid_json(e.to_string()).into_response(),
    };

    // --- validate messages ---
    if request.messages.is_empty() {
        return AppError::empty_messages().into_response();
    }

    // --- normalize & validate model ---
    let normalized = normalize_model(&request.model);
    if !state.config.free_models.iter().any(|m| m == normalized) {
        return AppError::invalid_model(request.model).into_response();
    }

    // TODO: forward to zen-proxy (OpenAI-format)
    // For now, return a placeholder indicating the model is valid.
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "id": "chatcmpl-placeholder",
            "object": "chat.completion",
            "created": 0,
            "model": normalized,
            "choices": []
        })),
    )
        .into_response()
}

/// POST /v1/messages and /messages
///
/// Handles Anthropic-format messages endpoint.
pub async fn messages_handler(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: String,
) -> Response {
    // --- auth check ---
    let auth_header = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if !auth::is_authorized(&state.config, auth_header) {
        return AppError::auth_error().into_response();
    }

    // --- parse JSON ---
    let request: AnthropicRequest = match serde_json::from_str(&body) {
        Ok(req) => req,
        Err(e) => return AppError::invalid_json(e.to_string()).into_response(),
    };

    // --- validate messages ---
    if request.messages.is_empty() {
        return AppError::empty_messages().into_response();
    }

    // --- normalize & validate model ---
    let normalized = normalize_model(&request.model);
    if !state.config.free_models.iter().any(|m| m == normalized) {
        return AppError::invalid_model(request.model).into_response();
    }

    // TODO: forward to Anthropic proxy
    // For now, return a placeholder indicating the model is valid.
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "id": "msg-placeholder",
            "type": "message",
            "role": "assistant",
            "model": normalized,
            "content": [],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 0, "output_tokens": 0}
        })),
    )
        .into_response()
}

/// Strip the "opencode/" prefix from model names.
fn normalize_model(name: &str) -> &str {
    name.strip_prefix("opencode/").unwrap_or(name)
}
