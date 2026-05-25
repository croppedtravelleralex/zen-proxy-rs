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
use crate::config::ProviderMode;
use crate::ledger::LedgerEvent;
use crate::opencode_headers::{apply_opencode_headers, build_opencode_headers};
use crate::pool::{DispatchError, ErrorKind, RequestMeta};
use crate::sse::SseBuffer;
use crate::state::AppState;
use crate::utils::{
    apply_model_override, build_upstream_url, patch_response_content, should_retry, smart_backoff,
};

pub struct ProxyResult {
    pub response: Response,
    pub body_bytes: Vec<u8>,
    pub retry_count: u32,
    pub was_rate_limited: bool,
    pub pool: String,
    pub upstream_ms: u64,
    pub ttft_ms: u64,
    pub model: String,
    pub node_url: String,
}

fn extract_bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

fn extract_proxy_token(headers: &HeaderMap) -> Option<String> {
    extract_bearer_token(headers)
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        })
        .or_else(|| {
            headers
                .get("api-key")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        })
        .or_else(|| {
            headers
                .get(http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(|s| s.to_string())
        })
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
    let client_id = extract_proxy_token(&headers).unwrap_or_default();
    let conf = state.config.read().unwrap().clone();

    // PROXY_API_KEY 校验
    if let Some(ref key) = conf.proxy_api_key {
        let provided = extract_proxy_token(&headers);
        if provided.as_deref() != Some(key.as_str()) {
            warn!("proxy authentication failed");
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": { "message": "invalid proxy api key" }
                })),
            )
                .into_response();
        }
    }

    if conf.zen_provider_mode == ProviderMode::FreeModelKernel
        && matches!(path.as_str(), "chat/completions" | "messages")
    {
        return crate::v4::provider::handle_v4_proxy(
            &state, &path, &method, &headers, body, &client_id, start,
        )
        .await;
    }
    let (streaming, modified_body) = if body.is_empty() {
        (false, body.to_vec())
    } else {
        let patched = apply_model_override(&body, &conf);
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
        &headers,
        &model,
    )
    .await;

    match result {
        Ok(pr) => {
            let status = pr.response.status().as_u16();
            let latency = start.elapsed().as_millis() as u64;

            let tele = RequestTelemetry {
                rid: uuid::Uuid::new_v4().to_string(),
                ts: chrono::Utc::now().timestamp_millis(),
                model: pr.model.clone(),
                public_model: pr.model.clone(),
                upstream_model: pr.model.clone(),
                protocol: if path == "messages" {
                    "anthropic_messages".to_string()
                } else {
                    "openai_chat_completions".to_string()
                },
                client_id: client_id.clone(),
                path: path.clone(),
                method: method.to_string(),
                is_streaming: streaming,
                node_url: pr.node_url.clone(),
                selected_node_id: String::new(),
                selected_node_url_redacted: LedgerEvent::redact_node_url(&pr.node_url),
                observed_exit_ip: String::new(),
                outcome: if status < 400 { "success" } else { "error" }.to_string(),
                pool: pr.pool.clone(),
                exit_ip: String::new(),
                status,
                rate_limited: pr.was_rate_limited,
                retry_count: pr.retry_count,
                latency_total_ms: latency,
                upstream_ms: pr.upstream_ms,
                ttft_ms: pr.ttft_ms,
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                bytes_sent: body_len,
                bytes_received: pr.body_bytes.len() as u64,
                context: None,
            };

            state.collector.record_request(&tele);
            state.upstream_health.record(status);
            info!(
                method = %method, path = %path,
                status = status,
                duration_ms = latency,
                "proxy OK"
            );
            pr.response
        }
        Err(status) => {
            let elapsed = start.elapsed().as_millis() as u64;
            state.upstream_health.record(status);
            warn!(
                method = %method, path = %path,
                status = status, duration_ms = elapsed,
                "proxy FAIL"
            );

            let (status_code, error_code, message): (StatusCode, i64, String) = match status {
                999 => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    -999,
                    "no proxy resources available".into(),
                ),
                998 => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    -998,
                    "circuit open: upstream rate limit detected".into(),
                ),
                _ => (
                    StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                    status as i64,
                    format!("upstream error {}", status),
                ),
            };

            let retry_after = match status {
                999 => Some(conf.pool_starvation_retry_after_secs.to_string()),
                998 => Some(conf.global_backoff_cooldown_secs.to_string()),
                _ => None,
            };

            let mut resp = (
                status_code,
                Json(serde_json::json!({
                    "error": { "code": error_code, "message": message }
                })),
            )
                .into_response();

            if let Some(secs) = retry_after {
                resp.headers_mut()
                    .insert("Retry-After", HeaderValue::from_str(&secs).unwrap());
            }

            resp
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn proxy_with_retry(
    state: &Arc<AppState>,
    path: &str,
    method: &Method,
    body: &[u8],
    streaming: bool,
    req_meta: &RequestMeta,
    client_id: &str,
    headers: &HeaderMap,
    model: &str,
) -> Result<ProxyResult, u16> {
    let conf = state.config.read().unwrap().clone();
    let max = conf.pool_max_retries.max(1);
    let mut last_status = 502u16;
    let mut was_rate_limited = false;
    let mut last_node_id = String::new();

    for attempt in 0..=max {
        let dispatch_result = if attempt == 0 {
            match state.pool_manager.dispatch(req_meta) {
                Ok(r) => r,
                Err(DispatchError::CircuitOpen) => return Err(998),
                Err(DispatchError::NoResource) => {
                    if attempt < max {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                    return Err(999);
                }
            }
        } else {
            // Sticky retry: try same node first, fall back to fresh dispatch
            let sticky = state.pool_manager.dispatch_sticky(req_meta, &last_node_id);
            match sticky {
                Ok(r) => r,
                Err(_) => match state.pool_manager.dispatch(req_meta) {
                    Ok(r) => r,
                    Err(DispatchError::CircuitOpen) => return Err(998),
                    Err(DispatchError::NoResource) => {
                        if attempt < max {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            continue;
                        }
                        return Err(999);
                    }
                },
            }
        };

        let node_url = dispatch_result.url.clone();
        let node_id = dispatch_result.node.id.clone();
        last_node_id = node_id.clone();
        let client = dispatch_result.client;
        let upstream = build_upstream_url(&conf.upstream_base, &format!("v1/{}", path));
        let request_start = Instant::now();

        let req_method = reqwest::Method::from_bytes(method.as_str().as_bytes())
            .unwrap_or(reqwest::Method::POST);
        let mut upstream_req = client.request(req_method, &upstream);
        upstream_req = upstream_req.header("Content-Type", "application/json");

        // Forward original headers (whitelist)
        for (key, value) in headers.iter() {
            match key.as_str() {
                "content-type" | "content-length" | "host" | "authorization" => continue,
                _ => {
                    upstream_req = upstream_req.header(key, value.clone());
                }
            }
        }

        // Inject opencode headers
        if let Some(opencode_headers) = build_opencode_headers(headers, &conf, client_id, model) {
            upstream_req = apply_opencode_headers(upstream_req, &opencode_headers);
        }

        // Set API key
        upstream_req = upstream_req.header("x-api-key", &conf.upstream_api_key);

        if !body.is_empty() {
            upstream_req = upstream_req.body(body.to_vec());
        }

        match upstream_req.send().await {
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
                        status,
                        retry_after: None,
                        error_type: None,
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
                        pool_from: None,
                        pool_to: None,
                        attempt,
                    });
                    if streaming && status == 200 {
                        let resp = stream_to_axum(up_resp).await;
                        return Ok(ProxyResult {
                            response: resp,
                            body_bytes: Vec::new(),
                            retry_count: attempt,
                            was_rate_limited,
                            pool: "dispatch".into(),
                            upstream_ms: latency,
                            ttft_ms: 0,
                            model: model.to_string(),
                            node_url,
                        });
                    }
                    // Non-streaming success: read body and build response
                    let resp_status = http::StatusCode::from_u16(status).unwrap();
                    let ct = up_resp
                        .headers()
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("application/json")
                        .to_string();
                    let body_bytes = up_resp.bytes().await;
                    match body_bytes {
                        Ok(bytes) => {
                            let patched = patch_response_content(&bytes);
                            let mut resp = Response::new(Body::from(patched));
                            *resp.status_mut() = resp_status;
                            resp.headers_mut()
                                .insert("content-type", HeaderValue::from_str(&ct).unwrap());
                            return Ok(ProxyResult {
                                response: resp,
                                body_bytes: bytes.to_vec(),
                                retry_count: attempt,
                                was_rate_limited,
                                pool: "dispatch".into(),
                                upstream_ms: latency,
                                ttft_ms: 0,
                                model: model.to_string(),
                                node_url,
                            });
                        }
                        Err(e) => {
                            error!(error = %e, "failed to read upstream body");
                            return Err(502);
                        }
                    }
                }

                if status == 429 {
                    was_rate_limited = true;
                    state.pool_manager.report(
                        node_id.clone(),
                        crate::pool::ResultKind::RateLimited,
                        latency,
                    );
                    let _retry_after_secs = up_resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<i64>().ok());
                    let _cf_ray = up_resp
                        .headers()
                        .get("cf-ray")
                        .and_then(|v| v.to_str().ok())
                        .map(|s| s.to_string());
                    state.ledger.record(&LedgerEvent {
                        ts: chrono::Utc::now().timestamp_millis(),
                        rid: uuid::Uuid::new_v4().to_string(),
                        event_type: "rate_limited".into(),
                        node_id: node_id.clone(),
                        node_url_redacted: LedgerEvent::redact_node_url(&node_url),
                        model: model.to_string(),
                        stream: streaming,
                        status,
                        retry_after: up_resp
                            .headers()
                            .get("retry-after")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|v| v.parse::<i64>().ok()),
                        error_type: Some("upstream_429".into()),
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
                        pool_from: Some("dispatch".into()),
                        pool_to: Some("ratelimited".into()),
                        attempt,
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
                        status,
                        retry_after: None,
                        error_type: None,
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
                        pool_from: Some("dispatch".into()),
                        pool_to: None,
                        attempt,
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
                    error_body_summary: None,
                    exit_ip: None,
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
                    pool_from: Some("dispatch".into()),
                    pool_to: None,
                    attempt,
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
        .is_some_and(|ct| ct.contains("text/event-stream"));

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
        let mut sse_buf = SseBuffer::new();
        while let Some(chunk_result) = s.next().await {
            match chunk_result {
                Ok(chunk) => {
                    let lines = sse_buf.push_bytes(&chunk);
                    for line in lines {
                        if !line.is_empty() {
                            let _ = tx.send(Ok(axum::body::Bytes::from(line)));
                        }
                    }
                    if sse_buf.done() {
                        break;
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
