use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug)]
pub struct AppError {
    pub status: StatusCode,
    pub message: String,
    pub upstream_headers: Option<Vec<(String, String)>>,
    pub upstream_error_kind: Option<UpstreamErrorKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamErrorKind {
    MissingReasoningContent,
    ThinkingToolChoiceUnsupported,
    RateLimited,
    ProviderInvalidRequest,
    ProviderError,
}

impl UpstreamErrorKind {
    pub const fn public_code(self) -> &'static str {
        match self {
            Self::MissingReasoningContent => "provider_missing_reasoning_content",
            Self::ThinkingToolChoiceUnsupported => "provider_thinking_tool_choice_unsupported",
            Self::RateLimited => "provider_rate_limited",
            Self::ProviderInvalidRequest => "provider_invalid_request",
            Self::ProviderError => "provider_error",
        }
    }
}

impl AppError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            upstream_headers: None,
            upstream_error_kind: None,
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
        let upstream_error_kind = classify_upstream_error(status, &body_text);
        Self {
            status: code,
            message: public_upstream_message(status, &body_text, upstream_error_kind),
            upstream_headers: Some(headers),
            upstream_error_kind: Some(upstream_error_kind),
        }
    }

    pub fn is_missing_reasoning_content(&self) -> bool {
        self.upstream_error_kind == Some(UpstreamErrorKind::MissingReasoningContent)
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
        let error_type = if self.status == StatusCode::TOO_MANY_REQUESTS {
            "rate_limit_error"
        } else if self.upstream_error_kind.is_some() {
            "upstream_provider_error"
        } else {
            "api_error"
        };
        let mut error = json!({
            "type": error_type,
            "message": self.message,
        });
        if let Some(kind) = self.upstream_error_kind {
            error["code"] = json!(kind.public_code());
        }
        let body = json!({
            "error": error
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

fn classify_upstream_error(status: u16, body_text: &str) -> UpstreamErrorKind {
    let lower = body_text.to_ascii_lowercase();
    if status == StatusCode::TOO_MANY_REQUESTS.as_u16() {
        return UpstreamErrorKind::RateLimited;
    }
    if lower.contains("reasoning_content") && lower.contains("must be passed back") {
        return UpstreamErrorKind::MissingReasoningContent;
    }
    if lower.contains("thinking mode does not support this tool_choice") {
        return UpstreamErrorKind::ThinkingToolChoiceUnsupported;
    }
    if (400..500).contains(&status) {
        return UpstreamErrorKind::ProviderInvalidRequest;
    }
    UpstreamErrorKind::ProviderError
}

fn public_upstream_message(
    status: u16,
    body_text: &str,
    upstream_error_kind: UpstreamErrorKind,
) -> String {
    match upstream_error_kind {
        UpstreamErrorKind::MissingReasoningContent => {
            "upstream provider rejected transformed tool-history request (code=provider_missing_reasoning_content)".to_string()
        }
        UpstreamErrorKind::ThinkingToolChoiceUnsupported => {
            "upstream provider rejected thinking with forced tool choice (code=provider_thinking_tool_choice_unsupported)".to_string()
        }
        UpstreamErrorKind::RateLimited => "upstream provider rate limited the request".to_string(),
        UpstreamErrorKind::ProviderInvalidRequest | UpstreamErrorKind::ProviderError => {
            if let Some(code) = provider_error_code(body_text) {
                format!("upstream provider error (status={status}, code={code})")
            } else {
                format!("upstream provider error (status={status})")
            }
        }
    }
}

fn provider_error_code(body_text: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(body_text).ok()?;
    parsed
        .get("error")
        .and_then(|error| error.get("code").or_else(|| error.get("type")))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
}
