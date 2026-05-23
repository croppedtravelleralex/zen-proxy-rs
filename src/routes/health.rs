use axum::http::StatusCode;
use axum::response::IntoResponse;

/// GET /health — always returns 200 OK.
/// OPTIONS preflight for CORS is handled by the CORS layer.
pub async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, "OK")
}
