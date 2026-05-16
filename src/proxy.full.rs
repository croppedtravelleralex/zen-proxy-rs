use std::sync::Arc;
use std::time::Instant;

use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures::stream::StreamExt;
use reqwest::Client;
use serde_json::Value;
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::state::AppState;
use crate::utils::{
    apply_model_override, build_upstream_url, patch_response_content, patch_sse_line,
    should_retry, smart_backoff,
};

const MAX_RETRIES: u32 = 3;

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

fn select_upstream_url(state: &AppState, path: &str) -> (Option<String>, String) {
    let proxy_url = state.proxy_selector.next().map(|s| s.to_string());
    let base = proxy_url.as_deref().unwrap_or(&state.config.upstream_base);
    let upstream = build_upstream_url(base, path);
    (proxy_url, upstream)
}

fn clone_headers(src: &reqwest::header::HeaderMap, keys: &[&str]) -> HeaderMap {
    let mut dst = HeaderMap::new();
    for key in keys {
        if let Some(val) = src.get(*key) {
            if let Ok(hv) = HeaderValue::from_bytes(val.as_bytes()) {
                if let Ok(hn) = http::HeaderName::from_static(key) {
                    dst.insert(hn, hv);
                }
            }
        }
    }
    dst
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

    let upstream_headers = build_upstream_headers(&headers, &modified_body);

    let result = proxy_request_with_retry(
        &state, &path, &method, &modified_body, &upstream_headers, streaming,
    )
    .await;

    match result {
        Ok(response) => {
            state.proxy_selector.record_success();
            state.token_bucket.record_success();
            let duration = start.elapsed();
            info!(
                method = %method,
                path = %path,
                duration_ms = duration.as_millis() as u64,
                "proxy request completed"
            );

            if !streaming {
                let (parts, body_bytes) = response.into_parts();
                let patched = patch_response_content(&body_bytes);
                return Response::from_parts(parts, Body::from(patched));
            }
            response
        }
        Err(upstream_status) => {
            state.proxy_selector.record_error();
            state.token_bucket.record_failure();
            let duration = start.elapsed();
            warn!(
                method = %method,
                path = %path,
                status = %upstream_status,
                duration_ms = duration.as_millis() as u64,
                "proxy request failed"
            );
            let err_body = serde_json::json!({
                "error": {
                    "message": format!("Upstream error {}", upstream_status),
                    "type": "upstream_error",
                    "code": upstream_status,
                }
            });
            let code = StatusCode::from_u16(upstream_status).unwrap_or(StatusCode::BAD_GATEWAY);
            (code, Json(err_body)).into_response()
        }
    }
}

async fn proxy_request_with_retry(
    state: &AppState,
    path: &str,
    method: &Method,
    body: &[u8],
    upstream_headers: &[(String, String)],
    streaming: bool,
) -> Result<Response, u16> {
    let max_retries = state.config.pool_max_retries.max(1).min(MAX_RETRIES);
    let mut last_status: u16 = 502;

    for attempt in 0..=max_retries {
        if !state.token_bucket.allow() {
            if attempt < max_retries {
                let backoff = smart_backoff(attempt, Some(429));
                sleep(std::time::Duration::from_secs_f64(backoff)).await;
                continue;
            }
            return Err(429);
        }

        let (_proxy_url, upstream) = select_upstream_url(state, path);
        let client = state.session_pool.get_client(None);

        match build_upstream_request(client, method, &upstream, body, upstream_headers).await {
            Ok(upstream_resp) => {
                let status: u16 = upstream_resp.status().into();
                last_status = status;

                if status < 400 {
                    if streaming && status == 200 {
                        return Ok(stream_to_axum(upstream_resp).await);
                    }
                    return Ok(read_full_response(upstream_resp).await);
                }

                if !should_retry(status, attempt, max_retries) {
                    return Err(status);
                }
                if status == 429 {
                    state.token_bucket.record_429();
                }
                let backoff = smart_backoff(attempt, Some(status));
                sleep(std::time::Duration::from_secs_f64(backoff)).await;
            }
            Err(e) => {
                last_status = 502;
                warn!(attempt, error = %e, "upstream request failed, retrying");
                if attempt < max_retries {
                    let backoff = smart_backoff(attempt, None);
                    sleep(std::time::Duration::from_secs_f64(backoff)).await;
                }
            }
        }
    }
    Err(last_status)
}

async fn build_upstream_request(
    client: Client,
    method: &Method,
    upstream: &str,
    body: &[u8],
    upstream_headers: &[(String, String)],
) -> Result<reqwest::Response, reqwest::Error> {
    let m = reqwest::Method::from_bytes(method.as_str().as_bytes())
        .unwrap_or(reqwest::Method::POST);
    let mut req = client.request(m, upstream);
    for (k, v) in upstream_headers {
        req = req.header(k.as_str(), v.as_str());
    }
    if !body.is_empty() {
        req = req.body(body.to_vec());
    }
    req.send().await
}

fn build_upstream_headers(headers: &HeaderMap, body: &[u8]) -> Vec<(String, String)> {
    let mut result = Vec::new();
    for key in &[
        http::header::AUTHORIZATION,
        http::header::CONTENT_TYPE,
        http::header::ACCEPT,
        http::header::USER_AGENT,
    ] {
        if let Some(val) = headers.get(key).and_then(|v| v.to_str().ok()) {
            result.push((key.as_str().to_string(), val.to_string()));
        }
    }
    if !body.is_empty() && !result.iter().any(|(k, _)| k == "content-type") {
        result.push(("content-type".to_string(), "application/json".to_string()));
    }
    result
}

async fn read_full_response(response: reqwest::Response) -> Response {
    let status: u16 = response.status().into();
    let hdrs = clone_headers(response.headers(), &["content-type", "content-encoding", "cache-control"]);

    match response.bytes().await {
        Ok(bytes) => {
            let patched = patch_response_content(&bytes);
            let mut resp = Response::new(Body::from(patched));
            *resp.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
            *resp.headers_mut() = hdrs;
            resp
        }
        Err(e) => {
            error!(error = %e, "failed to read upstream response");
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": { "message": "Failed to read upstream response" }
                })),
            )
                .into_response()
        }
    }
}

async fn stream_to_axum(response: reqwest::Response) -> Response {
    let status: u16 = response.status().into();
    let is_sse = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map_or(false, |ct| ct.contains("text/event-stream"));

    if !is_sse {
        return read_full_response(response).await;
    }

    let sse_stream = response.bytes_stream().map(|chunk_result| {
        let chunk = match chunk_result {
            Ok(c) => c,
            Err(_) => return Ok(axum::body::Bytes::from_static(b"")),
        };
        let mut patched = patch_sse_line(&chunk);
        if !patched.ends_with(b"\n") {
            patched.extend_from_slice(b"\n");
        }
        Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(patched))
    });

    let body = Body::from_stream(sse_stream);
    let mut resp = Response::new(body);
    *resp.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
    resp.headers_mut().insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    resp.headers_mut().insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    );
    resp.headers_mut().insert(
        http::header::CONNECTION,
        HeaderValue::from_static("keep-alive"),
    );
    resp
}
