use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::Value;
use tracing::{error, info, warn};

use crate::collector::RequestTelemetry;
use crate::ledger::{LedgerCounters, LedgerEvent};
use crate::pool::{DispatchError, ErrorKind, RequestMeta};
use crate::state::AppState;
use crate::utils::{
    apply_model_override, build_upstream_url, patch_response_content, patch_sse_line, should_retry,
    smart_backoff,
};

fn extract_bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

fn is_streaming(body: &Value) -> bool {
    body.get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

pub async fn proxy_handler(
    State(state): State<Arc<AppState>>,
    method: Method,
    Path(path): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let start = Instant::now();
    let client_id = extract_bearer_token(&headers).unwrap_or_default();

    let (streaming, modified_body) = if body.is_empty() {
        (false, body.to_vec())
    } else {
        let patched = apply_model_override(&body, &state.config);
        let parsed: Value = serde_json::from_slice(&patched).unwrap_or(Value::Null);
        (is_streaming(&parsed), patched)
    };

    let body_len = modified_body.len() as u64;
    let model = serde_json::from_slice::<Value>(&modified_body)
        .ok()
        .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(String::from))
        .unwrap_or_default();

    let req_meta = RequestMeta {
        model: model.clone(),
        stream: streaming,
        body_size: body_len,
    };

    let result = proxy_with_retry(
        &state,
        &path,
        &method,
        &modified_body,
        streaming,
        &req_meta,
        &client_id,
        &model,
    )
    .await;

    match result {
        Ok((resp, node_url, status, latency)) => {
            state.collector.record_request(&RequestTelemetry {
                rid: uuid::Uuid::new_v4().to_string(),
                ts: chrono::Utc::now().timestamp_millis(),
                model,
                client_id,
                path: path.clone(),
                method: method.to_string(),
                is_streaming: streaming,
                node_url,
                pool: String::new(),
                exit_ip: String::new(),
                status: status as u16,
                rate_limited: false,
                retry_count: 0,
                latency_total_ms: latency,
                upstream_ms: latency,
                ttft_ms: 0,
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                bytes_sent: body_len,
                bytes_received: 0,
            });
            state.upstream_health.record(status);
            info!(
                method = %method, path = %path,
                status = status,
                duration_ms = latency,
                "proxy OK"
            );
            resp
        }
        Err(status) => {
            let elapsed = start.elapsed().as_millis() as u64;
            state.upstream_health.record(status);
            warn!(
                method = %method, path = %path,
                status = status, duration_ms = elapsed,
                "proxy FAIL"
            );
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
            (
                code,
                Json(serde_json::json!({
                    "error": { "message": format!("upstream error {}", status) }
                })),
            )
                .into_response()
        }
    }
}

async fn proxy_with_retry(
    state: &Arc<AppState>,
    path: &str,
    method: &Method,
    body: &[u8],
    streaming: bool,
    req_meta: &RequestMeta,
    client_id: &str,
    model: &str,
) -> Result<(Response, String, u16, u64), u16> {
    let max = state.config.pool_max_retries.max(1);
    let mut last_status = 502u16;

    for attempt in 0..=max {
        let dispatch_result = match state.pool_manager.dispatch(req_meta) {
            Ok(r) => r,
            Err(DispatchError::NoResource) => {
                if attempt < max {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                return Err(503);
            }
        };

        let node_url = dispatch_result.url.clone();
        let node_id = dispatch_result.node.id.clone();
        let client = dispatch_result.client;
        let upstream = build_upstream_url(&state.config.upstream_base, &format!("v1/{}", path));
        let request_start = Instant::now();

        let req_method = reqwest::Method::from_bytes(method.as_str().as_bytes())
            .unwrap_or(reqwest::Method::POST);
        let mut req = client.request(req_method, &upstream);
        req = req.header("Content-Type", "application/json");
        req = req.header("x-api-key", &state.config.upstream_api_key);
        if !body.is_empty() {
            req = req.body(body.to_vec());
        }

        match req.send().await {
            Ok(up_resp) => {
                let status = up_resp.status().as_u16();
                let latency = request_start.elapsed().as_millis() as u64;
                last_status = status;

                if status < 400 {
                    state.pool_manager.report(
                        node_id.clone(),
                        crate::pool::ResultKind::Success(status),
                        latency,
                    );
                    state.ledger.record(&LedgerEvent {
                        ts: chrono::Utc::now().timestamp_millis(),
                        rid: uuid::Uuid::new_v4().to_string(),
                        event_type: "success".into(),
                        node_id: node_id.clone(),
                        node_url_redacted: LedgerEvent::redact_node_url(&node_url),
                        model: model.to_string(),
                        stream: streaming,
                        status: status as u16,
                        retry_after: None,
                        error_type: None,
                        latency_ms: latency,
                        upstream_api_key_hash: LedgerEvent::short_hash(&state.config.upstream_api_key),
                        user_agent_hash: None,
                        client_hash: None,
                        project_hash: None,
                        session_hash: None,
                        request_hash: None,
                        prompt_tokens: None,
                        completion_tokens: None,
                        total_tokens: None,
                        pool_from: None,
                        pool_to: None,
                        attempt: attempt as u32,
                    });
                    if streaming && status == 200 {
                        return Ok((stream_to_axum(up_resp).await, node_url, status, latency));
                    }
                    return Ok((read_full_body(up_resp).await, node_url, status, latency));
                }

                if status == 429 {
                    state.pool_manager.report(
                        node_id.clone(),
                        crate::pool::ResultKind::RateLimited,
                        latency,
                    );
                    state.ledger.record(&LedgerEvent {
                        ts: chrono::Utc::now().timestamp_millis(),
                        rid: uuid::Uuid::new_v4().to_string(),
                        event_type: "rate_limited".into(),
                        node_id: node_id.clone(),
                        node_url_redacted: LedgerEvent::redact_node_url(&node_url),
                        model: model.to_string(),
                        stream: streaming,
                        status: status as u16,
                        retry_after: None,
                        error_type: Some("upstream_429".into()),
                        latency_ms: latency,
                        upstream_api_key_hash: LedgerEvent::short_hash(&state.config.upstream_api_key),
                        user_agent_hash: None,
                        client_hash: None,
                        project_hash: None,
                        session_hash: None,
                        request_hash: None,
                        prompt_tokens: None,
                        completion_tokens: None,
                        total_tokens: None,
                        pool_from: Some("dispatch".into()),
                        pool_to: Some("ratelimited".into()),
                        attempt: attempt as u32,
                    });
                } else {
                    state.pool_manager.report(
                        node_id.clone(),
                        crate::pool::ResultKind::Error {
                            kind: ErrorKind::Upstream5xx,
                        },
                        latency,
                    );
                    state.ledger.record(&LedgerEvent {
                        ts: chrono::Utc::now().timestamp_millis(),
                        rid: uuid::Uuid::new_v4().to_string(),
                        event_type: "upstream_5xx".into(),
                        node_id: node_id.clone(),
                        node_url_redacted: LedgerEvent::redact_node_url(&node_url),
                        model: model.to_string(),
                        stream: streaming,
                        status: status as u16,
                        retry_after: None,
                        error_type: None,
                        latency_ms: latency,
                        upstream_api_key_hash: LedgerEvent::short_hash(&state.config.upstream_api_key),
                        user_agent_hash: None,
                        client_hash: None,
                        project_hash: None,
                        session_hash: None,
                        request_hash: None,
                        prompt_tokens: None,
                        completion_tokens: None,
                        total_tokens: None,
                        pool_from: Some("dispatch".into()),
                        pool_to: None,
                        attempt: attempt as u32,
                    });
                }

                if !should_retry(status, attempt, max) {
                    return Err(status);
                }
                let backoff = smart_backoff(attempt, Some(status));
                tokio::time::sleep(Duration::from_secs_f64(backoff)).await;
            }
            Err(e) => {
                let latency = request_start.elapsed().as_millis() as u64;
                last_status = 502;
                state.pool_manager.report(
                    node_id.clone(),
                    crate::pool::ResultKind::Error {
                        kind: ErrorKind::Timeout,
                    },
                    latency,
                );
                state.ledger.record(&LedgerEvent {
                    ts: chrono::Utc::now().timestamp_millis(),
                    rid: uuid::Uuid::new_v4().to_string(),
                    event_type: "network_error".into(),
                    node_id: node_id.clone(),
                    node_url_redacted: LedgerEvent::redact_node_url(&node_url),
                    model: model.to_string(),
                    stream: streaming,
                    status: 502,
                    retry_after: None,
                    error_type: Some("timeout".into()),
                    latency_ms: latency,
                    upstream_api_key_hash: LedgerEvent::short_hash(&state.config.upstream_api_key),
                    user_agent_hash: None,
                    client_hash: None,
                    project_hash: None,
                    session_hash: None,
                    request_hash: None,
                    prompt_tokens: None,
                    completion_tokens: None,
                    total_tokens: None,
                    pool_from: Some("dispatch".into()),
                    pool_to: None,
                    attempt: attempt as u32,
                });
                warn!(attempt, error = %e, "upstream request error");
                if attempt < max {
                    let backoff = smart_backoff(attempt, None);
                    tokio::time::sleep(Duration::from_secs_f64(backoff)).await;
                }
            }
        }
    }
    Err(last_status)
}

async fn read_full_body(response: reqwest::Response) -> Response {
    let status = response.status();
    let ct = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();

    match response.bytes().await {
        Ok(bytes) => {
            let patched = patch_response_content(&bytes);
            let mut resp = Response::new(Body::from(patched));
            *resp.status_mut() = http::StatusCode::from_u16(status.as_u16()).unwrap();
            resp.headers_mut()
                .insert("content-type", HeaderValue::from_str(&ct).unwrap());
            resp
        }
        Err(e) => {
            error!(error = %e, "failed to read upstream body");
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": "failed to read upstream response"
                })),
            )
                .into_response()
        }
    }
}

async fn stream_to_axum(response: reqwest::Response) -> Response {
    let status = response.status();
    let is_sse = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map_or(false, |ct| ct.contains("text/event-stream"));

    if !is_sse {
        return read_full_body(response).await;
    }

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<
        Result<axum::body::Bytes, std::convert::Infallible>,
    >();
    let upstream_stream = response.bytes_stream();

    tokio::spawn(async move {
        use futures::stream::StreamExt;
        let mut s = std::pin::pin!(upstream_stream);
        while let Some(chunk_result) = s.next().await {
            match chunk_result {
                Ok(chunk) => {
                    let patched = patch_sse_line(&chunk);
                    if !patched.is_empty() {
                        let _ = tx.send(Ok(axum::body::Bytes::from(patched)));
                    }
                }
                Err(_) => break,
            }
        }
    });

    let body = Body::from_stream(tokio_stream::wrappers::UnboundedReceiverStream::new(rx));

    let mut resp = Response::new(body);
    *resp.status_mut() = http::StatusCode::from_u16(status.as_u16()).unwrap();
    resp.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("text/event-stream"),
    );
    resp.headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-cache"));
    resp
}
