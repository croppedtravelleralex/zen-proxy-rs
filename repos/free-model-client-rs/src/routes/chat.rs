use crate::auth;
use crate::client_profile::ClientProfile;
use crate::error::AppError;
use crate::kernel::FreeModelKernel;
use crate::protocol::translate;
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
    if !auth::is_authorized(&state.config, auth::request_api_key(&headers)) {
        return AppError::auth_error().into_response();
    }
    let req: ChatRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => return AppError::invalid_json(e.to_string()).into_response(),
    };
    if req.messages.is_empty() {
        return AppError::empty_messages().into_response();
    }
    let nm = translate::normalize_model(&req.model);
    let model_mappings = state.model_mappings().await;
    if !model_mappings
        .iter()
        .any(|mapping| mapping.public_name == nm)
    {
        return AppError::invalid_model(req.model).into_response();
    }
    let kernel = FreeModelKernel::from_config_and_mappings(&state.config, &model_mappings);
    let profile = ClientProfile::from_openai(&headers, &req);
    match kernel
        .openai_chat_with_profile(&state.http_client, req, profile)
        .await
    {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}

pub async fn messages_handler(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: String,
) -> Response {
    if !auth::is_authorized(&state.config, auth::request_api_key(&headers)) {
        return AppError::auth_error().into_response();
    }
    let req: AnthropicRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => return AppError::invalid_json(e.to_string()).into_response(),
    };
    if req.messages.is_empty() {
        return AppError::empty_messages().into_response();
    }
    let nm = translate::normalize_model(&req.model);
    let model_mappings = state.model_mappings().await;
    if !model_mappings
        .iter()
        .any(|mapping| mapping.public_name == nm)
    {
        return AppError::invalid_model(req.model).into_response();
    }
    let kernel = FreeModelKernel::from_config_and_mappings(&state.config, &model_mappings);
    let profile = ClientProfile::from_anthropic(&headers, &req);
    match kernel
        .anthropic_messages_with_profile(&state.http_client, req, profile)
        .await
    {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}
