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

use crate::state::AppState;
use crate::utils::{
    apply_model_override, build_upstream_url, patch_response_content, patch_sse_line,
    should_retry, smart_backoff,
};

fn extract_bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

fn is_streaming(body: &Value) -> bool {
    body.get("stream").and_then(|v| v.as_bool()).unwrap_or(false)
}

pub async fn proxy_handler(
    State(state): State<Arc<AppState>>,
    method: Method,
    Path(path): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let start = Instant::now();
    let _token = extract_bearer_token(&headers);

    let (streaming, modified_body) = if body.is_empty() {
        (false, body.to_vec())
    } else {
        let patched = apply_model_override(&body, &state.config);
        let parsed: Value = serde_json::from_slice(&patched).unwrap_or(Value::Null);
        (is_streaming(&parsed), patched)
    };

    let body_len = modified_body.len() as u64;
    state.bandwidth.record_bytes(body_len);

    let result = proxy_with_retry(&state, &path, &method, &modified_body, streaming).await;

    match result {
        Ok(resp) => {
            let elapsed = start.elapsed();
            let status_u16 = resp.status().as_u16();
            state.token_bucket.record_success();
            state.upstream_health.record(status_u16);
            state.metrics.record_request(elapsed.as_millis() as u64, body_len, status_u16, false);
            info!(
                method = %method, path = %path,
                status = status_u16,
                duration_ms = elapsed.as_millis(),
                "proxy OK"
            );
            resp
        }
        Err(status) => {
            let elapsed = start.elapsed();
            state.token_bucket.record_failure();
            state.metrics.record_request(elapsed.as_millis() as u64, body_len, status, true);
            if status == 429 {
                state.token_bucket.record_429();
            }
            warn!(
                method = %method, path = %path,
                status = status, duration_ms = elapsed.as_millis(),
                "proxy FAIL"
            );
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
            (code, Json(serde_json::json!({
                "error": { "message": format!("upstream error {}", status) }
            }))).into_response()
        }
    }
}

async fn proxy_with_retry(
    state: &AppState,
    path: &str,
    method: &Method,
    body: &[u8],
    streaming: bool,
) -> Result<Response, u16> {
    let max = state.config.pool_max_retries.max(1);
    let mut last_status = 502u16;

    for attempt in 0..=max {
        if !state.token_bucket.allow() {
            if attempt < max {
                let backoff = smart_backoff(attempt, Some(429));
                tokio::time::sleep(Duration::from_secs_f64(backoff)).await;
                continue;
            }
            return Err(429);
        }

        let node = state.proxy_selector.next();
        let node_url = node.map(|s| s.to_string());
        let node_url_dbg = node_url.as_deref().unwrap_or("direct").to_string();
        let upstream = build_upstream_url(&state.config.upstream_base, &format!("v1/{}", path));

        let client = state.session_pool.get_client(node_url.as_deref());

        let req_method = reqwest::Method::from_bytes(method.as_str().as_bytes())
            .unwrap_or(reqwest::Method::POST);
        let mut req = client.request(req_method, &upstream);
        req = req.header("Content-Type", "application/json");
        req = req.header("x-api-key", "public");
        if !body.is_empty() {
            req = req.body(body.to_vec());
        }
        state.metrics.record_bytes_sent(body.len() as u64);

        match req.send().await {
            Ok(up_resp) => {
                let status = up_resp.status().as_u16();
                last_status = status;

                state.upstream_health.record(status);

                if status < 400 {
                    state.metrics.record_node_success();
                    state.node_db.record(&node_url_dbg, true, 0);
                    if streaming && status == 200 {
                        return Ok(stream_to_axum(up_resp).await);
                    }
                    return Ok(read_full_body(up_resp).await);
                }

                state.node_db.record(&node_url_dbg, false, 0);
                state.metrics.record_node_error();

                if !should_retry(status, attempt, max) {
                    return Err(status);
                }
                if status == 429 {
                    state.token_bucket.record_429();
                }
                let backoff = smart_backoff(attempt, Some(status));
                tokio::time::sleep(Duration::from_secs_f64(backoff)).await;
            }
            Err(e) => {
                last_status = 502;
                state.metrics.record_timeout();
                state.node_db.record(&node_url_dbg, false, 0);
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
            (StatusCode::BAD_GATEWAY, Json(serde_json::json!({
                "error": "failed to read upstream response"
            }))).into_response()
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

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<axum::body::Bytes, std::convert::Infallible>>();
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
    resp.headers_mut()
        .insert("content-type", HeaderValue::from_static("text/event-stream"));
    resp.headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-cache"));
    resp
}
