use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{to_bytes, Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use free_model_client_rs::error::AppError;
use free_model_client_rs::kernel::{FreeModelKernel, KernelConfig};
use free_model_client_rs::protocol::types::{AnthropicRequest, ChatRequest};
use futures::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::collector::{DataCollector, RequestAttemptTelemetry, RequestTelemetry, RequestTimings};
use crate::config::Config;
use crate::ledger::LedgerEvent;
use crate::pool::{DispatchError, ErrorKind, RequestMeta, ResultKind};
use crate::state::AppState;
use crate::v4::context;
use crate::v4::model::{ModelError, ModelRegistry, StaticModelRegistry};

pub async fn handle_v4_proxy(
    state: &Arc<AppState>,
    path: &str,
    method: &Method,
    headers: &HeaderMap,
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
    let context_plan = match context::govern_request(&conf, path, parsed, body.len()) {
        Ok(plan) => plan,
        Err(reject) => return error_response(reject.status, reject.message),
    };
    let external_request_id = extract_external_request_id(headers);
    let gateway = infer_gateway(headers, &external_request_id);
    let mut context_telemetry = context_plan.telemetry();
    let parsed = context_plan.body;

    let streaming = parsed
        .get("stream")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let public_model = parsed
        .get("model")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    tracing::info!(
        path,
        model = %public_model,
        stream_seen_by_zenproxy = streaming,
        body_size = body.len(),
        context_action = %context_telemetry.action,
        effective_body_size = context_telemetry.effective_body_bytes,
        "v4 ingress request"
    );
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
    if path == "messages"
        && upstream_body
            .get("max_tokens")
            .is_none_or(|value| value.is_null())
    {
        upstream_body["max_tokens"] = Value::from(1024);
    }
    let effective_body_len = serde_json::to_vec(&upstream_body)
        .map(|bytes| bytes.len() as u64)
        .unwrap_or(context_telemetry.effective_body_bytes);
    context_telemetry.effective_body_bytes = effective_body_len;
    context_telemetry.trimmed = effective_body_len < context_telemetry.original_body_bytes;
    context_telemetry.trimmed_bytes = context_telemetry
        .original_body_bytes
        .saturating_sub(effective_body_len);

    let request_meta = RequestMeta {
        model: public_model.clone(),
        stream: streaming,
        body_size: effective_body_len,
    };
    let stream_usage_fallback = if streaming {
        UsageCounts {
            prompt_tokens: estimate_prompt_tokens(path, &upstream_body),
            ..UsageCounts::default()
        }
    } else {
        UsageCounts::default()
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
            let mut timings = result.timings.clone();
            timings.total_ms = latency;
            if !streaming {
                timings.stream_complete_ms = latency;
                timings.first_chunk_ms = timings.first_chunk_ms.max(result.ttft_ms.unwrap_or(0));
            }
            let telemetry = RequestTelemetry {
                rid: result.request_id.clone(),
                ts: chrono::Utc::now().timestamp_millis(),
                external_request_id: external_request_id.clone(),
                gateway: gateway.clone(),
                gateway_channel_id: extract_header(headers, "x-newapi-channel-id")
                    .unwrap_or_default(),
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
                timings,
                prompt_tokens: result.usage.prompt_tokens,
                completion_tokens: result.usage.completion_tokens,
                total_tokens: result.usage.total_tokens,
                bytes_sent: effective_body_len,
                bytes_received: result.body_bytes_len,
                failure_kind: String::new(),
                failure_message: String::new(),
                retry_chain: result.retry_chain,
                context: Some(context_telemetry.clone()),
            };
            state.upstream_health.record(status);
            let mut response = if streaming {
                metered_stream_response(
                    state.clone(),
                    result.response,
                    path.to_string(),
                    telemetry,
                    start,
                    stream_usage_fallback,
                    state.collector.clone(),
                )
            } else {
                state.collector.record_request(&telemetry);
                result.response
            };
            response.headers_mut().insert(
                "x-zen-stream-seen",
                HeaderValue::from_static(if streaming { "true" } else { "false" }),
            );
            insert_context_headers(response.headers_mut(), &context_telemetry);
            response
        }
        Err(err) => {
            state.upstream_health.record(err.status.as_u16());
            if let Some(rid) = err.request_id.as_ref() {
                let latency = start.elapsed().as_millis() as u64;
                state.collector.record_request(&RequestTelemetry {
                    rid: rid.clone(),
                    ts: chrono::Utc::now().timestamp_millis(),
                    external_request_id: external_request_id.clone(),
                    gateway: gateway.clone(),
                    gateway_channel_id: extract_header(headers, "x-newapi-channel-id")
                        .unwrap_or_default(),
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
                    timings: RequestTimings {
                        upstream_response_ms: err.upstream_ms,
                        total_ms: latency,
                        ..RequestTimings::default()
                    },
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                    bytes_sent: effective_body_len,
                    bytes_received: 0,
                    failure_kind: err.failure_kind.clone(),
                    failure_message: err.message.clone(),
                    retry_chain: err.retry_chain.clone(),
                    context: Some(context_telemetry.clone()),
                });
            }
            let mut response = error_response(err.status, err.message);
            if let Some(retry_after) = err.retry_after_secs {
                response.headers_mut().insert(
                    "retry-after",
                    HeaderValue::from_str(&retry_after.to_string()).unwrap(),
                );
            }
            insert_context_headers(response.headers_mut(), &context_telemetry);
            response
        }
    }
}

fn extract_external_request_id(headers: &HeaderMap) -> String {
    for name in [
        "x-newapi-request-id",
        "x-one-api-request-id",
        "x-request-id",
        "x-client-request-id",
        "cf-ray",
    ] {
        if let Some(value) = extract_header(headers, name) {
            return value;
        }
    }
    String::new()
}

fn extract_header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn infer_gateway(headers: &HeaderMap, external_request_id: &str) -> String {
    extract_header(headers, "x-gateway")
        .or_else(|| {
            if headers.contains_key("x-newapi-request-id")
                || headers.contains_key("x-one-api-request-id")
            {
                Some("newapi".to_string())
            } else {
                None
            }
        })
        .or_else(|| {
            if !external_request_id.is_empty() {
                Some("external".to_string())
            } else {
                None
            }
        })
        .unwrap_or_default()
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
    timings: RequestTimings,
    retry_chain: Vec<RequestAttemptTelemetry>,
    body_bytes_len: u64,
    usage: UsageCounts,
}

#[derive(Debug, Clone, Copy, Default)]
struct UsageCounts {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
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
    failure_kind: String,
    retry_chain: Vec<RequestAttemptTelemetry>,
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
            failure_kind: String::new(),
            retry_chain: Vec::new(),
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
        failure_kind: impl Into<String>,
        retry_chain: Vec<RequestAttemptTelemetry>,
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
            failure_kind: failure_kind.into(),
            retry_chain,
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
    let mut dispatch_wait_ms = 0u64;
    let mut retry_chain = Vec::new();
    let retry_budget_ms = conf.v4_retry_budget_ms;

    for attempt in 0..=max {
        let dispatch_start = Instant::now();
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
        dispatch_wait_ms =
            dispatch_wait_ms.saturating_add(dispatch_start.elapsed().as_millis() as u64);

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
                    let (response, body_bytes_len, usage, has_output) = if request_meta.stream {
                        (response, 0, UsageCounts::default(), true)
                    } else {
                        buffered_response_with_usage(response, path).await?
                    };
                    if !request_meta.stream && !has_output {
                        state.pool_manager.report(
                            node_id.clone(),
                            ResultKind::Error {
                                kind: ErrorKind::Other,
                            },
                            latency,
                        );
                        record_ledger(
                            state,
                            conf,
                            &request_id,
                            "empty_output",
                            &node_id,
                            &node_url,
                            public_model,
                            upstream_model,
                            StatusCode::BAD_GATEWAY.as_u16(),
                            None,
                            Some("empty_output"),
                            latency,
                            attempt,
                            request_meta.stream,
                        );
                        retry_chain.push(RequestAttemptTelemetry {
                            attempt,
                            node_id: node_id.clone(),
                            node_url_redacted: LedgerEvent::redact_node_url(&node_url),
                            status: StatusCode::BAD_GATEWAY.as_u16(),
                            latency_ms: latency,
                            outcome: "empty_output".to_string(),
                            error_type: "empty_output".to_string(),
                        });
                        last_status = StatusCode::BAD_GATEWAY;
                        if attempt >= max {
                            return Err(V4CallError::after_dispatch(
                                StatusCode::BAD_GATEWAY,
                                "upstream returned no assistant content or tool call",
                                None,
                                request_id,
                                node_id,
                                &node_url,
                                upstream_model,
                                "empty_output",
                                attempt,
                                was_rate_limited,
                                latency,
                                "empty_output",
                                retry_chain,
                            ));
                        }
                        continue;
                    }
                    if !request_meta.stream {
                        state.pool_manager.report(
                            node_id.clone(),
                            ResultKind::Success(status.as_u16()),
                            latency,
                        );
                    }
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
                    retry_chain.push(RequestAttemptTelemetry {
                        attempt,
                        node_id: node_id.clone(),
                        node_url_redacted: LedgerEvent::redact_node_url(&node_url),
                        status: status.as_u16(),
                        latency_ms: latency,
                        outcome: "success".to_string(),
                        error_type: String::new(),
                    });
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
                        timings: RequestTimings {
                            dispatch_wait_ms,
                            upstream_response_ms: latency,
                            first_chunk_ms: if request_meta.stream { 0 } else { latency },
                            stream_complete_ms: 0,
                            total_ms: latency,
                        },
                        retry_chain,
                        body_bytes_len,
                        usage,
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
                let failure_kind = if status == StatusCode::TOO_MANY_REQUESTS {
                    "upstream_429"
                } else {
                    "upstream_error"
                };
                retry_chain.push(RequestAttemptTelemetry {
                    attempt,
                    node_id: node_id.clone(),
                    node_url_redacted: LedgerEvent::redact_node_url(&node_url),
                    status: status.as_u16(),
                    latency_ms: latency,
                    outcome: if status == StatusCode::TOO_MANY_REQUESTS {
                        "rate_limited".to_string()
                    } else {
                        "upstream_error".to_string()
                    },
                    error_type: failure_kind.to_string(),
                });
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
                        failure_kind,
                        retry_chain,
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
                    retry_chain.push(RequestAttemptTelemetry {
                        attempt,
                        node_id: node_id.clone(),
                        node_url_redacted: LedgerEvent::redact_node_url(&node_url),
                        status: status.as_u16(),
                        latency_ms: latency,
                        outcome: "rate_limited".to_string(),
                        error_type: "upstream_429".to_string(),
                    });
                } else if is_upstream_busy(status, &err.message) {
                    state.pool_manager.report(
                        node_id.clone(),
                        ResultKind::Success(status.as_u16()),
                        latency,
                    );
                    record_ledger(
                        state,
                        conf,
                        &request_id,
                        "upstream_busy",
                        &node_id,
                        &node_url,
                        public_model,
                        upstream_model,
                        status.as_u16(),
                        retry_after.map(|value| value as i64),
                        Some("upstream_busy"),
                        latency,
                        attempt,
                        request_meta.stream,
                    );
                    retry_chain.push(RequestAttemptTelemetry {
                        attempt,
                        node_id: node_id.clone(),
                        node_url_redacted: LedgerEvent::redact_node_url(&node_url),
                        status: status.as_u16(),
                        latency_ms: latency,
                        outcome: "upstream_busy".to_string(),
                        error_type: "upstream_busy".to_string(),
                    });
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
                    retry_chain.push(RequestAttemptTelemetry {
                        attempt,
                        node_id: node_id.clone(),
                        node_url_redacted: LedgerEvent::redact_node_url(&node_url),
                        status: status.as_u16(),
                        latency_ms: latency,
                        outcome: outcome.to_string(),
                        error_type: error_type.to_string(),
                    });
                }
                if attempt >= max {
                    let (error_kind, outcome, _) = classify_app_error(&err);
                    let outcome = if status == StatusCode::TOO_MANY_REQUESTS {
                        "rate_limited"
                    } else if is_upstream_busy(status, &err.message) {
                        "upstream_busy"
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
                        outcome,
                        retry_chain,
                    ));
                }
            }
        }

        let elapsed_ms: u64 = retry_chain.iter().map(|attempt| attempt.latency_ms).sum();
        if retry_budget_ms > 0 && elapsed_ms >= retry_budget_ms {
            return Err(V4CallError::after_dispatch(
                last_status,
                format!(
                    "upstream retry budget exhausted after {}ms with status {}",
                    elapsed_ms,
                    last_status.as_u16()
                ),
                None,
                uuid::Uuid::new_v4().to_string(),
                last_node_id.clone(),
                "",
                upstream_model,
                "retry_budget_exhausted",
                attempt,
                was_rate_limited,
                elapsed_ms,
                "retry_budget_exhausted",
                retry_chain,
            ));
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
        Err(DispatchError::RequestTooLarge) => Err(V4CallError::before_dispatch(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request exceeds proxy node budget",
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

fn is_upstream_busy(status: StatusCode, message: &str) -> bool {
    status == StatusCode::SERVICE_UNAVAILABLE
        && (message.contains("Service is too busy")
            || message.contains("service_unavailable_error"))
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

fn insert_context_headers(headers: &mut HeaderMap, telemetry: &crate::collector::ContextTelemetry) {
    if let Ok(value) = HeaderValue::from_str(&telemetry.action) {
        headers.insert("x-zen-context-action", value);
    }
    if let Ok(value) = HeaderValue::from_str(&telemetry.original_body_bytes.to_string()) {
        headers.insert("x-zen-context-original-bytes", value);
    }
    if let Ok(value) = HeaderValue::from_str(&telemetry.effective_body_bytes.to_string()) {
        headers.insert("x-zen-context-effective-bytes", value);
    }
    headers.insert(
        "x-zen-context-trimmed",
        HeaderValue::from_static(if telemetry.trimmed { "true" } else { "false" }),
    );
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

async fn buffered_response_with_usage(
    response: Response,
    path: &str,
) -> Result<(Response, u64, UsageCounts, bool), V4CallError> {
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
    let usage = extract_usage_counts(path, &bytes);
    let has_output = response_has_assistant_output(path, &bytes) || usage.completion_tokens > 0;
    let mut rebuilt = Response::new(Body::from(bytes));
    *rebuilt.status_mut() = status;
    *rebuilt.headers_mut() = headers;
    Ok((rebuilt, len, usage, has_output))
}

fn response_has_assistant_output(path: &str, bytes: &Bytes) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return false;
    };
    if path == "messages" {
        return value
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    matches!(item.get("type").and_then(Value::as_str), Some("tool_use"))
                        || item
                            .get("text")
                            .and_then(Value::as_str)
                            .is_some_and(|text| !text.trim().is_empty())
                })
            });
    }
    value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .is_some_and(|message| {
            message
                .get("content")
                .and_then(Value::as_str)
                .is_some_and(|text| !text.trim().is_empty())
                || message
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .is_some_and(|items| !items.is_empty())
        })
}

fn metered_stream_response(
    state: Arc<AppState>,
    response: Response,
    path: String,
    telemetry: RequestTelemetry,
    request_start: Instant,
    fallback_usage: UsageCounts,
    collector: Arc<dyn DataCollector>,
) -> Response {
    let status = response.status();
    let headers = response.headers().clone();
    let mut upstream = response.into_body().into_data_stream();
    let (tx, rx) = mpsc::channel::<Result<Bytes, axum::Error>>(16);

    tokio::spawn(async move {
        let mut telemetry = telemetry;
        let mut metrics = StreamMetrics::new(fallback_usage);
        let mut first_chunk_ms = 0u64;
        while let Some(item) = upstream.next().await {
            match item {
                Ok(bytes) => {
                    if first_chunk_ms == 0 {
                        first_chunk_ms = request_start.elapsed().as_millis() as u64;
                    }
                    metrics.ingest(&path, &bytes);
                    if tx.send(Ok(bytes)).await.is_err() {
                        break;
                    }
                }
                Err(err) => {
                    let _ = tx.send(Err(err)).await;
                    break;
                }
            }
        }
        let stream_complete_ms = request_start.elapsed().as_millis() as u64;
        telemetry.bytes_received = metrics.bytes_received;
        let usage = metrics.final_usage();
        telemetry.prompt_tokens = usage.prompt_tokens;
        telemetry.completion_tokens = usage.completion_tokens;
        telemetry.total_tokens = usage.total_tokens;
        telemetry.latency_total_ms = stream_complete_ms;
        telemetry.ttft_ms = first_chunk_ms;
        telemetry.timings.first_chunk_ms = first_chunk_ms;
        telemetry.timings.stream_complete_ms = stream_complete_ms;
        telemetry.timings.total_ms = stream_complete_ms;
        let empty_output = usage.completion_tokens == 0 && !metrics.has_assistant_output();
        if empty_output {
            telemetry.outcome = "empty_output".to_string();
            telemetry.failure_kind = "empty_output".to_string();
            telemetry.failure_message =
                "upstream returned no assistant content or tool call".to_string();
            telemetry.retry_chain.push(RequestAttemptTelemetry {
                attempt: telemetry.retry_count,
                node_id: telemetry.selected_node_id.clone(),
                node_url_redacted: telemetry.selected_node_url_redacted.clone(),
                status: telemetry.status,
                latency_ms: stream_complete_ms,
                outcome: "empty_output".to_string(),
                error_type: "empty_output".to_string(),
            });
            state.pool_manager.report(
                telemetry.selected_node_id.clone(),
                ResultKind::Error {
                    kind: ErrorKind::Other,
                },
                stream_complete_ms,
            );
        } else {
            state.pool_manager.report(
                telemetry.selected_node_id.clone(),
                ResultKind::Success(telemetry.status),
                stream_complete_ms,
            );
        }
        collector.record_request(&telemetry);
    });

    let mut rebuilt = Response::new(Body::from_stream(ReceiverStream::new(rx)));
    *rebuilt.status_mut() = status;
    *rebuilt.headers_mut() = headers;
    rebuilt
}

#[derive(Default)]
struct StreamMetrics {
    bytes_received: u64,
    usage: UsageCounts,
    fallback_usage: UsageCounts,
    completion_text: String,
    tool_output_chunks: u64,
    text_output_chunks: u64,
    buffer: String,
}

impl StreamMetrics {
    fn new(fallback_usage: UsageCounts) -> Self {
        Self {
            fallback_usage,
            ..Self::default()
        }
    }

    fn ingest(&mut self, path: &str, bytes: &Bytes) {
        self.bytes_received = self.bytes_received.saturating_add(bytes.len() as u64);
        let text = String::from_utf8_lossy(bytes);
        self.buffer.push_str(&text);
        while let Some(idx) = self.buffer.find("\n\n") {
            let frame = self.buffer[..idx].to_string();
            self.buffer.drain(..idx + 2);
            self.ingest_sse_frame(path, &frame);
        }
    }

    fn ingest_sse_frame(&mut self, path: &str, frame: &str) {
        for line in frame.lines() {
            let Some(data) = line.trim_start().strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            self.ingest_usage_value(path, &value);
        }
    }

    fn ingest_usage_value(&mut self, path: &str, value: &Value) {
        if path == "messages" {
            match value.get("type").and_then(Value::as_str) {
                Some("content_block_start") => {
                    if value
                        .get("content_block")
                        .and_then(|block| block.get("type"))
                        .and_then(Value::as_str)
                        .is_some_and(|kind| kind == "tool_use")
                    {
                        self.tool_output_chunks = self.tool_output_chunks.saturating_add(1);
                    }
                }
                Some("content_block_delta") => {
                    if let Some(delta) = value.get("delta") {
                        if delta
                            .get("text")
                            .and_then(Value::as_str)
                            .is_some_and(|text| !text.trim().is_empty())
                        {
                            self.text_output_chunks = self.text_output_chunks.saturating_add(1);
                        }
                        if delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .is_some_and(|json| !json.trim().is_empty())
                        {
                            self.tool_output_chunks = self.tool_output_chunks.saturating_add(1);
                        }
                    }
                }
                Some("error") => {
                    self.usage.completion_tokens = 0;
                }
                _ => {}
            }
            if let Some(usage) = value
                .get("message")
                .and_then(|message| message.get("usage"))
                .or_else(|| value.get("usage"))
            {
                self.usage.prompt_tokens = usage_u32(usage, "input_tokens");
                let output_tokens = usage_u32(usage, "output_tokens");
                if output_tokens > 0 {
                    self.usage.completion_tokens = output_tokens;
                }
                self.usage.total_tokens = self
                    .usage
                    .prompt_tokens
                    .saturating_add(self.usage.completion_tokens);
            }
            return;
        }

        if let Some(text) = value
            .get("choices")
            .and_then(|choices| choices.as_array())
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("delta"))
            .and_then(|delta| delta.get("content"))
            .and_then(|content| content.as_str())
        {
            self.completion_text.push_str(text);
            if !text.trim().is_empty() {
                self.text_output_chunks = self.text_output_chunks.saturating_add(1);
            }
        }
        if value
            .get("choices")
            .and_then(|choices| choices.as_array())
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("delta"))
            .and_then(|delta| delta.get("tool_calls"))
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty())
        {
            self.tool_output_chunks = self.tool_output_chunks.saturating_add(1);
        }

        let Some(usage) = value.get("usage") else {
            return;
        };
        let prompt_tokens = usage_u32(usage, "prompt_tokens");
        let completion_tokens = usage_u32(usage, "completion_tokens");
        let total_tokens = usage_u32(usage, "total_tokens");
        if prompt_tokens > 0 {
            self.usage.prompt_tokens = prompt_tokens;
        }
        if completion_tokens > 0 {
            self.usage.completion_tokens = completion_tokens;
        }
        if total_tokens > 0 {
            self.usage.total_tokens = total_tokens;
        } else {
            self.usage.total_tokens = self
                .usage
                .prompt_tokens
                .saturating_add(self.usage.completion_tokens);
        }
    }

    fn final_usage(&self) -> UsageCounts {
        let prompt_tokens = self
            .usage
            .prompt_tokens
            .max(self.fallback_usage.prompt_tokens);
        let completion_tokens = self.usage.completion_tokens.max(
            self.fallback_usage
                .completion_tokens
                .max(estimate_text_tokens(&self.completion_text))
                .max(if self.tool_output_chunks > 0 { 1 } else { 0 }),
        );
        let total_tokens = self
            .usage
            .total_tokens
            .max(prompt_tokens.saturating_add(completion_tokens));
        UsageCounts {
            prompt_tokens,
            completion_tokens,
            total_tokens,
        }
    }

    fn has_assistant_output(&self) -> bool {
        !self.completion_text.trim().is_empty()
            || self.text_output_chunks > 0
            || self.tool_output_chunks > 0
            || self.usage.completion_tokens > 0
    }
}

fn usage_u32(usage: &Value, name: &str) -> u32 {
    usage
        .get(name)
        .and_then(|value| value.as_u64())
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32
}

fn extract_usage_counts(path: &str, bytes: &Bytes) -> UsageCounts {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return UsageCounts::default();
    };
    let Some(usage) = value.get("usage") else {
        return UsageCounts::default();
    };
    if path == "messages" {
        let prompt_tokens = usage_u32(usage, "input_tokens");
        let completion_tokens = usage_u32(usage, "output_tokens");
        UsageCounts {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens.saturating_add(completion_tokens),
        }
    } else {
        UsageCounts {
            prompt_tokens: usage_u32(usage, "prompt_tokens"),
            completion_tokens: usage_u32(usage, "completion_tokens"),
            total_tokens: usage_u32(usage, "total_tokens"),
        }
    }
}

fn estimate_prompt_tokens(path: &str, body: &Value) -> u32 {
    if path == "messages" {
        return body
            .get("messages")
            .and_then(|messages| messages.as_array())
            .map(|messages| {
                messages
                    .iter()
                    .map(|message| estimate_message_content_tokens(message.get("content")))
                    .sum()
            })
            .unwrap_or(0);
    }

    body.get("messages")
        .and_then(|messages| messages.as_array())
        .map(|messages| {
            messages
                .iter()
                .map(|message| estimate_message_content_tokens(message.get("content")))
                .sum()
        })
        .unwrap_or(0)
}

fn estimate_message_content_tokens(content: Option<&Value>) -> u32 {
    match content {
        Some(Value::String(text)) => estimate_text_tokens(text),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(|text| text.as_str())
                    .or_else(|| part.get("content").and_then(|text| text.as_str()))
            })
            .map(estimate_text_tokens)
            .sum(),
        _ => 0,
    }
}

fn estimate_text_tokens(text: &str) -> u32 {
    let word_like = text.split_whitespace().count() as u32;
    let char_like = text.chars().count().div_ceil(4) as u32;
    word_like.max(char_like)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Bytes;

    #[test]
    fn detects_openai_empty_assistant_output() {
        let body = Bytes::from_static(
            br#"{"choices":[{"message":{"role":"assistant","content":""}}],"usage":{"prompt_tokens":10,"completion_tokens":0,"total_tokens":10}}"#,
        );
        assert!(!response_has_assistant_output("chat/completions", &body));
    }

    #[test]
    fn detects_openai_tool_output_as_assistant_output() {
        let body = Bytes::from_static(
            br#"{"choices":[{"message":{"role":"assistant","content":"","tool_calls":[{"type":"function","function":{"name":"Task","arguments":"{}"}}]}}],"usage":{"prompt_tokens":10,"completion_tokens":0,"total_tokens":10}}"#,
        );
        assert!(response_has_assistant_output("chat/completions", &body));
    }

    #[test]
    fn detects_anthropic_empty_assistant_output() {
        let body = Bytes::from_static(
            br#"{"content":[{"type":"text","text":""}],"usage":{"input_tokens":10,"output_tokens":0}}"#,
        );
        assert!(!response_has_assistant_output("messages", &body));
    }

    #[test]
    fn detects_anthropic_tool_use_as_assistant_output() {
        let body = Bytes::from_static(
            br#"{"content":[{"type":"tool_use","id":"toolu_1","name":"Task","input":{}}],"usage":{"input_tokens":10,"output_tokens":0}}"#,
        );
        assert!(response_has_assistant_output("messages", &body));
    }
}
