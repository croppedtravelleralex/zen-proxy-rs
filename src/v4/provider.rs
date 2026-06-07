use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{to_bytes, Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use free_model_client_rs::client_profile::{ClientKind, ClientProfile, ClientProfileSource};
use free_model_client_rs::error::{AppError, UpstreamErrorKind};
use free_model_client_rs::kernel::{FreeModelKernel, KernelConfig};
use free_model_client_rs::protocol::types::{AnthropicRequest, ChatRequest};
use futures::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::collector::{
    DataCollector, ProtocolGuardTelemetry, RequestAttemptTelemetry, RequestTelemetry,
    RequestTimings,
};
use crate::config::Config;
use crate::ledger::LedgerEvent;
use crate::pool::{body_size_bucket, DispatchError, ErrorKind, RequestMeta, ResultKind};
use crate::state::AppState;
use crate::v4::context;
use crate::v4::model::{ModelError, ModelRegistry, StaticModelRegistry};
use crate::v4::protocol_guard::{self, GuardPhase};

const MAX_PROVIDER_RESPONSE_BODY_BYTES: usize = 32 * 1024 * 1024;
const STREAM_DOWNSTREAM_SEND_TIMEOUT_SECS: u64 = 30;

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
    let mut parsed: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(err) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!("request body must be valid JSON: {err}"),
            );
        }
    };
    let source_client = infer_source_client(path, headers, &parsed);
    let mut protocol_guard_summary: Option<ProtocolGuardTelemetry> = None;
    let raw_has_tool_markers = protocol_guard::raw_body_has_tool_markers(&body);
    match protocol_guard::guard_body(
        &conf,
        path,
        &mut parsed,
        &source_client,
        GuardPhase::PreCompact,
        raw_has_tool_markers,
    ) {
        Ok(summary) => merge_protocol_guard_summary(&mut protocol_guard_summary, summary),
        Err(reject) => return error_response(reject.status, reject.message),
    }

    let context_plan = match context::govern_request(&conf, path, parsed, body.len()) {
        Ok(plan) => plan,
        Err(reject) => return error_response(reject.status, reject.message),
    };
    let external_request_id = extract_external_request_id(headers);
    let gateway = infer_gateway(headers, &external_request_id);
    let mut context_telemetry = context_plan.telemetry();
    let mut parsed = context_plan.body;
    let force_final_guard = protocol_guard_summary
        .as_ref()
        .is_some_and(|summary| summary.applied || summary.pre_invalid)
        || context_telemetry.trimmed;
    match protocol_guard::guard_body(
        &conf,
        path,
        &mut parsed,
        &source_client,
        GuardPhase::PostCompact,
        force_final_guard,
    ) {
        Ok(summary) => merge_protocol_guard_summary(&mut protocol_guard_summary, summary),
        Err(reject) => return error_response(reject.status, reject.message),
    }

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
        source_client = %source_client,
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
    let nonstream_guard = apply_nonstream_output_guard(path, &upstream_body);
    if nonstream_guard.applied() {
        context_telemetry.trace.push(nonstream_guard.trace_line());
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
        affinity_key: build_affinity_key(
            &public_model,
            path,
            client_id,
            effective_body_len,
            streaming,
        ),
    };
    let request_body_bucket = body_size_bucket(effective_body_len).to_string();
    let request_affinity_key = request_meta.affinity_key.clone();
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
        request_meta.clone(),
        upstream_body,
        UpstreamCallContext {
            public_model: &public_model,
            upstream_model: &resolved.upstream_model,
            source_client: &source_client,
        },
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
                timings.protocol_first_byte_ms = timings.first_chunk_ms;
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
                affinity_key: request_affinity_key.clone(),
                affinity_hit: result.affinity_hit,
                affinity_node_id: result.affinity_node_id.clone(),
                body_size_bucket: request_body_bucket.clone(),
                prompt_tokens: result.usage.prompt_tokens,
                completion_tokens: result.usage.completion_tokens,
                total_tokens: result.usage.total_tokens,
                cached_tokens: result.usage.cached_tokens,
                cache_creation_input_tokens: result.usage.cache_creation_input_tokens,
                cache_read_input_tokens: result.usage.cache_read_input_tokens,
                bytes_sent: effective_body_len,
                bytes_received: result.body_bytes_len,
                failure_kind: String::new(),
                failure_message: String::new(),
                retry_chain: result.retry_chain,
                context: Some(context_telemetry.clone()),
                protocol_guard: protocol_guard_summary.clone(),
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
                if !telemetry.affinity_key.is_empty() && status < 400 {
                    state.pool_manager.record_affinity_success(
                        &telemetry.affinity_key,
                        telemetry.selected_node_id.clone(),
                    );
                    state.pool_manager.record_bucket_latency_hint(
                        telemetry.selected_node_id.clone(),
                        &telemetry.body_size_bucket,
                        telemetry.ttft_ms.max(result.upstream_ms),
                    );
                }
                state.collector.record_request(&telemetry);
                result.response
            };
            response.headers_mut().insert(
                "x-zen-stream-seen",
                HeaderValue::from_static(if streaming { "true" } else { "false" }),
            );
            insert_nonstream_guard_headers(response.headers_mut(), &nonstream_guard);
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
                    affinity_key: request_affinity_key.clone(),
                    affinity_hit: false,
                    affinity_node_id: String::new(),
                    body_size_bucket: request_body_bucket.clone(),
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                    cached_tokens: 0,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    bytes_sent: effective_body_len,
                    bytes_received: 0,
                    failure_kind: err.failure_kind.clone(),
                    failure_message: err.message.clone(),
                    retry_chain: err.retry_chain.clone(),
                    context: Some(context_telemetry.clone()),
                    protocol_guard: protocol_guard_summary.clone(),
                });
            }
            let mut response = error_response(err.status, err.message);
            if let Some(retry_after) = err.retry_after_secs {
                response.headers_mut().insert(
                    "retry-after",
                    HeaderValue::from_str(&retry_after.to_string()).unwrap(),
                );
            }
            insert_nonstream_guard_headers(response.headers_mut(), &nonstream_guard);
            insert_context_headers(response.headers_mut(), &context_telemetry);
            response
        }
    }
}

#[derive(Debug, Clone, Default)]
struct NonStreamGuardDecision {
    action: &'static str,
    prompt_tokens: u32,
    max_tokens_before: Option<u64>,
    max_tokens_after: Option<u64>,
}

impl NonStreamGuardDecision {
    fn applied(&self) -> bool {
        self.action != "pass"
    }

    fn trace_line(&self) -> String {
        format!(
            "nonstream_guard action={} prompt_tokens={} max_tokens_before={} max_tokens_after={}",
            self.action,
            self.prompt_tokens,
            self.max_tokens_before
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            self.max_tokens_after
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string())
        )
    }
}

fn apply_nonstream_output_guard(path: &str, body: &Value) -> NonStreamGuardDecision {
    let streaming = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    if streaming || !matches!(path, "chat/completions" | "messages") {
        return NonStreamGuardDecision {
            action: "pass",
            ..NonStreamGuardDecision::default()
        };
    }

    let prompt_tokens = estimate_prompt_tokens(path, body);
    let max_tokens_before = body
        .get("max_tokens")
        .or_else(|| body.get("max_completion_tokens"))
        .and_then(Value::as_u64);

    NonStreamGuardDecision {
        action: "pass",
        prompt_tokens,
        max_tokens_before,
        max_tokens_after: body.get("max_tokens").and_then(Value::as_u64),
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

fn build_affinity_key(
    public_model: &str,
    path: &str,
    client_id: &str,
    body_size: u64,
    streaming: bool,
) -> String {
    if !streaming || body_size < 128 * 1024 {
        return String::new();
    }
    let client_bucket = if client_id.trim().is_empty() {
        "anon".to_string()
    } else {
        LedgerEvent::short_hash(client_id)
    };
    format!(
        "{}:{}:{}:{}",
        public_model,
        path,
        client_bucket,
        body_size_bucket(body_size)
    )
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

fn infer_source_client(path: &str, headers: &HeaderMap, body: &Value) -> String {
    if let Some(value) = extract_header(headers, "x-fmc-client")
        .or_else(|| extract_header(headers, "x-zen-source-client"))
        .or_else(|| extract_header(headers, "x-client-name"))
    {
        return normalize_source_client(&value);
    }

    if let Some(value) = infer_source_client_from_body(body) {
        return value.to_string();
    }

    if let Some(value) = extract_header(headers, "x-stainless-package-version") {
        let normalized = normalize_source_client(&value);
        if normalized != "unknown" {
            return normalized;
        }
    }
    let user_agent = extract_header(headers, "user-agent").unwrap_or_default();
    let normalized_user_agent = normalize_source_client(&user_agent);
    if normalized_user_agent != "unknown" {
        return normalized_user_agent;
    }

    if path == "messages" {
        return "claude-code".to_string();
    }

    "unknown".to_string()
}

fn infer_source_client_from_body(body: &Value) -> Option<&'static str> {
    if body_contains_strong_client_marker(body, "openclaw") {
        return Some("openclaw");
    }
    if body_contains_strong_client_marker(body, "hermes") {
        return Some("hermes");
    }

    let tool_names = body
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flat_map(|tools| tools.iter())
        .filter_map(tool_name_from_value)
        .map(normalize_tool_name)
        .collect::<Vec<_>>();

    if tool_names
        .iter()
        .any(|name| is_openclaw_strong_tool_name(name))
    {
        return Some("openclaw");
    }

    if tool_names.iter().any(|name| name.contains("hermes")) {
        return Some("hermes");
    }

    if tool_names.iter().any(|name| {
        matches!(
            name.as_str(),
            "task"
                | "bash"
                | "read"
                | "edit"
                | "multiedit"
                | "write"
                | "todowrite"
                | "grep"
                | "glob"
                | "ls"
        )
    }) {
        return Some("claude-code");
    }

    None
}

fn tool_name_from_value(tool: &Value) -> Option<&str> {
    tool.get("function")
        .and_then(|function| function.get("name"))
        .and_then(Value::as_str)
        .or_else(|| tool.get("name").and_then(Value::as_str))
}

fn normalize_tool_name(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn body_contains_strong_client_marker(value: &Value, marker: &str) -> bool {
    match value {
        Value::String(text) => {
            let lower = text.to_ascii_lowercase();
            match marker {
                "openclaw" => contains_strong_openclaw_marker(&lower),
                "hermes" => contains_strong_hermes_marker(&lower),
                _ => false,
            }
        }
        Value::Array(items) => items
            .iter()
            .any(|item| body_contains_strong_client_marker(item, marker)),
        Value::Object(map) => map
            .values()
            .any(|item| body_contains_strong_client_marker(item, marker)),
        _ => false,
    }
}

fn contains_strong_openclaw_marker(lower: &str) -> bool {
    lower.contains("running inside openclaw")
        || lower.contains("openclaw cli")
        || lower.contains("openclaw agent")
        || lower.contains("openclaw_config")
        || lower.contains("openclaw-config")
}

fn contains_strong_hermes_marker(lower: &str) -> bool {
    lower.contains("running inside hermes")
        || lower.contains("hermes cli")
        || lower.contains("hermes agent")
        || lower.contains("hermes_config")
        || lower.contains("hermes-config")
}

fn is_openclaw_strong_tool_name(name: &str) -> bool {
    matches!(
        name,
        "subagents"
            | "sessionsspawn"
            | "sessionssend"
            | "sessionsyield"
            | "sessionstatus"
            | "sessionsstatus"
            | "sessionshistory"
            | "sessionslist"
            | "memoryget"
            | "memorysearch"
    ) || name.contains("openclaw")
}

fn normalize_source_client(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.contains("openclaw") {
        "openclaw".to_string()
    } else if lower.contains("hermes") {
        "hermes".to_string()
    } else if lower.contains("claude") {
        "claude-code".to_string()
    } else if lower.contains("cherrystudio") || lower.contains("cherry studio") {
        "cherrystudio".to_string()
    } else if lower.contains("anthropic") {
        "anthropic-sdk".to_string()
    } else if lower.contains("openai") {
        "openai-sdk".to_string()
    } else {
        "unknown".to_string()
    }
}

fn profile_for_openai_request(source_client: &str, request: &ChatRequest) -> ClientProfile {
    profile_from_source_client(source_client)
        .unwrap_or_else(|| ClientProfile::from_openai(&HeaderMap::new(), request))
}

fn profile_for_anthropic_request(source_client: &str, request: &AnthropicRequest) -> ClientProfile {
    profile_from_source_client(source_client)
        .unwrap_or_else(|| ClientProfile::from_anthropic(&HeaderMap::new(), request))
}

fn profile_from_source_client(source_client: &str) -> Option<ClientProfile> {
    let kind = match normalize_source_client(source_client).as_str() {
        "claude-code" => ClientKind::ClaudeCode,
        "hermes" => ClientKind::Hermes,
        "openclaw" => ClientKind::OpenClaw,
        "cherrystudio" => ClientKind::CherryStudio,
        "anthropic-sdk" => ClientKind::AnthropicSdk,
        "openai-sdk" => ClientKind::OpenAiSdk,
        _ => return None,
    };
    Some(ClientProfile::new(kind, ClientProfileSource::Header))
}

fn merge_protocol_guard_summary(
    target: &mut Option<ProtocolGuardTelemetry>,
    summary: ProtocolGuardTelemetry,
) {
    if !summary.applied && !summary.pre_invalid {
        return;
    }
    match target {
        Some(existing) => existing.merge(summary),
        None => *target = Some(summary),
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
    timings: RequestTimings,
    affinity_hit: bool,
    affinity_node_id: String,
    retry_chain: Vec<RequestAttemptTelemetry>,
    body_bytes_len: u64,
    usage: UsageCounts,
}

#[derive(Debug, Clone, Copy, Default)]
struct UsageCounts {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
    cached_tokens: u32,
    cache_creation_input_tokens: u32,
    cache_read_input_tokens: u32,
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

struct UpstreamCallContext<'a> {
    public_model: &'a str,
    upstream_model: &'a str,
    source_client: &'a str,
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
    call_context: UpstreamCallContext<'_>,
) -> Result<V4CallResult, V4CallError> {
    let public_model = call_context.public_model;
    let upstream_model = call_context.upstream_model;
    let source_client = call_context.source_client;
    let base_max = conf.pool_max_retries;
    let empty_upstream_max = conf.v4_empty_upstream_max_retries.max(base_max);
    let mut last_status = StatusCode::BAD_GATEWAY;
    let mut was_rate_limited = false;
    let mut dispatch_wait_ms = 0u64;
    let mut retry_chain = Vec::new();
    let retry_budget_ms = conf.v4_retry_budget_ms;

    for attempt in 0..=empty_upstream_max {
        let dispatch_start = Instant::now();
        let dispatch_result =
            dispatch_or_wait(state, &request_meta, attempt, empty_upstream_max).await?;
        dispatch_wait_ms =
            dispatch_wait_ms.saturating_add(dispatch_start.elapsed().as_millis() as u64);

        let node_id = dispatch_result.node.id.clone();
        let node_url = dispatch_result.url.clone();
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
            true_first_token_frt: conf.free_model_true_first_token_frt,
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
                let profile = profile_for_openai_request(source_client, &request);
                kernel
                    .openai_chat_with_profile(&dispatch_result.client, request, profile)
                    .await
            }
            "messages" => {
                let request = serde_json::from_value::<AnthropicRequest>(upstream_body.clone())
                    .map_err(|err| {
                        V4CallError::before_dispatch(
                            StatusCode::BAD_REQUEST,
                            format!("invalid Anthropic messages request: {err}"),
                        )
                    })?;
                let profile = profile_for_anthropic_request(source_client, &request);
                kernel
                    .anthropic_messages_with_profile(&dispatch_result.client, request, profile)
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
                            ResultKind::EmptyOutput,
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
                        let elapsed_ms = retry_chain_latency_ms(&retry_chain);
                        if retry_budget_ms > 0 && elapsed_ms >= retry_budget_ms {
                            return Err(V4CallError::after_dispatch(
                                StatusCode::BAD_GATEWAY,
                                retry_budget_message(
                                    elapsed_ms,
                                    StatusCode::BAD_GATEWAY,
                                    "empty_output",
                                    &retry_chain,
                                ),
                                None,
                                request_id,
                                node_id,
                                &node_url,
                                upstream_model,
                                "retry_budget_exhausted",
                                attempt,
                                was_rate_limited,
                                latency,
                                "retry_budget_exhausted",
                                retry_chain,
                            ));
                        }
                        if attempt >= empty_upstream_max {
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
                            protocol_first_byte_ms: if request_meta.stream { 0 } else { latency },
                            stream_complete_ms: if request_meta.stream { 0 } else { latency },
                            total_ms: latency,
                            ..RequestTimings::default()
                        },
                        affinity_hit: dispatch_result.affinity_hit,
                        affinity_node_id: dispatch_result.affinity_node_id,
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
                if attempt >= base_max {
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
                    let result = result_kind_for_classified_error(error_kind, error_type);
                    state.pool_manager.report(node_id.clone(), result, latency);
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
                let elapsed_ms = retry_chain_latency_ms(&retry_chain);
                if retry_budget_ms > 0 && elapsed_ms >= retry_budget_ms {
                    return Err(V4CallError::after_dispatch(
                        last_status,
                        retry_budget_message(
                            elapsed_ms,
                            last_status,
                            "provider_error",
                            &retry_chain,
                        ),
                        retry_after,
                        request_id,
                        node_id,
                        &node_url,
                        upstream_model,
                        "retry_budget_exhausted",
                        attempt,
                        was_rate_limited || status == StatusCode::TOO_MANY_REQUESTS,
                        latency,
                        "retry_budget_exhausted",
                        retry_chain,
                    ));
                }
                let max_for_error = max_retries_for_app_error(&err, base_max, empty_upstream_max);
                if attempt >= max_for_error {
                    let (error_kind, outcome, error_type) = classify_app_error(&err);
                    let outcome = if status == StatusCode::TOO_MANY_REQUESTS {
                        "rate_limited"
                    } else if is_upstream_busy(status, &err.message) {
                        "upstream_busy"
                    } else if is_empty_upstream_error(&err) {
                        "empty_output"
                    } else if error_type == "provider_invalid_request" {
                        outcome
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

        let elapsed_ms = retry_chain_latency_ms(&retry_chain);
        if retry_budget_ms > 0 && elapsed_ms >= retry_budget_ms {
            return Err(V4CallError::after_dispatch(
                last_status,
                retry_budget_message(elapsed_ms, last_status, "provider_error", &retry_chain),
                None,
                uuid::Uuid::new_v4().to_string(),
                String::new(),
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

fn retry_chain_latency_ms(retry_chain: &[RequestAttemptTelemetry]) -> u64 {
    retry_chain.iter().map(|attempt| attempt.latency_ms).sum()
}

fn retry_budget_message(
    elapsed_ms: u64,
    status: StatusCode,
    category: &str,
    retry_chain: &[RequestAttemptTelemetry],
) -> String {
    let last_error = retry_chain
        .last()
        .map(|attempt| attempt.error_type.as_str())
        .filter(|value| !value.is_empty())
        .unwrap_or(category);
    let attempts = retry_chain.len();
    format!(
        "upstream retry budget exhausted after {elapsed_ms}ms with status {} ({category}; last_error={last_error}; attempts={attempts})",
        status.as_u16()
    )
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
            ResultKind::SoftFailure {
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
    if is_provider_invalid_request_error(err) {
        return (
            ErrorKind::Other,
            "upstream_error",
            "provider_invalid_request",
        );
    }
    if is_empty_upstream_message(&message) {
        return (ErrorKind::Other, "empty_output", "empty_output");
    }
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

fn result_kind_for_classified_error(error_kind: ErrorKind, error_type: &str) -> ResultKind {
    if error_type == "provider_invalid_request" {
        return ResultKind::Success(400);
    }

    if error_type == "empty_output" {
        return ResultKind::EmptyOutput;
    }

    if is_transport_error_type(error_type) {
        return ResultKind::Error { kind: error_kind };
    }

    match error_kind {
        ErrorKind::Timeout
        | ErrorKind::ConnectionRefused
        | ErrorKind::DnsFailure
        | ErrorKind::SocksHandshake => ResultKind::Error { kind: error_kind },
        ErrorKind::Upstream5xx | ErrorKind::Other => ResultKind::SoftFailure { kind: error_kind },
    }
}

fn is_transport_error_type(error_type: &str) -> bool {
    matches!(
        error_type,
        "timeout" | "network" | "connection_refused" | "dns_failure" | "socks_handshake"
    )
}

fn is_empty_upstream_error(err: &AppError) -> bool {
    is_empty_upstream_message(&err.message.to_ascii_lowercase())
}

fn is_provider_invalid_request_error(err: &AppError) -> bool {
    err.upstream_error_kind == Some(UpstreamErrorKind::ProviderInvalidRequest)
}

fn max_retries_for_app_error(err: &AppError, base_max: u32, empty_upstream_max: u32) -> u32 {
    if is_provider_invalid_request_error(err) {
        0
    } else if is_empty_upstream_error(err) {
        empty_upstream_max
    } else {
        base_max
    }
}

fn is_empty_upstream_message(message: &str) -> bool {
    message.contains("no assistant content or tool call")
        || message.contains("upstream returned no assistant content")
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

fn insert_nonstream_guard_headers(headers: &mut HeaderMap, decision: &NonStreamGuardDecision) {
    if let Ok(value) = HeaderValue::from_str(decision.action) {
        headers.insert("x-zen-nonstream-guard-action", value);
    }
    if let Ok(value) = HeaderValue::from_str(&decision.prompt_tokens.to_string()) {
        headers.insert("x-zen-nonstream-prompt-tokens", value);
    }
    if let Some(max_tokens) = decision.max_tokens_before {
        if let Ok(value) = HeaderValue::from_str(&max_tokens.to_string()) {
            headers.insert("x-zen-nonstream-original-max-tokens", value);
        }
    }
    if let Some(max_tokens) = decision.max_tokens_after {
        if let Ok(value) = HeaderValue::from_str(&max_tokens.to_string()) {
            headers.insert("x-zen-nonstream-max-tokens", value);
        }
    }
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
    let bytes = to_bytes(response.into_body(), MAX_PROVIDER_RESPONSE_BODY_BYTES)
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
    let bytes = to_bytes(response.into_body(), MAX_PROVIDER_RESPONSE_BODY_BYTES)
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
    let (tx, rx) = mpsc::channel::<Result<Bytes, Infallible>>(16);

    tokio::spawn(async move {
        let mut telemetry = telemetry;
        let mut lease_guard =
            StreamLeaseGuard::new(state.clone(), telemetry.selected_node_id.clone());
        let mut metrics = StreamMetrics::new(fallback_usage);
        let mut first_chunk_ms = 0u64;
        let mut first_content_token_ms = 0u64;
        let mut first_tool_call_ms = 0u64;
        let mut stream_error: Option<String> = None;
        let mut client_gone = false;
        let mut client_gone_reason = "client disconnected before stream completed".to_string();
        while let Some(item) = upstream.next().await {
            match item {
                Ok(bytes) => {
                    if first_chunk_ms == 0 {
                        first_chunk_ms = request_start.elapsed().as_millis() as u64;
                        state.pool_manager.record_latency_hint(
                            telemetry.selected_node_id.clone(),
                            first_chunk_ms,
                        );
                        state.pool_manager.record_bucket_latency_hint(
                            telemetry.selected_node_id.clone(),
                            &telemetry.body_size_bucket,
                            first_chunk_ms,
                        );
                    }
                    let had_content = metrics.has_content_signal();
                    let had_tool = metrics.has_tool_signal();
                    metrics.ingest(&path, &bytes);
                    let elapsed_ms = request_start.elapsed().as_millis() as u64;
                    if first_content_token_ms == 0 && !had_content && metrics.has_content_signal() {
                        first_content_token_ms = elapsed_ms;
                    }
                    if first_tool_call_ms == 0 && !had_tool && metrics.has_tool_signal() {
                        first_tool_call_ms = elapsed_ms;
                    }
                    match send_stream_bytes(&tx, bytes).await {
                        Ok(()) => {}
                        Err(StreamSendError::Closed) => {
                            client_gone = true;
                            client_gone_reason =
                                "client disconnected before stream completed".to_string();
                            break;
                        }
                        Err(StreamSendError::Timeout) => {
                            client_gone = true;
                            client_gone_reason = format!(
                                "downstream stream backpressure exceeded {}s",
                                STREAM_DOWNSTREAM_SEND_TIMEOUT_SECS
                            );
                            break;
                        }
                    }
                }
                Err(err) => {
                    let kind = classify_stream_body_error(&err);
                    let message = format!("upstream stream error ({kind}): {err}");
                    match send_stream_bytes(&tx, stream_error_frame(&path, &message)).await {
                        Ok(()) => {}
                        Err(StreamSendError::Closed) => {
                            client_gone = true;
                            client_gone_reason =
                                "client disconnected before stream error frame".to_string();
                        }
                        Err(StreamSendError::Timeout) => {
                            client_gone = true;
                            client_gone_reason = format!(
                                "downstream stream backpressure exceeded {}s before error frame",
                                STREAM_DOWNSTREAM_SEND_TIMEOUT_SECS
                            );
                        }
                    }
                    stream_error = Some(message);
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
        telemetry.cached_tokens = usage.cached_tokens;
        telemetry.cache_creation_input_tokens = usage.cache_creation_input_tokens;
        telemetry.cache_read_input_tokens = usage.cache_read_input_tokens;
        telemetry.latency_total_ms = stream_complete_ms;
        telemetry.ttft_ms = first_chunk_ms;
        telemetry.timings.first_chunk_ms = first_chunk_ms;
        telemetry.timings.protocol_first_byte_ms = first_chunk_ms;
        telemetry.timings.first_content_token_ms = first_content_token_ms;
        telemetry.timings.first_tool_call_ms = first_tool_call_ms;
        telemetry.timings.stream_complete_ms = stream_complete_ms;
        telemetry.timings.total_ms = stream_complete_ms;
        let empty_output = stream_error.is_none()
            && usage.completion_tokens == 0
            && !metrics.has_assistant_output();
        if client_gone {
            telemetry.outcome = "client_gone".to_string();
            telemetry.failure_kind = "client_gone".to_string();
            telemetry.failure_message = client_gone_reason;
            telemetry.retry_chain.push(RequestAttemptTelemetry {
                attempt: telemetry.retry_count,
                node_id: telemetry.selected_node_id.clone(),
                node_url_redacted: telemetry.selected_node_url_redacted.clone(),
                status: 499,
                latency_ms: stream_complete_ms,
                outcome: "client_gone".to_string(),
                error_type: "client_gone".to_string(),
            });
            lease_guard.release(ResultKind::ClientGone, stream_complete_ms);
        } else if let Some(message) = stream_error {
            let error_type = classify_stream_error_message(&message).to_string();
            telemetry.outcome = "stream_error".to_string();
            telemetry.failure_kind = error_type.clone();
            telemetry.failure_message = message;
            telemetry.retry_chain.push(RequestAttemptTelemetry {
                attempt: telemetry.retry_count,
                node_id: telemetry.selected_node_id.clone(),
                node_url_redacted: telemetry.selected_node_url_redacted.clone(),
                status: telemetry.status,
                latency_ms: stream_complete_ms,
                outcome: "stream_error".to_string(),
                error_type,
            });
            state.pool_manager.report(
                telemetry.selected_node_id.clone(),
                ResultKind::SoftFailure {
                    kind: ErrorKind::Other,
                },
                stream_complete_ms,
            );
            lease_guard.mark_released();
        } else if empty_output {
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
                ResultKind::EmptyOutput,
                stream_complete_ms,
            );
            lease_guard.mark_released();
        } else {
            if !telemetry.affinity_key.is_empty() {
                state.pool_manager.record_affinity_success(
                    &telemetry.affinity_key,
                    telemetry.selected_node_id.clone(),
                );
            }
            state.pool_manager.report(
                telemetry.selected_node_id.clone(),
                ResultKind::Success(telemetry.status),
                stream_complete_ms,
            );
            lease_guard.mark_released();
        }
        collector.record_request(&telemetry);
    });

    let mut rebuilt = Response::new(Body::from_stream(ReceiverStream::new(rx)));
    *rebuilt.status_mut() = status;
    *rebuilt.headers_mut() = headers;
    rebuilt
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamSendError {
    Closed,
    Timeout,
}

async fn send_stream_bytes(
    tx: &mpsc::Sender<Result<Bytes, Infallible>>,
    bytes: Bytes,
) -> Result<(), StreamSendError> {
    match tokio::time::timeout(
        Duration::from_secs(STREAM_DOWNSTREAM_SEND_TIMEOUT_SECS),
        tx.send(Ok(bytes)),
    )
    .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(StreamSendError::Closed),
        Err(_) => Err(StreamSendError::Timeout),
    }
}

struct StreamLeaseGuard {
    state: Arc<AppState>,
    node_id: String,
    released: bool,
}

impl StreamLeaseGuard {
    fn new(state: Arc<AppState>, node_id: String) -> Self {
        Self {
            state,
            node_id,
            released: false,
        }
    }

    fn release(&mut self, result: ResultKind, latency_ms: u64) {
        if self.released || self.node_id.is_empty() {
            return;
        }
        self.state
            .pool_manager
            .report(self.node_id.clone(), result, latency_ms);
        self.released = true;
    }

    fn mark_released(&mut self) {
        self.released = true;
    }
}

impl Drop for StreamLeaseGuard {
    fn drop(&mut self) {
        if self.released || self.node_id.is_empty() {
            return;
        }
        tracing::warn!(
            node_id = %self.node_id,
            "stream lease guard released leaked stream lease"
        );
        self.state
            .pool_manager
            .report(self.node_id.clone(), ResultKind::ClientGone, 0);
        self.released = true;
    }
}

fn stream_error_frame(path: &str, message: &str) -> Bytes {
    let escaped = serde_json::to_string(message).unwrap_or_else(|_| "\"stream error\"".to_string());
    if path == "messages" {
        Bytes::from(format!(
            "event: error\ndata: {{\"type\":\"error\",\"error\":{{\"type\":\"api_error\",\"message\":{escaped}}}}}\n\n"
        ))
    } else {
        Bytes::from(format!(
            "data: {{\"error\":{{\"message\":{escaped}}}}}\n\ndata: [DONE]\n\n"
        ))
    }
}

fn classify_stream_body_error(err: &axum::Error) -> &'static str {
    classify_stream_error_message(&err.to_string())
}

fn classify_stream_error_message(message: &str) -> &'static str {
    let lower = message.to_ascii_lowercase();
    if lower.contains("decode") || lower.contains("decoding") {
        "stream_decode_error"
    } else if lower.contains("timeout") || lower.contains("elapsed") {
        "stream_timeout"
    } else if lower.contains("connection") || lower.contains("closed") || lower.contains("reset") {
        "stream_connection_error"
    } else {
        "stream_error"
    }
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
                Some("content_block_start")
                    if value
                        .get("content_block")
                        .and_then(|block| block.get("type"))
                        .and_then(Value::as_str)
                        .is_some_and(|kind| kind == "tool_use") =>
                {
                    self.tool_output_chunks = self.tool_output_chunks.saturating_add(1);
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
                self.usage.cache_creation_input_tokens =
                    usage_u32(usage, "cache_creation_input_tokens");
                self.usage.cache_read_input_tokens = usage_u32(usage, "cache_read_input_tokens");
                self.usage.cached_tokens = self.usage.cache_read_input_tokens;
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
        self.usage.cached_tokens = usage
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                usage
                    .get("cached_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            })
            .min(u32::MAX as u64) as u32;
        self.usage.cache_creation_input_tokens = usage_u32(usage, "cache_creation_input_tokens");
        self.usage.cache_read_input_tokens =
            usage_u32(usage, "cache_read_input_tokens").max(self.usage.cached_tokens);
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
            cached_tokens: self
                .usage
                .cached_tokens
                .max(self.fallback_usage.cached_tokens),
            cache_creation_input_tokens: self
                .usage
                .cache_creation_input_tokens
                .max(self.fallback_usage.cache_creation_input_tokens),
            cache_read_input_tokens: self
                .usage
                .cache_read_input_tokens
                .max(self.fallback_usage.cache_read_input_tokens),
        }
    }

    fn has_assistant_output(&self) -> bool {
        !self.completion_text.trim().is_empty()
            || self.text_output_chunks > 0
            || self.tool_output_chunks > 0
            || self.usage.completion_tokens > 0
    }

    fn has_content_signal(&self) -> bool {
        !self.completion_text.trim().is_empty() || self.text_output_chunks > 0
    }

    fn has_tool_signal(&self) -> bool {
        self.tool_output_chunks > 0
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
            cached_tokens: usage_u32(usage, "cache_read_input_tokens"),
            cache_creation_input_tokens: usage_u32(usage, "cache_creation_input_tokens"),
            cache_read_input_tokens: usage_u32(usage, "cache_read_input_tokens"),
        }
    } else {
        let cached_tokens = usage
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                usage
                    .get("cached_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            })
            .min(u32::MAX as u64) as u32;
        UsageCounts {
            prompt_tokens: usage_u32(usage, "prompt_tokens"),
            completion_tokens: usage_u32(usage, "completion_tokens"),
            total_tokens: usage_u32(usage, "total_tokens"),
            cached_tokens,
            cache_creation_input_tokens: usage_u32(usage, "cache_creation_input_tokens"),
            cache_read_input_tokens: usage_u32(usage, "cache_read_input_tokens").max(cached_tokens),
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
    fn infers_openclaw_from_body_before_generic_openai_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("user-agent", "OpenAI/JS 6.38.0".parse().unwrap());
        let body = serde_json::json!({
            "model": "deepseek-v4-flash",
            "messages": [
                {"role": "system", "content": "You are a personal assistant running inside OpenClaw."},
                {"role": "user", "content": "use subagent"}
            ],
            "tools": [
                {"type": "function", "function": {"name": "read"}},
                {"type": "function", "function": {"name": "subagents"}},
                {"type": "function", "function": {"name": "sessions_spawn"}}
            ]
        });

        assert_eq!(infer_source_client("messages", &headers, &body), "openclaw");
    }

    #[test]
    fn infers_claude_code_when_only_claude_tool_names_exist() {
        let headers = HeaderMap::new();
        let body = serde_json::json!({
            "model": "deepseek-v4-flash",
            "messages": [{"role": "user", "content": "use task"}],
            "tools": [
                {"type": "function", "function": {"name": "Task"}},
                {"type": "function", "function": {"name": "TodoWrite"}}
            ]
        });

        assert_eq!(
            infer_source_client("messages", &headers, &body),
            "claude-code"
        );
    }

    #[test]
    fn claude_code_web_tools_do_not_infer_openclaw() {
        let headers = HeaderMap::new();
        let body = serde_json::json!({
            "model": "deepseek-v4-flash",
            "messages": [{"role": "user", "content": "use Task and web search"}],
            "tools": [
                {"type": "function", "function": {"name": "Task"}},
                {"type": "function", "function": {"name": "TodoWrite"}},
                {"type": "function", "function": {"name": "web_fetch"}},
                {"type": "function", "function": {"name": "web_search"}}
            ]
        });

        assert_eq!(
            infer_source_client("messages", &headers, &body),
            "claude-code"
        );
    }

    #[test]
    fn ordinary_openclaw_reference_does_not_override_claude_tools() {
        let headers = HeaderMap::new();
        let body = serde_json::json!({
            "model": "deepseek-v4-flash",
            "messages": [{"role": "user", "content": "Compare OpenClaw and Hermes behavior, then use Task if needed."}],
            "tools": [
                {"type": "function", "function": {"name": "Task"}}
            ]
        });

        assert_eq!(
            infer_source_client("messages", &headers, &body),
            "claude-code"
        );
    }

    #[test]
    fn web_tools_alone_do_not_infer_openclaw_for_chat_completions() {
        let headers = HeaderMap::new();
        let body = serde_json::json!({
            "model": "deepseek-v4-flash",
            "messages": [{"role": "user", "content": "search"}],
            "tools": [
                {"type": "function", "function": {"name": "web_fetch"}},
                {"type": "function", "function": {"name": "web_search"}}
            ]
        });

        assert_eq!(
            infer_source_client("chat/completions", &headers, &body),
            "unknown"
        );
    }

    #[test]
    fn markerless_anthropic_messages_default_to_claude_code() {
        let headers = HeaderMap::new();
        let body = serde_json::json!({
            "model": "deepseek-v4-flash",
            "messages": [{"role": "user", "content": "large markerless ClaudeCode prompt"}],
            "stream": true
        });

        assert_eq!(
            infer_source_client("messages", &headers, &body),
            "claude-code"
        );
        assert_eq!(
            infer_source_client("chat/completions", &headers, &body),
            "unknown"
        );
    }

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

    #[test]
    fn extracts_openai_cache_usage_counts() {
        let body = Bytes::from_static(
            br#"{"usage":{"prompt_tokens":100,"completion_tokens":5,"total_tokens":105,"prompt_tokens_details":{"cached_tokens":80}}}"#,
        );

        let usage = extract_usage_counts("chat/completions", &body);

        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.cached_tokens, 80);
        assert_eq!(usage.cache_read_input_tokens, 80);
    }

    #[test]
    fn extracts_anthropic_cache_usage_counts() {
        let body = Bytes::from_static(
            br#"{"usage":{"input_tokens":100,"output_tokens":5,"cache_creation_input_tokens":20,"cache_read_input_tokens":70}}"#,
        );

        let usage = extract_usage_counts("messages", &body);

        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.cached_tokens, 70);
        assert_eq!(usage.cache_creation_input_tokens, 20);
        assert_eq!(usage.cache_read_input_tokens, 70);
    }

    #[test]
    fn transport_errors_are_hard_proxy_failures() {
        assert!(matches!(
            result_kind_for_classified_error(ErrorKind::Timeout, "timeout"),
            ResultKind::Error {
                kind: ErrorKind::Timeout
            }
        ));
        assert!(matches!(
            result_kind_for_classified_error(ErrorKind::Other, "network"),
            ResultKind::Error {
                kind: ErrorKind::Other
            }
        ));
        assert!(matches!(
            result_kind_for_classified_error(ErrorKind::Upstream5xx, "upstream_error"),
            ResultKind::SoftFailure {
                kind: ErrorKind::Upstream5xx
            }
        ));
    }

    #[test]
    fn provider_invalid_request_is_not_proxy_failure() {
        let err = AppError {
            status: StatusCode::BAD_REQUEST,
            message: "upstream provider error (status=400, code=invalid_request_error)".to_string(),
            upstream_headers: None,
            upstream_error_kind: Some(UpstreamErrorKind::ProviderInvalidRequest),
        };

        let (kind, outcome, error_type) = classify_app_error(&err);

        assert_eq!(kind, ErrorKind::Other);
        assert_eq!(outcome, "upstream_error");
        assert_eq!(error_type, "provider_invalid_request");
        assert!(matches!(
            result_kind_for_classified_error(kind, error_type),
            ResultKind::Success(400)
        ));
        assert_eq!(max_retries_for_app_error(&err, 3, 5), 0);

        let terminal_outcome = if err.status == StatusCode::TOO_MANY_REQUESTS {
            "rate_limited"
        } else if is_upstream_busy(err.status, &err.message) {
            "upstream_busy"
        } else if is_empty_upstream_error(&err) {
            "empty_output"
        } else if error_type == "provider_invalid_request" {
            outcome
        } else if matches!(
            kind,
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
        assert_eq!(terminal_outcome, "upstream_error");
    }

    #[test]
    fn nonstream_guard_observes_missing_max_tokens_without_default() {
        let body = serde_json::json!({
            "model":"deepseek-v4-flash-free",
            "stream": false,
            "messages":[{"role":"user","content":"hello"}]
        });

        let decision = apply_nonstream_output_guard("chat/completions", &body);

        assert_eq!(decision.action, "pass");
        assert_eq!(decision.max_tokens_before, None);
        assert_eq!(decision.max_tokens_after, None);
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn nonstream_guard_preserves_long_prompt_output() {
        let body = serde_json::json!({
            "model":"deepseek-v4-flash-free",
            "stream": false,
            "max_tokens": 4096,
            "messages":[{"role":"user","content":"x".repeat(220_000)}]
        });

        let decision = apply_nonstream_output_guard("chat/completions", &body);

        assert_eq!(decision.action, "pass");
        assert_eq!(decision.max_tokens_before, Some(4096));
        assert_eq!(decision.max_tokens_after, Some(4096));
        assert_eq!(body["max_tokens"], 4096);
    }

    #[test]
    fn nonstream_guard_preserves_huge_prompt_output() {
        let body = serde_json::json!({
            "model":"deepseek-v4-flash-free",
            "stream": false,
            "max_tokens": 4096,
            "messages":[{"role":"user","content":"x".repeat(440_000)}]
        });

        let decision = apply_nonstream_output_guard("chat/completions", &body);

        assert_eq!(decision.action, "pass");
        assert_eq!(decision.max_tokens_before, Some(4096));
        assert_eq!(decision.max_tokens_after, Some(4096));
        assert_eq!(body["max_tokens"], 4096);
    }

    #[test]
    fn nonstream_guard_preserves_huge_prompt_with_very_large_output() {
        let body = serde_json::json!({
            "model":"deepseek-v4-flash-free",
            "stream": false,
            "max_tokens": 20_000,
            "messages":[{"role":"user","content":"x".repeat(440_000)}]
        });

        let decision = apply_nonstream_output_guard("chat/completions", &body);

        assert_eq!(decision.action, "pass");
        assert_eq!(decision.max_tokens_before, Some(20_000));
        assert_eq!(decision.max_tokens_after, Some(20_000));
        assert_eq!(body["max_tokens"], 20_000);
    }

    #[test]
    fn stream_metrics_distinguishes_content_and_tool_signals() {
        let mut metrics = StreamMetrics::new(UsageCounts::default());

        metrics.ingest(
            "chat/completions",
            &Bytes::from_static(br#"data: {"choices":[{"delta":{"role":"assistant"}}]}"#),
        );
        assert!(!metrics.has_content_signal());
        assert!(!metrics.has_tool_signal());

        metrics.ingest(
            "chat/completions",
            &Bytes::from_static(b"\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"OK\"}}]}\n\n"),
        );
        assert!(metrics.has_content_signal());
        assert!(!metrics.has_tool_signal());

        metrics.ingest(
            "chat/completions",
            &Bytes::from_static(
                b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0}]}}]}\n\n",
            ),
        );
        assert!(metrics.has_tool_signal());
    }

    #[test]
    fn affinity_key_is_only_for_large_streaming_requests() {
        assert!(build_affinity_key("m", "chat/completions", "sk", 10, true).is_empty());
        assert!(build_affinity_key("m", "chat/completions", "sk", 200_000, false).is_empty());
        let key = build_affinity_key("m", "chat/completions", "sk", 200_000, true);
        assert!(key.starts_with("m:chat/completions:"));
        assert!(key.ends_with(":small"));
    }

    #[test]
    fn stream_error_frame_is_protocol_shaped() {
        let openai = String::from_utf8(
            stream_error_frame("chat/completions", "upstream stream error: broken").to_vec(),
        )
        .unwrap();
        assert!(openai.contains("data: {\"error\""));
        assert!(openai.contains("data: [DONE]"));

        let anthropic = String::from_utf8(
            stream_error_frame("messages", "upstream stream error: broken").to_vec(),
        )
        .unwrap();
        assert!(anthropic.contains("event: error"));
        assert!(anthropic.contains("\"type\":\"api_error\""));
    }

    #[test]
    fn classifies_stream_error_messages() {
        assert_eq!(
            classify_stream_error_message("error decoding response body"),
            "stream_decode_error"
        );
        assert_eq!(
            classify_stream_error_message("deadline elapsed while reading"),
            "stream_timeout"
        );
        assert_eq!(
            classify_stream_error_message("connection closed before message completed"),
            "stream_connection_error"
        );
        assert_eq!(classify_stream_error_message("other"), "stream_error");
    }

    #[tokio::test]
    async fn stream_send_detects_closed_downstream() {
        let (tx, rx) = mpsc::channel::<Result<Bytes, Infallible>>(1);
        drop(rx);

        let err = send_stream_bytes(&tx, Bytes::from_static(b"data: test\n\n"))
            .await
            .unwrap_err();

        assert_eq!(err, StreamSendError::Closed);
    }

    #[test]
    fn retry_budget_message_includes_last_error_and_attempt_count() {
        let chain = vec![RequestAttemptTelemetry {
            attempt: 1,
            node_id: "node".to_string(),
            node_url_redacted: "redacted".to_string(),
            status: 502,
            latency_ms: 1200,
            outcome: "transport_error".to_string(),
            error_type: "timeout".to_string(),
        }];

        let message =
            retry_budget_message(45_000, StatusCode::BAD_GATEWAY, "provider_error", &chain);

        assert!(message.contains("last_error=timeout"));
        assert!(message.contains("attempts=1"));
    }
}
