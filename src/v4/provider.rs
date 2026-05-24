use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{to_bytes, Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use free_model_client_rs::error::AppError;
use free_model_client_rs::kernel::{FreeModelKernel, KernelConfig};
use free_model_client_rs::protocol::types::{AnthropicRequest, ChatRequest};
use serde_json::Value;

use crate::collector::RequestTelemetry;
use crate::config::Config;
use crate::ledger::LedgerEvent;
use crate::pool::{DispatchError, ErrorKind, RequestMeta, ResultKind};
use crate::state::AppState;
use crate::v4::model::{ModelError, ModelRegistry, StaticModelRegistry};

pub async fn handle_v4_proxy(
    state: &Arc<AppState>,
    path: &str,
    method: &Method,
    _headers: &HeaderMap,
    body: Bytes,
    client_id: &str,
    start: Instant,
) -> Response {
    if method != Method::POST {
        return error_response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
    }

    let conf = state.config.read().unwrap().clone();
    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(err) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!("request body must be valid JSON: {err}"),
            );
        }
    };
    let streaming = parsed
        .get("stream")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let public_model = parsed
        .get("model")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let registry = StaticModelRegistry;
    let resolved = match registry.resolve(&public_model) {
        Ok(resolved) => resolved,
        Err(ModelError::UnknownModel(model)) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!("unsupported V4 model: {model}"),
            );
        }
    };

    let mut upstream_body = parsed;
    upstream_body["model"] = Value::String(resolved.upstream_model.clone());
    let request_meta = RequestMeta {
        model: public_model.clone(),
        stream: streaming,
        body_size: body.len() as u64,
    };

    match call_with_retry(
        state,
        path,
        &conf,
        request_meta,
        upstream_body,
        &public_model,
        &resolved.upstream_model,
    )
    .await
    {
        Ok(result) => {
            let latency = start.elapsed().as_millis() as u64;
            let status = result.response.status().as_u16();
            state.collector.record_request(&RequestTelemetry {
                rid: result.request_id.clone(),
                ts: chrono::Utc::now().timestamp_millis(),
                model: public_model.clone(),
                public_model: public_model.clone(),
                upstream_model: result.upstream_model,
                protocol: if path == "messages" {
                    "anthropic_messages".to_string()
                } else {
                    "openai_chat_completions".to_string()
                },
                client_id: client_id.to_string(),
                path: path.to_string(),
                method: method.to_string(),
                is_streaming: streaming,
                node_url: result.node_url_redacted.clone(),
                selected_node_id: result.selected_node_id,
                selected_node_url_redacted: result.node_url_redacted.clone(),
                observed_exit_ip: result.observed_exit_ip.clone().unwrap_or_default(),
                outcome: result.outcome,
                pool: "dispatch".to_string(),
                exit_ip: result.observed_exit_ip.unwrap_or_default(),
                status,
                rate_limited: result.was_rate_limited,
                retry_count: result.retry_count,
                latency_total_ms: latency,
                upstream_ms: result.upstream_ms,
                ttft_ms: result.ttft_ms.unwrap_or_default(),
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                bytes_sent: body.len() as u64,
                bytes_received: result.body_bytes_len,
            });
            state.upstream_health.record(status);
            result.response
        }
        Err(err) => {
            state.upstream_health.record(err.status.as_u16());
            if let Some(rid) = err.request_id.as_ref() {
                let latency = start.elapsed().as_millis() as u64;
                state.collector.record_request(&RequestTelemetry {
                    rid: rid.clone(),
                    ts: chrono::Utc::now().timestamp_millis(),
                    model: public_model.clone(),
                    public_model: public_model.clone(),
                    upstream_model: err.upstream_model.clone(),
                    protocol: if path == "messages" {
                        "anthropic_messages".to_string()
                    } else {
                        "openai_chat_completions".to_string()
                    },
                    client_id: client_id.to_string(),
                    path: path.to_string(),
                    method: method.to_string(),
                    is_streaming: streaming,
                    node_url: err.node_url_redacted.clone().unwrap_or_default(),
                    selected_node_id: err.selected_node_id.clone().unwrap_or_default(),
                    selected_node_url_redacted: err.node_url_redacted.clone().unwrap_or_default(),
                    observed_exit_ip: String::new(),
                    outcome: err.outcome.clone(),
                    pool: "dispatch".to_string(),
                    exit_ip: String::new(),
                    status: err.status.as_u16(),
                    rate_limited: err.was_rate_limited,
                    retry_count: err.retry_count,
                    latency_total_ms: latency,
                    upstream_ms: err.upstream_ms,
                    ttft_ms: 0,
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                    bytes_sent: body.len() as u64,
                    bytes_received: 0,
                });
            }
            let mut response = error_response(err.status, err.message);
            if let Some(retry_after) = err.retry_after_secs {
                response.headers_mut().insert(
                    "retry-after",
                    HeaderValue::from_str(&retry_after.to_string()).unwrap(),
                );
            }
            response
        }
    }
}

struct V4CallResult {
    response: Response,
    request_id: String,
    selected_node_id: String,
    node_url_redacted: String,
    observed_exit_ip: Option<String>,
    upstream_model: String,
    outcome: String,
    retry_count: u32,
    was_rate_limited: bool,
    upstream_ms: u64,
    ttft_ms: Option<u64>,
    body_bytes_len: u64,
}

struct V4CallError {
    status: StatusCode,
    message: String,
    retry_after_secs: Option<u64>,
    request_id: Option<String>,
    selected_node_id: Option<String>,
    node_url_redacted: Option<String>,
    upstream_model: String,
    outcome: String,
    retry_count: u32,
    was_rate_limited: bool,
    upstream_ms: u64,
}

impl V4CallError {
    fn before_dispatch(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            retry_after_secs: None,
            request_id: None,
            selected_node_id: None,
            node_url_redacted: None,
            upstream_model: String::new(),
            outcome: "error".to_string(),
            retry_count: 0,
            was_rate_limited: false,
            upstream_ms: 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn after_dispatch(
        status: StatusCode,
        message: impl Into<String>,
        retry_after_secs: Option<u64>,
        request_id: String,
        node_id: String,
        node_url: &str,
        upstream_model: &str,
        outcome: &str,
        retry_count: u32,
        was_rate_limited: bool,
        upstream_ms: u64,
    ) -> Self {
        Self {
            status,
            message: message.into(),
            retry_after_secs,
            request_id: Some(request_id),
            selected_node_id: Some(node_id),
            node_url_redacted: Some(LedgerEvent::redact_node_url(node_url)),
            upstream_model: upstream_model.to_string(),
            outcome: outcome.to_string(),
            retry_count,
            was_rate_limited,
            upstream_ms,
        }
    }
}

async fn call_with_retry(
    state: &Arc<AppState>,
    path: &str,
    conf: &Config,
    request_meta: RequestMeta,
    upstream_body: Value,
    public_model: &str,
    upstream_model: &str,
) -> Result<V4CallResult, V4CallError> {
    let max = conf.pool_max_retries;
    let mut last_status = StatusCode::BAD_GATEWAY;
    let mut last_node_id = String::new();
    let mut was_rate_limited = false;

    for attempt in 0..=max {
        let dispatch_result = if attempt == 0 {
            dispatch_or_wait(state, &request_meta, attempt, max).await?
        } else {
            match state
                .pool_manager
                .dispatch_sticky(&request_meta, &last_node_id)
            {
                Ok(result) => result,
                Err(_) => dispatch_or_wait(state, &request_meta, attempt, max).await?,
            }
        };

        let node_id = dispatch_result.node.id.clone();
        let node_url = dispatch_result.url.clone();
        last_node_id = node_id.clone();
        let request_id = uuid::Uuid::new_v4().to_string();
        let kernel = FreeModelKernel::new(KernelConfig {
            zen_chat_url: conf.chat_url(),
            zen_api_key: conf.upstream_api_key.clone(),
            extra_headers: vec![
                ("x-zen-proxy-selected-node-id".to_string(), node_id.clone()),
                (
                    "x-zen-proxy-selected-node-url".to_string(),
                    LedgerEvent::redact_node_url(&node_url),
                ),
            ],
            model_mappings: conf
                .model_mapping
                .iter()
                .map(|(public, upstream)| (public.clone(), upstream.clone()))
                .collect(),
        });
        let call_start = Instant::now();
        let response = match path {
            "chat/completions" => {
                let request = serde_json::from_value::<ChatRequest>(upstream_body.clone())
                    .map_err(|err| {
                        V4CallError::before_dispatch(
                            StatusCode::BAD_REQUEST,
                            format!("invalid OpenAI chat request: {err}"),
                        )
                    })?;
                kernel.openai_chat(&dispatch_result.client, request).await
            }
            "messages" => {
                let request = serde_json::from_value::<AnthropicRequest>(upstream_body.clone())
                    .map_err(|err| {
                        V4CallError::before_dispatch(
                            StatusCode::BAD_REQUEST,
                            format!("invalid Anthropic messages request: {err}"),
                        )
                    })?;
                kernel
                    .anthropic_messages(&dispatch_result.client, request)
                    .await
            }
            _ => {
                return Err(V4CallError::before_dispatch(
                    StatusCode::NOT_FOUND,
                    format!("unsupported V4 path: {path}"),
                ))
            }
        };
        let latency = call_start.elapsed().as_millis() as u64;

        match response {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    let observed_exit_ip = response
                        .headers()
                        .get("x-zen-observed-exit-ip")
                        .and_then(|value| value.to_str().ok())
                        .map(ToOwned::to_owned);
                    state.pool_manager.report(
                        node_id.clone(),
                        ResultKind::Success(status.as_u16()),
                        latency,
                    );
                    record_ledger(
                        state,
                        conf,
                        &request_id,
                        "success",
                        &node_id,
                        &node_url,
                        public_model,
                        upstream_model,
                        status.as_u16(),
                        None,
                        None,
                        latency,
                        attempt,
                        request_meta.stream,
                    );
                    return Ok(V4CallResult {
                        response,
                        request_id,
                        selected_node_id: node_id,
                        node_url_redacted: LedgerEvent::redact_node_url(&node_url),
                        observed_exit_ip,
                        upstream_model: upstream_model.to_string(),
                        outcome: "success".to_string(),
                        retry_count: attempt,
                        was_rate_limited,
                        upstream_ms: latency,
                        ttft_ms: Some(latency),
                        body_bytes_len: 0,
                    });
                }
                last_status = status;
                report_status_failure(
                    state,
                    conf,
                    &request_id,
                    &node_id,
                    &node_url,
                    public_model,
                    upstream_model,
                    status.as_u16(),
                    latency,
                    attempt,
                    request_meta.stream,
                );
                if status == StatusCode::TOO_MANY_REQUESTS {
                    was_rate_limited = true;
                }
                if attempt >= max {
                    let outcome = if status == StatusCode::TOO_MANY_REQUESTS {
                        "rate_limited"
                    } else {
                        "upstream_error"
                    };
                    return Err(V4CallError::after_dispatch(
                        status,
                        format!("upstream error {}", status.as_u16()),
                        retry_after(&response),
                        request_id,
                        node_id,
                        &node_url,
                        upstream_model,
                        outcome,
                        attempt,
                        was_rate_limited,
                        latency,
                    ));
                }
            }
            Err(err) => {
                let status = err.status;
                last_status = status;
                let retry_after = err
                    .upstream_headers
                    .as_ref()
                    .and_then(|headers| {
                        headers
                            .iter()
                            .find(|(key, _)| key.eq_ignore_ascii_case("retry-after"))
                    })
                    .and_then(|(_, value)| value.parse::<u64>().ok());
                if status == StatusCode::TOO_MANY_REQUESTS {
                    was_rate_limited = true;
                    state
                        .pool_manager
                        .report(node_id.clone(), ResultKind::RateLimited, latency);
                    record_ledger(
                        state,
                        conf,
                        &request_id,
                        "rate_limited",
                        &node_id,
                        &node_url,
                        public_model,
                        upstream_model,
                        status.as_u16(),
                        retry_after.map(|value| value as i64),
                        Some("upstream_429"),
                        latency,
                        attempt,
                        request_meta.stream,
                    );
                } else {
                    let (error_kind, outcome, error_type) = classify_app_error(&err);
                    state.pool_manager.report(
                        node_id.clone(),
                        ResultKind::Error { kind: error_kind },
                        latency,
                    );
                    record_ledger(
                        state,
                        conf,
                        &request_id,
                        outcome,
                        &node_id,
                        &node_url,
                        public_model,
                        upstream_model,
                        status.as_u16(),
                        None,
                        Some(error_type),
                        latency,
                        attempt,
                        request_meta.stream,
                    );
                }
                if attempt >= max {
                    let (error_kind, outcome, _) = classify_app_error(&err);
                    let outcome = if status == StatusCode::TOO_MANY_REQUESTS {
                        "rate_limited"
                    } else if matches!(
                        error_kind,
                        ErrorKind::Timeout
                            | ErrorKind::ConnectionRefused
                            | ErrorKind::DnsFailure
                            | ErrorKind::SocksHandshake
                            | ErrorKind::Other
                    ) {
                        "transport_error"
                    } else {
                        outcome
                    };
                    return Err(V4CallError::after_dispatch(
                        status,
                        err.message,
                        retry_after,
                        request_id,
                        node_id,
                        &node_url,
                        upstream_model,
                        outcome,
                        attempt,
                        was_rate_limited || status == StatusCode::TOO_MANY_REQUESTS,
                        latency,
                    ));
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(100 * (attempt as u64 + 1))).await;
    }

    Err(V4CallError::before_dispatch(
        last_status,
        format!("upstream error {}", last_status.as_u16()),
    ))
}

async fn dispatch_or_wait(
    state: &Arc<AppState>,
    request_meta: &RequestMeta,
    attempt: u32,
    max: u32,
) -> Result<crate::pool::DispatchResult, V4CallError> {
    match state.pool_manager.dispatch(request_meta) {
        Ok(result) => Ok(result),
        Err(DispatchError::CircuitOpen) => Err(V4CallError::before_dispatch(
            StatusCode::SERVICE_UNAVAILABLE,
            "circuit open: upstream rate limit detected",
        )),
        Err(DispatchError::NoResource) => {
            if attempt < max {
                tokio::time::sleep(Duration::from_millis(100)).await;
                state.pool_manager.dispatch(request_meta).map_err(|_| {
                    V4CallError::before_dispatch(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "no proxy resources available",
                    )
                })
            } else {
                Err(V4CallError::before_dispatch(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "no proxy resources available",
                ))
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn report_status_failure(
    state: &Arc<AppState>,
    conf: &Config,
    request_id: &str,
    node_id: &str,
    node_url: &str,
    public_model: &str,
    upstream_model: &str,
    status: u16,
    latency: u64,
    attempt: u32,
    stream: bool,
) {
    if status == 429 {
        state
            .pool_manager
            .report(node_id.to_string(), ResultKind::RateLimited, latency);
        record_ledger(
            state,
            conf,
            request_id,
            "rate_limited",
            node_id,
            node_url,
            public_model,
            upstream_model,
            status,
            None,
            Some("upstream_429"),
            latency,
            attempt,
            stream,
        );
    } else {
        state.pool_manager.report(
            node_id.to_string(),
            ResultKind::Error {
                kind: ErrorKind::Upstream5xx,
            },
            latency,
        );
        record_ledger(
            state,
            conf,
            request_id,
            "upstream_error",
            node_id,
            node_url,
            public_model,
            upstream_model,
            status,
            None,
            Some("upstream_error"),
            latency,
            attempt,
            stream,
        );
    }
}

fn classify_app_error(err: &AppError) -> (ErrorKind, &'static str, &'static str) {
    let message = err.message.to_ascii_lowercase();
    if err.status == StatusCode::GATEWAY_TIMEOUT || message.contains("timeout") {
        return (ErrorKind::Timeout, "transport_error", "timeout");
    }
    if message.contains("connection refused") || message.contains("os error 111") {
        return (
            ErrorKind::ConnectionRefused,
            "transport_error",
            "connection_refused",
        );
    }
    if message.contains("dns") {
        return (ErrorKind::DnsFailure, "transport_error", "dns_failure");
    }
    if message.contains("socks") || message.contains("proxy") {
        return (
            ErrorKind::SocksHandshake,
            "transport_error",
            "socks_handshake",
        );
    }
    if message.contains("upstream connection error") {
        return (ErrorKind::Other, "transport_error", "network");
    }
    (ErrorKind::Upstream5xx, "upstream_error", "upstream_error")
}

#[allow(clippy::too_many_arguments)]
fn record_ledger(
    state: &Arc<AppState>,
    conf: &Config,
    request_id: &str,
    event_type: &str,
    node_id: &str,
    node_url: &str,
    public_model: &str,
    upstream_model: &str,
    status: u16,
    retry_after: Option<i64>,
    error_type: Option<&str>,
    latency: u64,
    attempt: u32,
    stream: bool,
) {
    state.ledger.record(&LedgerEvent {
        ts: chrono::Utc::now().timestamp_millis(),
        rid: request_id.to_string(),
        event_type: event_type.to_string(),
        node_id: node_id.to_string(),
        node_url_redacted: LedgerEvent::redact_node_url(node_url),
        model: format!("{public_model}->{upstream_model}"),
        stream,
        status,
        retry_after,
        error_type: error_type.map(ToOwned::to_owned),
        latency_ms: latency,
        upstream_api_key_hash: LedgerEvent::short_hash(&conf.upstream_api_key),
        user_agent_hash: None,
        client_hash: None,
        project_hash: None,
        session_hash: None,
        request_hash: None,
        prompt_tokens: None,
        completion_tokens: None,
        total_tokens: None,
        error_body_summary: None,
        exit_ip: None,
        pool_from: Some("dispatch".to_string()),
        pool_to: None,
        attempt,
    });
}

fn retry_after(response: &Response) -> Option<u64> {
    response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(serde_json::json!({
            "error": { "message": message.into() }
        })),
    )
        .into_response()
}

#[allow(dead_code)]
async fn buffered_response(response: Response) -> Result<(Response, u64), V4CallError> {
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .map_err(|err| {
            V4CallError::before_dispatch(
                StatusCode::BAD_GATEWAY,
                format!("failed to read provider response body: {err}"),
            )
        })?;
    let len = bytes.len() as u64;
    let mut rebuilt = Response::new(Body::from(bytes));
    *rebuilt.status_mut() = status;
    *rebuilt.headers_mut() = headers;
    Ok((rebuilt, len))
}
