use crate::auth;
use crate::error::AppError;
use crate::kernel::FreeModelKernel;
use crate::protocol::types::{AnthropicRequest, ChatRequest};
use crate::routes::AppState;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

pub async fn chat_handler(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: String,
) -> Response {
    let ah = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if !auth::is_authorized(&state.config, ah) {
        return AppError::auth_error().into_response();
    }
    let req: ChatRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => return AppError::invalid_json(e.to_string()).into_response(),
    };
    if req.messages.is_empty() {
        return AppError::empty_messages().into_response();
    }
    let nm = req.model.strip_prefix("opencode/").unwrap_or(&req.model);
    if !state.config.free_models.iter().any(|m| m == nm) {
        return AppError::invalid_model(req.model).into_response();
    }
    let kernel = FreeModelKernel::from_config(&state.config);
    match kernel.openai_chat(&state.http_client, req).await {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}

pub async fn messages_handler(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: String,
) -> Response {
    let ah = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let xkey = headers.get("x-api-key").and_then(|v| v.to_str().ok());
    let key_to_check = ah.or(xkey);
    if !auth::is_authorized(&state.config, key_to_check) {
        return AppError::auth_error().into_response();
    }
    let req: AnthropicRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => return AppError::invalid_json(e.to_string()).into_response(),
    };
    if req.messages.is_empty() {
        return AppError::empty_messages().into_response();
    }
    let nm = req.model.strip_prefix("opencode/").unwrap_or(&req.model);
    if !state.config.free_models.iter().any(|m| m == nm) {
        return AppError::invalid_model(req.model).into_response();
    }
    let kernel = FreeModelKernel::from_config(&state.config);
    match kernel.anthropic_messages(&state.http_client, req).await {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}
