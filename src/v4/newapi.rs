use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;

use crate::collector::RequestTelemetry;
use crate::config::{Config, NewApiChannelConn};
use crate::ledger::LedgerEvent;
use crate::pool::{DispatchError, ErrorKind, RequestMeta, ResultKind};
use crate::state::AppState;
use crate::v4::model::{ModelError, ModelRegistry, StaticModelRegistry};

pub async fn handle_newapi_proxy(
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
    if path != "chat/completions" {
        return error_response(
            StatusCode::NOT_FOUND,
            format!("unsupported NewAPI path: {path}"),
        );
    }

    let conf = state.config.read().unwrap().clone();
    let Some(conn) = conf.newapi_channel.clone() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "NEWAPI_CHANNEL_CONN or NEWAPI_KEY/NEWAPI_URL is required",
        );
    };

    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(err) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!("request body must be valid JSON: {err}"),
            )
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

    match call_newapi_with_retry(
        state,
        &conf,
        &conn,
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
                rid: result.request_id,
                ts: chrono::Utc::now().timestamp_millis(),
                model: public_model.clone(),
                public_model: public_model.clone(),
                upstream_model: result.upstream_model,
                protocol: "openai_chat_completions".to_string(),
                client_id: client_id.to_string(),
                path: path.to_string(),
                method: method.to_string(),
                is_streaming: streaming,
                node_url: result.node_url_redacted.clone(),
                selected_node_id: result.selected_node_id,
                selected_node_url_redacted: result.node_url_redacted,
                observed_exit_ip: result.observed_exit_ip.unwrap_or_default(),
                outcome: result.outcome,
                pool: "dispatch".to_string(),
                exit_ip: String::new(),
                status,
                rate_limited: result.was_rate_limited,
                retry_count: result.retry_count,
                latency_total_ms: latency,
                upstream_ms: result.upstream_ms,
                ttft_ms: 0,
                prompt_tokens: result.prompt_tokens.unwrap_or_default(),
                completion_tokens: result.completion_tokens.unwrap_or_default(),
                total_tokens: result.total_tokens.unwrap_or_default(),
                bytes_sent: body.len() as u64,
                bytes_received: result.body_bytes_len,
            });
            state.upstream_health.record(status);
            result.response
        }
        Err(err) => {
            state.upstream_health.record(err.status.as_u16());
            if let Some(request_id) = err.request_id.as_ref() {
                let latency = start.elapsed().as_millis() as u64;
                state.collector.record_request(&RequestTelemetry {
                    rid: request_id.clone(),
                    ts: chrono::Utc::now().timestamp_millis(),
                    model: public_model.clone(),
                    public_model: public_model.clone(),
                    upstream_model: err.upstream_model.clone(),
                    protocol: "openai_chat_completions".to_string(),
                    client_id: client_id.to_string(),
                    path: path.to_string(),
                    method: method.to_string(),
                    is_streaming: streaming,
                    node_url: err.node_url_redacted.clone().unwrap_or_default(),
                    selected_node_id: err.selected_node_id.clone().unwrap_or_default(),
                    selected_node_url_redacted: err.node_url_redacted.unwrap_or_default(),
                    observed_exit_ip: String::new(),
                    outcome: err.outcome,
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

struct NewApiCallResult {
    response: Response,
    request_id: String,
    selected_node_id: String,
    node_url_redacted: String,
    observed_exit_ip: Option<String>,
    outcome: String,
    retry_count: u32,
    was_rate_limited: bool,
    upstream_ms: u64,
    body_bytes_len: u64,
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
    total_tokens: Option<u32>,
    upstream_model: String,
}

struct NewApiCallError {
    status: StatusCode,
    message: String,
    retry_after_secs: Option<u64>,
    request_id: Option<String>,
    selected_node_id: Option<String>,
    node_url_redacted: Option<String>,
    outcome: String,
    retry_count: u32,
    was_rate_limited: bool,
    upstream_ms: u64,
    upstream_model: String,
}

impl NewApiCallError {
    fn before_dispatch(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            retry_after_secs: None,
            request_id: None,
            selected_node_id: None,
            node_url_redacted: None,
            outcome: "error".to_string(),
            retry_count: 0,
            was_rate_limited: false,
            upstream_ms: 0,
            upstream_model: String::new(),
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
        outcome: &str,
        retry_count: u32,
        was_rate_limited: bool,
        upstream_ms: u64,
        upstream_model: &str,
    ) -> Self {
        Self {
            status,
            message: message.into(),
            retry_after_secs,
            request_id: Some(request_id),
            selected_node_id: Some(node_id),
            node_url_redacted: Some(LedgerEvent::redact_node_url(node_url)),
            outcome: outcome.to_string(),
            retry_count,
            was_rate_limited,
            upstream_ms,
            upstream_model: upstream_model.to_string(),
        }
    }
}

async fn call_newapi_with_retry(
    state: &Arc<AppState>,
    conf: &Config,
    conn: &NewApiChannelConn,
    request_meta: RequestMeta,
    body: Value,
    public_model: &str,
    upstream_model: &str,
) -> Result<NewApiCallResult, NewApiCallError> {
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
        let call_start = Instant::now();

        let response = dispatch_result
            .client
            .post(format!(
                "{}/v1/chat/completions",
                conn.url.trim_end_matches('/')
            ))
            .bearer_auth(&conn.key)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await;
        let latency = call_start.elapsed().as_millis() as u64;

        match response {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
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
                        status.as_u16(),
                        None,
                        None,
                        latency,
                        attempt,
                        request_meta.stream,
                        upstream_model,
                    );
                    let observed_exit_ip = response
                        .headers()
                        .get("x-zen-observed-exit-ip")
                        .and_then(|value| value.to_str().ok())
                        .map(ToOwned::to_owned);
                    return buffered_response(
                        response,
                        NewApiSuccessMeta {
                            request_id,
                            node_id,
                            node_url,
                            observed_exit_ip,
                            retry_count: attempt,
                            was_rate_limited,
                            upstream_ms: latency,
                            upstream_model: upstream_model.to_string(),
                        },
                    )
                    .await;
                }

                last_status = status;
                if status == StatusCode::TOO_MANY_REQUESTS {
                    was_rate_limited = true;
                    state
                        .pool_manager
                        .report(node_id.clone(), ResultKind::RateLimited, latency);
                } else {
                    state.pool_manager.report(
                        node_id.clone(),
                        ResultKind::Error {
                            kind: ErrorKind::Upstream5xx,
                        },
                        latency,
                    );
                }
                if attempt >= max {
                    let retry_after = retry_after(&response);
                    let message = response.text().await.unwrap_or_default();
                    let outcome = if status == StatusCode::TOO_MANY_REQUESTS {
                        "rate_limited"
                    } else {
                        "upstream_error"
                    };
                    return Err(NewApiCallError::after_dispatch(
                        status,
                        if message.is_empty() {
                            format!("newapi upstream error {}", status.as_u16())
                        } else {
                            message
                        },
                        retry_after,
                        request_id,
                        node_id,
                        &node_url,
                        outcome,
                        attempt,
                        was_rate_limited,
                        latency,
                        upstream_model,
                    ));
                }
            }
            Err(err) => {
                last_status = if err.is_timeout() {
                    StatusCode::GATEWAY_TIMEOUT
                } else {
                    StatusCode::BAD_GATEWAY
                };
                let error_kind = if err.is_timeout() {
                    ErrorKind::Timeout
                } else if err.is_connect() {
                    ErrorKind::ConnectionRefused
                } else {
                    ErrorKind::Other
                };
                state.pool_manager.report(
                    node_id.clone(),
                    ResultKind::Error { kind: error_kind },
                    latency,
                );
                if attempt >= max {
                    return Err(NewApiCallError::after_dispatch(
                        last_status,
                        format!("newapi transport error: {err}"),
                        None,
                        request_id,
                        node_id,
                        &node_url,
                        "transport_error",
                        attempt,
                        was_rate_limited,
                        latency,
                        upstream_model,
                    ));
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(100 * (attempt as u64 + 1))).await;
    }

    Err(NewApiCallError::before_dispatch(
        last_status,
        format!("newapi upstream error {}", last_status.as_u16()),
    ))
}

async fn dispatch_or_wait(
    state: &Arc<AppState>,
    request_meta: &RequestMeta,
    attempt: u32,
    max: u32,
) -> Result<crate::pool::DispatchResult, NewApiCallError> {
    match state.pool_manager.dispatch(request_meta) {
        Ok(result) => Ok(result),
        Err(DispatchError::CircuitOpen) => Err(NewApiCallError::before_dispatch(
            StatusCode::SERVICE_UNAVAILABLE,
            "circuit open: upstream rate limit detected",
        )),
        Err(DispatchError::NoResource) => {
            if attempt < max {
                tokio::time::sleep(Duration::from_millis(100)).await;
                state.pool_manager.dispatch(request_meta).map_err(|_| {
                    NewApiCallError::before_dispatch(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "no proxy resources available",
                    )
                })
            } else {
                Err(NewApiCallError::before_dispatch(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "no proxy resources available",
                ))
            }
        }
    }
}

struct NewApiSuccessMeta {
    request_id: String,
    node_id: String,
    node_url: String,
    observed_exit_ip: Option<String>,
    retry_count: u32,
    was_rate_limited: bool,
    upstream_ms: u64,
    upstream_model: String,
}

async fn buffered_response(
    response: reqwest::Response,
    meta: NewApiSuccessMeta,
) -> Result<NewApiCallResult, NewApiCallError> {
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.bytes().await.map_err(|err| {
        NewApiCallError::after_dispatch(
            StatusCode::BAD_GATEWAY,
            format!("failed to read NewAPI response body: {err}"),
            None,
            meta.request_id.clone(),
            meta.node_id.clone(),
            &meta.node_url,
            "transport_error",
            meta.retry_count,
            meta.was_rate_limited,
            meta.upstream_ms,
            &meta.upstream_model,
        )
    })?;
    let len = bytes.len() as u64;
    let usage = serde_json::from_slice::<Value>(&bytes).ok();
    let prompt_tokens = usage
        .as_ref()
        .and_then(|value| value.pointer("/usage/prompt_tokens"))
        .and_then(|value| value.as_u64())
        .map(|value| value as u32);
    let completion_tokens = usage
        .as_ref()
        .and_then(|value| value.pointer("/usage/completion_tokens"))
        .and_then(|value| value.as_u64())
        .map(|value| value as u32);
    let total_tokens = usage
        .as_ref()
        .and_then(|value| value.pointer("/usage/total_tokens"))
        .and_then(|value| value.as_u64())
        .map(|value| value as u32);

    let mut rebuilt = Response::new(Body::from(bytes));
    *rebuilt.status_mut() = status;
    for (name, value) in headers {
        if let Some(name) = name {
            rebuilt.headers_mut().insert(name, value);
        }
    }

    Ok(NewApiCallResult {
        response: rebuilt,
        request_id: meta.request_id,
        selected_node_id: meta.node_id,
        node_url_redacted: LedgerEvent::redact_node_url(&meta.node_url),
        observed_exit_ip: meta.observed_exit_ip,
        outcome: "success".to_string(),
        retry_count: meta.retry_count,
        was_rate_limited: meta.was_rate_limited,
        upstream_ms: meta.upstream_ms,
        body_bytes_len: len,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        upstream_model: meta.upstream_model,
    })
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
    status: u16,
    retry_after: Option<i64>,
    error_type: Option<&str>,
    latency: u64,
    attempt: u32,
    stream: bool,
    upstream_model: &str,
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

fn retry_after(response: &reqwest::Response) -> Option<u64> {
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
