use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{to_bytes, Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
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
    let registry = StaticModelRegistry::default();
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
                model: public_model,
                client_id: client_id.to_string(),
                path: path.to_string(),
                method: method.to_string(),
                is_streaming: streaming,
                node_url: result.node_url_redacted.clone(),
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
    node_url_redacted: String,
    observed_exit_ip: Option<String>,
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
    let max = conf.pool_max_retries.max(1);
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
        });
        let call_start = Instant::now();
        let response = match path {
            "chat/completions" => {
                let request = serde_json::from_value::<ChatRequest>(upstream_body.clone())
                    .map_err(|err| V4CallError {
                        status: StatusCode::BAD_REQUEST,
                        message: format!("invalid OpenAI chat request: {err}"),
                        retry_after_secs: None,
                    })?;
                kernel.openai_chat(&dispatch_result.client, request).await
            }
            "messages" => {
                let request = serde_json::from_value::<AnthropicRequest>(upstream_body.clone())
                    .map_err(|err| V4CallError {
                        status: StatusCode::BAD_REQUEST,
                        message: format!("invalid Anthropic messages request: {err}"),
                        retry_after_secs: None,
                    })?;
                kernel
                    .anthropic_messages(&dispatch_result.client, request)
                    .await
            }
            _ => {
                return Err(V4CallError {
                    status: StatusCode::NOT_FOUND,
                    message: format!("unsupported V4 path: {path}"),
                    retry_after_secs: None,
                })
            }
        };
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
                        upstream_model,
                        status.as_u16(),
                        None,
                        None,
                        latency,
                        attempt,
                    );
                    let body_bytes_len = response
                        .headers()
                        .get("content-type")
                        .and_then(|value| value.to_str().ok())
                        .map(|ct| ct.contains("text/event-stream"))
                        .unwrap_or(false)
                        .then_some(0)
                        .unwrap_or(0);
                    return Ok(V4CallResult {
                        response,
                        request_id,
                        node_url_redacted: LedgerEvent::redact_node_url(&node_url),
                        observed_exit_ip: None,
                        retry_count: attempt,
                        was_rate_limited,
                        upstream_ms: latency,
                        ttft_ms: None,
                        body_bytes_len,
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
                );
                if status == StatusCode::TOO_MANY_REQUESTS {
                    was_rate_limited = true;
                }
                if attempt >= max {
                    return Err(V4CallError {
                        status,
                        message: format!("upstream error {}", status.as_u16()),
                        retry_after_secs: retry_after(&response),
                    });
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
                    );
                } else {
                    state.pool_manager.report(
                        node_id.clone(),
                        ResultKind::Error {
                            kind: ErrorKind::Upstream5xx,
                        },
                        latency,
                    );
                    record_ledger(
                        state,
                        conf,
                        &request_id,
                        "upstream_error",
                        &node_id,
                        &node_url,
                        public_model,
                        upstream_model,
                        status.as_u16(),
                        None,
                        Some("upstream_error"),
                        latency,
                        attempt,
                    );
                }
                if attempt >= max {
                    return Err(V4CallError {
                        status,
                        message: err.message,
                        retry_after_secs: retry_after,
                    });
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(100 * (attempt as u64 + 1))).await;
    }

    Err(V4CallError {
        status: last_status,
        message: format!("upstream error {}", last_status.as_u16()),
        retry_after_secs: None,
    })
}

async fn dispatch_or_wait(
    state: &Arc<AppState>,
    request_meta: &RequestMeta,
    attempt: u32,
    max: u32,
) -> Result<crate::pool::DispatchResult, V4CallError> {
    match state.pool_manager.dispatch(request_meta) {
        Ok(result) => Ok(result),
        Err(DispatchError::CircuitOpen) => Err(V4CallError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: "circuit open: upstream rate limit detected".to_string(),
            retry_after_secs: None,
        }),
        Err(DispatchError::NoResource) => {
            if attempt < max {
                tokio::time::sleep(Duration::from_millis(100)).await;
                state
                    .pool_manager
                    .dispatch(request_meta)
                    .map_err(|_| V4CallError {
                        status: StatusCode::SERVICE_UNAVAILABLE,
                        message: "no proxy resources available".to_string(),
                        retry_after_secs: None,
                    })
            } else {
                Err(V4CallError {
                    status: StatusCode::SERVICE_UNAVAILABLE,
                    message: "no proxy resources available".to_string(),
                    retry_after_secs: None,
                })
            }
        }
    }
}

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
        );
    }
}

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
) {
    state.ledger.record(&LedgerEvent {
        ts: chrono::Utc::now().timestamp_millis(),
        rid: request_id.to_string(),
        event_type: event_type.to_string(),
        node_id: node_id.to_string(),
        node_url_redacted: LedgerEvent::redact_node_url(node_url),
        model: format!("{public_model}->{upstream_model}"),
        stream: false,
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
        .map_err(|err| V4CallError {
            status: StatusCode::BAD_GATEWAY,
            message: format!("failed to read provider response body: {err}"),
            retry_after_secs: None,
        })?;
    let len = bytes.len() as u64;
    let mut rebuilt = Response::new(Body::from(bytes));
    *rebuilt.status_mut() = status;
    *rebuilt.headers_mut() = headers;
    Ok((rebuilt, len))
}
