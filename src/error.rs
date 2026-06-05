use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug)]
pub struct AppError {
    pub status: StatusCode,
    pub message: String,
    pub upstream_headers: Option<Vec<(String, String)>>,
}

impl AppError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            upstream_headers: None,
        }
    }

    pub fn auth_error() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "invalid API key")
    }

    pub fn invalid_json(detail: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            format!("request body must be valid JSON: {detail}"),
        )
    }

    pub fn empty_messages() -> Self {
        Self::new(StatusCode::BAD_REQUEST, "messages array must not be empty")
    }

    pub fn invalid_model(model: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            format!("unsupported free model: {model}"),
        )
    }

    pub fn empty_upstream() -> Self {
        Self::new(
            StatusCode::BAD_GATEWAY,
            "upstream returned no assistant content or tool call",
        )
    }

    pub fn empty_upstream_class(class: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::BAD_GATEWAY,
            format!("upstream returned no assistant content or tool call (class={class})"),
        )
    }

    pub fn upstream(status: u16, body_text: String, retry_after: Option<String>) -> Self {
        let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
        let mut headers = Vec::new();
        if let Some(ra) = retry_after {
            headers.push(("retry-after".to_string(), ra));
        }
        Self {
            status: code,
            message: format!("opencode zen {status}: {body_text}"),
            upstream_headers: Some(headers),
        }
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.status.as_u16(), self.message)
    }
}

impl std::error::Error for AppError {}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = json!({
            "error": {
                "type": if self.status == StatusCode::TOO_MANY_REQUESTS {
                    "rate_limit_error"
                } else {
                    "api_error"
                },
                "message": self.message,
            }
        });
        let mut response = (self.status, Json(body)).into_response();
        if let Some(headers) = self.upstream_headers {
            for (key, value) in headers {
                if let (Ok(name), Ok(val)) = (
                    axum::http::HeaderName::from_bytes(key.as_bytes()),
                    axum::http::HeaderValue::from_str(&value),
                ) {
                    response.headers_mut().insert(name, val);
                }
            }
        }
        response
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}
