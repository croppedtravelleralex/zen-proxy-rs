use crate::auth;
use crate::routes::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::sync::Arc;

/// GET /v1/models and GET /models — returns the free-model list.
///
/// Auth is required unless `require_api_key` is false in config.
pub async fn models_handler(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Response {
    let auth_header = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    if !auth::is_authorized(&state.config, auth_header) {
        return crate::error::AppError::AuthError.into_response();
    }

    let models: Vec<serde_json::Value> = state
        .config
        .free_models
        .iter()
        .map(|id| {
            serde_json::json!({
                "id": id,
                "object": "model",
                "created": 0,
                "owned_by": "opencode"
            })
        })
        .collect();

    let body = serde_json::json!({
        "object": "list",
        "data": models
    });

    (StatusCode::OK, Json(body)).into_response()
}
