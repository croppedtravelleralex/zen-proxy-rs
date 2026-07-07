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
use free_model_client_rs::protocol::translate;
use free_model_client_rs::protocol::types::{AnthropicRequest, ChatRequest};
use futures::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::collector::{
    CacheForensicsTelemetry, DataCollector, ProtocolGuardTelemetry, RequestAttemptTelemetry,
    RequestTelemetry, RequestTimings,
};
use crate::config::Config;
use crate::ledger::LedgerEvent;
use crate::pool::{body_size_bucket, DispatchError, ErrorKind, RequestMeta, ResultKind};
use crate::state::AppState;
use crate::v4::context;
use crate::v4::model::{
    EffectiveModelRegistry, ModelCompatibilityProfile, ModelError, ModelRegistry,
};
use crate::v4::protocol_guard::{self, GuardPhase};

const MAX_PROVIDER_RESPONSE_BODY_BYTES: usize = 32 * 1024 * 1024;
const STREAM_DOWNSTREAM_SEND_TIMEOUT_SECS: u64 = 30;
const AFFINITY_MIN_BODY_BYTES: u64 = 32 * 1024;
const AFFINITY_MIN_BODY_BYTES_CLAUDE_CODE: u64 = 16 * 1024;

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
    let run_tags = extract_run_tags(headers);
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
    let registry = EffectiveModelRegistry::with_dynamic_allowlists(
        conf.dynamic_model_public_mode,
        state.dynamic_models.snapshot(),
        conf.dynamic_model_public_allowlist.clone(),
        conf.dynamic_model_claudecode_compat_allowlist.clone(),
    );
    let resolved = match registry.resolve(&public_model) {
        Ok(resolved) => resolved,
        Err(ModelError::UnknownModel(model)) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!("unsupported V4 model: {model}"),
            );
        }
    };
    let dynamic_model = matches!(
        resolved.compatibility_profile,
        ModelCompatibilityProfile::DynamicGeneric
            | ModelCompatibilityProfile::DynamicClaudeCodeCompatible
            | ModelCompatibilityProfile::DynamicRestricted
    );

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

    let cache_api_key_id = free_model_client_rs::ccp::api_key_id_for_cache(&conf.upstream_api_key);
    let (usk, icp_scope, prefix_32k_hash, session_id) = resolve_session_identity(
        path,
        &upstream_body,
        &resolved.upstream_model,
        &source_client,
        &cache_api_key_id,
        client_id,
    );
    let session_id = if session_id.is_empty() {
        extract_header(headers, "x-opencode-session")
            .or_else(|| {
                if client_id.trim().is_empty() {
                    None
                } else {
                    Some(client_id.to_string())
                }
            })
            .unwrap_or_default()
    } else {
        session_id
    };

    let ccp_snap_preflight =
        build_ccp_audit_snap_preflight(&session_id, &usk, &icp_scope, &prefix_32k_hash);
    let cache_forensics = build_cache_forensics(
        path,
        &upstream_body,
        &resolved.upstream_model,
        &source_client,
        &cache_api_key_id,
        client_id,
    );
    let thinking_policy = infer_thinking_policy(&upstream_body);

    let request_meta = RequestMeta {
        model: public_model.clone(),
        upstream_model: resolved.upstream_model.clone(),
        session_id,
        stream: streaming,
        body_size: effective_body_len,
        affinity_key: build_affinity_key(AffinityKeyInput {
            public_model: &public_model,
            upstream_model: &resolved.upstream_model,
            path,
            source_client: &source_client,
            cache_api_key_id: &cache_api_key_id,
            fallback_client_id: client_id,
            body_size: effective_body_len,
            body: &upstream_body,
        }),
        allow_direct_fallback: !dynamic_model || conf.dynamic_model_allow_direct_fallback,
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
            compatibility_profile: resolved.compatibility_profile,
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
            let ccp_snap = build_ccp_audit_snap(
                &ccp_snap_preflight.session_id,
                &ccp_snap_preflight.usk,
                &ccp_snap_preflight.icp_scope,
                &ccp_snap_preflight.prefix_32k_hash,
                &result.usage,
                &thinking_policy,
            );
            let mut success_cache_forensics = cache_forensics.clone();
            if let Some(forensics) = success_cache_forensics.as_mut() {
                result.final_provider_cache.apply_to(forensics);
            }
            let telemetry = RequestTelemetry {
                rid: result.request_id.clone(),
                ts: chrono::Utc::now().timestamp_millis(),
                external_request_id: external_request_id.clone(),
                gateway: gateway.clone(),
                gateway_channel_id: extract_header(headers, "x-newapi-channel-id")
                    .unwrap_or_default(),
                run_id: run_tags.run_id.clone(),
                source_platform: run_tags.source_platform.clone(),
                case_id: run_tags.case_id.clone(),
                runner_model: run_tags.runner_model.clone(),
                provider_id: run_tags.provider_id.clone(),
                turn_index: run_tags.turn_index,
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
                cache_miss_input_tokens: ccp_snap.cache_miss_input_tokens,
                session_id: ccp_snap.session_id,
                usk: ccp_snap.usk,
                icp_scope: ccp_snap.icp_scope,
                prefix_32k_hash: ccp_snap.prefix_32k_hash,
                cache_forensics: success_cache_forensics.clone(),
                prefix_drift: ccp_snap.prefix_drift,
                session_pin_hit: result.session_pin_hit,
                thinking_policy: ccp_snap.thinking_policy,
                prompt_cache_key: ccp_snap.prompt_cache_key,
                provider_cache_observation: ccp_snap.provider_cache_observation,
                warmup_state: ccp_snap.warmup_state,
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
                record_dynamic_model_traffic_from_telemetry(state, &telemetry);
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
                    run_id: run_tags.run_id.clone(),
                    source_platform: run_tags.source_platform.clone(),
                    case_id: run_tags.case_id.clone(),
                    runner_model: run_tags.runner_model.clone(),
                    provider_id: run_tags.provider_id.clone(),
                    turn_index: run_tags.turn_index,
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
                    cache_miss_input_tokens: 0,
                    session_id: ccp_snap_preflight.session_id.clone(),
                    usk: ccp_snap_preflight.usk.clone(),
                    icp_scope: ccp_snap_preflight.icp_scope.clone(),
                    prefix_32k_hash: ccp_snap_preflight.prefix_32k_hash.clone(),
                    cache_forensics: cache_forensics.clone(),
                    prefix_drift: ccp_snap_preflight.prefix_drift,
                    session_pin_hit: false,
                    thinking_policy: thinking_policy.clone(),
                    prompt_cache_key: ccp_snap_preflight.prompt_cache_key.clone(),
                    provider_cache_observation: String::new(),
                    warmup_state: ccp_snap_preflight.warmup_state.clone(),
                    bytes_sent: effective_body_len,
                    bytes_received: 0,
                    failure_kind: err.failure_kind.clone(),
                    failure_message: err.message.clone(),
                    retry_chain: err.retry_chain.clone(),
                    context: Some(context_telemetry.clone()),
                    protocol_guard: protocol_guard_summary.clone(),
                });
            }
            record_dynamic_model_traffic(
                state,
                &public_model,
                err.status.as_u16(),
                &err.failure_kind,
                &err.message,
            );
            let mut response = error_response(err.status, err.message.clone());
            if let Some(retry_after) = err.retry_after_secs {
                response.headers_mut().insert(
                    "retry-after",
                    HeaderValue::from_str(&retry_after.to_string()).unwrap(),
                );
            }
            insert_error_diagnostics_headers(response.headers_mut(), &err);
            insert_nonstream_guard_headers(response.headers_mut(), &nonstream_guard);
            insert_context_headers(response.headers_mut(), &context_telemetry);
            response
        }
    }
}

fn insert_error_diagnostics_headers(headers: &mut HeaderMap, err: &V4CallError) {
    if let Ok(value) = HeaderValue::from_str(&err.retry_count.to_string()) {
        headers.insert("x-zen-retry-count", value);
    }
    if !err.failure_kind.is_empty() {
        if let Ok(value) = HeaderValue::from_str(&err.failure_kind) {
            headers.insert("x-zen-failure-kind", value);
        }
    }
    if let Some(node_id) = err
        .selected_node_id
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        if let Ok(value) = HeaderValue::from_str(node_id) {
            headers.insert("x-zen-selected-node-id", value);
        }
    }
    let retry_chain = err
        .retry_chain
        .iter()
        .take(16)
        .map(|attempt| {
            format!(
                "{}:{}:{}",
                attempt.node_id, attempt.status, attempt.error_type
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    if !retry_chain.is_empty() {
        if let Ok(value) = HeaderValue::from_str(&retry_chain) {
            headers.insert("x-zen-retry-chain", value);
        }
    }
}

fn record_dynamic_model_traffic_from_telemetry(
    state: &Arc<AppState>,
    telemetry: &RequestTelemetry,
) {
    record_dynamic_model_traffic(
        state,
        &telemetry.public_model,
        traffic_status_for_telemetry(telemetry),
        &telemetry.failure_kind,
        &telemetry.failure_message,
    );
}

fn record_dynamic_model_traffic(
    state: &Arc<AppState>,
    public_model: &str,
    status: u16,
    failure_kind: &str,
    failure_message: &str,
) {
    let model_id = {
        let cfg = state.config.read().unwrap();
        let registry = EffectiveModelRegistry::with_dynamic_allowlists(
            cfg.dynamic_model_public_mode,
            state.dynamic_models.snapshot(),
            cfg.dynamic_model_public_allowlist.clone(),
            cfg.dynamic_model_claudecode_compat_allowlist.clone(),
        );
        registry
            .resolve(public_model)
            .map(|resolved| resolved.upstream_model)
            .unwrap_or_else(|_| public_model.to_string())
    };
    let normalized_failure_kind = if status == 429 && failure_kind == "rate_limited" {
        "upstream_429".to_string()
    } else if failure_kind.trim().is_empty() && status >= 400 {
        classify_traffic_fallback_failure(status, failure_message)
    } else {
        failure_kind.to_string()
    };
    state.dynamic_models.record_traffic_result(
        &model_id,
        status,
        normalized_failure_kind,
        failure_message.to_string(),
    );
}

fn traffic_status_for_telemetry(telemetry: &RequestTelemetry) -> u16 {
    if telemetry.failure_kind == "client_gone" {
        499
    } else {
        telemetry.status
    }
}

fn classify_traffic_fallback_failure(status: u16, message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    if lower.contains("no proxy resources available") {
        "proxy_pool_exhausted".to_string()
    } else if lower.contains("circuit open") {
        "circuit_open".to_string()
    } else if lower.contains("request exceeds proxy node budget") {
        "request_too_large".to_string()
    } else {
        format!("http_{status}")
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

#[derive(Debug, Clone, Default)]
struct RunTags {
    run_id: String,
    source_platform: String,
    case_id: String,
    runner_model: String,
    provider_id: String,
    turn_index: u32,
}

fn extract_run_tags(headers: &HeaderMap) -> RunTags {
    RunTags {
        run_id: extract_first_header(headers, &["x-zfs-run-id", "x-zen-run-id", "x-run-id"]),
        source_platform: extract_first_header(
            headers,
            &["x-zfs-platform", "x-source-platform", "x-run-platform"],
        ),
        case_id: extract_first_header(headers, &["x-zfs-case", "x-case-id"]),
        runner_model: extract_first_header(
            headers,
            &["x-zfs-model", "x-runner-model", "x-claude-model"],
        ),
        provider_id: extract_first_header(headers, &["x-zfs-provider-id", "x-provider-id"]),
        turn_index: extract_first_header(headers, &["x-zfs-turn-index", "x-turn-index"])
            .parse::<u32>()
            .unwrap_or_default(),
    }
}

fn extract_first_header(headers: &HeaderMap, names: &[&str]) -> String {
    names
        .iter()
        .find_map(|name| extract_header(headers, name))
        .unwrap_or_default()
}

struct AffinityKeyInput<'a> {
    public_model: &'a str,
    upstream_model: &'a str,
    path: &'a str,
    source_client: &'a str,
    cache_api_key_id: &'a str,
    fallback_client_id: &'a str,
    body_size: u64,
    body: &'a Value,
}

fn build_affinity_key(input: AffinityKeyInput<'_>) -> String {
    let min_bytes = if input.source_client.trim() == "claude-code" {
        AFFINITY_MIN_BODY_BYTES_CLAUDE_CODE
    } else {
        AFFINITY_MIN_BODY_BYTES
    };
    if input.body_size < min_bytes {
        return String::new();
    }
    let client_bucket = if input.fallback_client_id.trim().is_empty() {
        "anon".to_string()
    } else {
        LedgerEvent::short_hash(input.fallback_client_id)
    };
    let source_bucket = if input.source_client.trim().is_empty() {
        "unknown"
    } else {
        input.source_client.trim()
    };
    if let Some((identity, _request_model)) = compute_cache_identity(
        input.path,
        input.body,
        input.upstream_model,
        source_bucket,
        input.cache_api_key_id,
    ) {
        return free_model_client_rs::ccp::affinity_key_from_identity(
            &identity,
            input.path,
            &client_bucket,
        );
    }
    format!(
        "{}:{}:{}:{}:{}",
        input.upstream_model, input.public_model, input.path, source_bucket, client_bucket
    )
}

fn resolve_session_identity(
    path: &str,
    body: &Value,
    upstream_model: &str,
    source_client: &str,
    cache_api_key_id: &str,
    fallback_client_id: &str,
) -> (String, String, String, String) {
    let client_bucket = if fallback_client_id.trim().is_empty() {
        "anon".to_string()
    } else {
        LedgerEvent::short_hash(fallback_client_id)
    };
    let source_bucket = if source_client.trim().is_empty() {
        "unknown".to_string()
    } else {
        source_client.trim().to_string()
    };
    if let Some((identity, _request_model)) =
        compute_cache_identity(path, body, upstream_model, &source_bucket, cache_api_key_id)
    {
        return (
            identity.usk.clone(),
            identity.icp_scope.clone(),
            format!("{:016x}", identity.prefix_32k_hash),
            identity.zen_session_id.clone(),
        );
    }
    (String::new(), String::new(), String::new(), client_bucket)
}

#[derive(Debug, Clone)]
struct CacheForkShape {
    ccp_prefix_32k_hash: String,
    raw_body_prefix_32k_hash: String,
    tools_hash: String,
    roles_hash: String,
    message_count: u64,
    tool_count: u64,
    tool_result_bytes: u64,
}

fn build_cache_forensics(
    path: &str,
    body: &Value,
    upstream_model: &str,
    source_client: &str,
    cache_api_key_id: &str,
    fallback_client_id: &str,
) -> Option<CacheForensicsTelemetry> {
    let request = cache_identity_chat_request(path, body)?;
    let shape = translate::request_shape(&request);
    let raw_body = serde_json::to_vec(body).unwrap_or_default();
    let tools_json = request
        .tools
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .unwrap_or_default()
        .unwrap_or_default();
    let roles = request
        .messages
        .iter()
        .map(|message| message.role.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let (tool_result_bytes, tool_result_count) = tool_result_stats(&request);
    let client_bucket = if fallback_client_id.trim().is_empty() {
        "anon".to_string()
    } else {
        LedgerEvent::short_hash(fallback_client_id)
    };
    let source_bucket = if source_client.trim().is_empty() {
        "unknown"
    } else {
        source_client.trim()
    };
    let fork_key = LedgerEvent::short_hash(&format!(
        "{cache_api_key_id}:{upstream_model}:{path}:{source_bucket}:{client_bucket}"
    ));
    let mut telemetry = CacheForensicsTelemetry {
        ccp_hash_algorithm: "fnv1a64:cache_material".to_string(),
        raw_body_hash_algorithm: "sha256:hex16".to_string(),
        raw_body_stage: "zenproxy_upstream_body_before_kernel".to_string(),
        ccp_prompt_hash: format!("{:016x}", shape.prompt_hash),
        ccp_prefix_4k_hash: format!("{:016x}", shape.prefix_4k_hash),
        ccp_prefix_32k_hash: format!("{:016x}", shape.prefix_32k_hash),
        ccp_prefix_128k_hash: format!("{:016x}", shape.prefix_128k_hash),
        ccp_prefix_256k_hash: format!("{:016x}", shape.prefix_256k_hash),
        ccp_cache_material_bytes: shape.cache_material_bytes as u64,
        raw_body_prefix_4k_hash: hash_prefix_bytes(&raw_body, 4 * 1024),
        raw_body_prefix_32k_hash: hash_prefix_bytes(&raw_body, 32 * 1024),
        raw_body_prefix_128k_hash: hash_prefix_bytes(&raw_body, 128 * 1024),
        raw_body_prefix_256k_hash: hash_prefix_bytes(&raw_body, 256 * 1024),
        raw_body_bytes: raw_body.len() as u64,
        estimated_total_tokens: shape.estimated_total_tokens,
        message_count: shape.message_count as u64,
        tool_count: shape.tool_count as u64,
        tools_hash: LedgerEvent::short_hash(&tools_json),
        roles_hash: LedgerEvent::short_hash(&roles),
        tool_result_bytes,
        tool_result_count,
        ccp_raw_prefix_match_32k: false,
        final_provider_body_bytes: 0,
        final_provider_body_prefix_32k_hash: String::new(),
        final_provider_cache_control_locations: String::new(),
        final_provider_cache_control_block_hashes: String::new(),
        final_provider_cache_policy_match: false,
        final_provider_cache_segment_hash: String::new(),
        fork_key,
        fork_reason: String::new(),
    };
    telemetry.ccp_raw_prefix_match_32k =
        telemetry.ccp_prefix_32k_hash == telemetry.raw_body_prefix_32k_hash;
    telemetry.fork_reason = classify_cache_fork(&telemetry);
    Some(telemetry)
}

fn tool_result_stats(request: &ChatRequest) -> (u64, u64) {
    let mut bytes = 0u64;
    let mut count = 0u64;
    for message in &request.messages {
        if message.role != "tool" {
            continue;
        }
        count = count.saturating_add(1);
        let len = match &message.content {
            Value::String(text) => text.len(),
            other => serde_json::to_vec(other)
                .map(|value| value.len())
                .unwrap_or(0),
        };
        bytes = bytes.saturating_add(len as u64);
    }
    (bytes, count)
}

fn hash_prefix_bytes(bytes: &[u8], prefix_bytes: usize) -> String {
    use sha2::{Digest, Sha256};
    let len = bytes.len().min(prefix_bytes);
    let digest = Sha256::digest(&bytes[..len]);
    hex16(&digest)
}

fn hex16(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn classify_cache_fork(current: &CacheForensicsTelemetry) -> String {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static LAST: OnceLock<Mutex<HashMap<String, CacheForkShape>>> = OnceLock::new();
    let store = LAST.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = match store.lock() {
        Ok(guard) => guard,
        Err(_) => return "unknown".to_string(),
    };
    let previous = guard.insert(
        current.fork_key.clone(),
        CacheForkShape {
            ccp_prefix_32k_hash: current.ccp_prefix_32k_hash.clone(),
            raw_body_prefix_32k_hash: current.raw_body_prefix_32k_hash.clone(),
            tools_hash: current.tools_hash.clone(),
            roles_hash: current.roles_hash.clone(),
            message_count: current.message_count,
            tool_count: current.tool_count,
            tool_result_bytes: current.tool_result_bytes,
        },
    );
    let Some(previous) = previous else {
        return "baseline".to_string();
    };
    if previous.raw_body_prefix_32k_hash == current.raw_body_prefix_32k_hash {
        return "raw_prefix_stable".to_string();
    }
    if previous.ccp_prefix_32k_hash == current.ccp_prefix_32k_hash {
        return "raw_prefix_drift_with_stable_ccp_identity".to_string();
    }
    if previous.tools_hash != current.tools_hash || previous.tool_count != current.tool_count {
        return "tools_schema_drift".to_string();
    }
    if previous.roles_hash != current.roles_hash {
        return "message_roles_drift".to_string();
    }
    if previous.tool_result_bytes != current.tool_result_bytes {
        return "tool_result_payload_drift".to_string();
    }
    if previous.message_count != current.message_count {
        return "message_history_growth".to_string();
    }
    "ccp_prefix_drift".to_string()
}

fn compute_cache_identity(
    path: &str,
    body: &Value,
    upstream_model: &str,
    source_client: &str,
    cache_api_key_id: &str,
) -> Option<(free_model_client_rs::ccp::IcpIdentity, String)> {
    let request = cache_identity_chat_request(path, body)?;
    let request_model = request.model.clone();
    let ctx = free_model_client_rs::ccp::UskContext {
        api_key_id: cache_api_key_id,
        public_model: &request_model,
        upstream_model,
        source_client,
    };
    Some((
        free_model_client_rs::ccp::compute_icp_identity(&request, &ctx),
        request_model,
    ))
}

fn cache_identity_chat_request(path: &str, body: &Value) -> Option<ChatRequest> {
    match path {
        "chat/completions" => serde_json::from_value::<ChatRequest>(body.clone()).ok(),
        "messages" => {
            let request = serde_json::from_value::<AnthropicRequest>(body.clone()).ok()?;
            let model = request.model.clone();
            let messages = translate::anthropic_to_openai_messages(&request);
            let tools = request
                .tools
                .as_ref()
                .map(|tools| translate::anthropic_tools_to_openai(tools))
                .filter(|tools| !tools.is_empty());
            let tool_choice = request
                .tool_choice
                .as_ref()
                .map(translate::anthropic_tool_choice_to_openai);
            Some(ChatRequest {
                model,
                messages,
                stream: request.stream,
                max_tokens: request.max_tokens,
                temperature: request.temperature,
                top_p: None,
                tools,
                tool_choice,
            })
        }
        _ => None,
    }
}

#[derive(Debug, Clone)]
struct CcpAuditSnap {
    session_id: String,
    usk: String,
    icp_scope: String,
    prefix_32k_hash: String,
    prefix_drift: bool,
    prompt_cache_key: String,
    cache_miss_input_tokens: u32,
    provider_cache_observation: String,
    warmup_state: String,
    thinking_policy: String,
}

fn detect_prefix_drift(usk: &str, prefix_32k_hash: &str) -> bool {
    if usk.is_empty() || prefix_32k_hash.is_empty() {
        return false;
    }
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static LAST: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let store = LAST.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = match store.lock() {
        Ok(guard) => guard,
        Err(_) => return false,
    };
    let drift = guard
        .get(usk)
        .is_some_and(|previous| previous != prefix_32k_hash);
    guard.insert(usk.to_string(), prefix_32k_hash.to_string());
    drift
}

fn provider_cache_observation(usage: &UsageCounts) -> String {
    if usage.cache_read_input_tokens > 0 || usage.cached_tokens > 0 {
        "cache_hit".to_string()
    } else if usage.cache_creation_input_tokens > 0 {
        "cache_write".to_string()
    } else {
        "no_cache_signal".to_string()
    }
}

fn cache_miss_input_tokens(usage: &UsageCounts) -> u32 {
    if let Some(cache_miss) = usage.cache_miss_input_tokens {
        return cache_miss;
    }
    let prompt = usage.prompt_tokens;
    let read = usage.cache_read_input_tokens.max(usage.cached_tokens);
    prompt.saturating_sub(read)
}

fn warmup_state_for(usk: &str, observation: &str) -> String {
    if usk.is_empty() {
        return "unknown".to_string();
    }
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static WARM: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let store = WARM.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = match store.lock() {
        Ok(guard) => guard,
        Err(_) => return "unknown".to_string(),
    };
    if observation == "cache_hit" {
        guard.insert(usk.to_string());
        "steady".to_string()
    } else if guard.contains(usk) {
        "steady".to_string()
    } else {
        "cold".to_string()
    }
}

fn build_ccp_audit_snap(
    session_id: &str,
    usk: &str,
    icp_scope: &str,
    prefix_32k_hash: &str,
    usage: &UsageCounts,
    thinking_policy: &str,
) -> CcpAuditSnap {
    let prefix_drift = detect_prefix_drift(usk, prefix_32k_hash);
    let provider_cache_observation = provider_cache_observation(usage);
    let warmup_state = warmup_state_for(usk, &provider_cache_observation);
    CcpAuditSnap {
        session_id: session_id.to_string(),
        usk: usk.to_string(),
        icp_scope: icp_scope.to_string(),
        prefix_32k_hash: prefix_32k_hash.to_string(),
        prefix_drift,
        prompt_cache_key: usk.to_string(),
        cache_miss_input_tokens: cache_miss_input_tokens(usage),
        provider_cache_observation,
        warmup_state,
        thinking_policy: thinking_policy.to_string(),
    }
}

fn build_ccp_audit_snap_preflight(
    session_id: &str,
    usk: &str,
    icp_scope: &str,
    prefix_32k_hash: &str,
) -> CcpAuditSnap {
    let prefix_drift = detect_prefix_drift(usk, prefix_32k_hash);
    CcpAuditSnap {
        session_id: session_id.to_string(),
        usk: usk.to_string(),
        icp_scope: icp_scope.to_string(),
        prefix_32k_hash: prefix_32k_hash.to_string(),
        prefix_drift,
        prompt_cache_key: usk.to_string(),
        cache_miss_input_tokens: 0,
        provider_cache_observation: String::new(),
        warmup_state: if usk.is_empty() {
            "unknown".to_string()
        } else {
            "cold".to_string()
        },
        thinking_policy: String::new(),
    }
}

fn infer_thinking_policy(body: &Value) -> String {
    if body.get("thinking").is_some() {
        return "enabled".to_string();
    }
    if body
        .get("metadata")
        .and_then(|meta| meta.get("thinking"))
        .is_some()
    {
        return "enabled".to_string();
    }
    "production_default".to_string()
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
    const MAX_SCAN_NODES: usize = 20_000;
    const MAX_SCAN_DEPTH: usize = 128;

    let mut stack = vec![(value, 0usize)];
    let mut seen = 0usize;
    while let Some((current, depth)) = stack.pop() {
        seen = seen.saturating_add(1);
        if seen > MAX_SCAN_NODES || depth > MAX_SCAN_DEPTH {
            return false;
        }
        match current {
            Value::String(text) => {
                let lower = text.to_ascii_lowercase();
                let matched = match marker {
                    "openclaw" => contains_strong_openclaw_marker(&lower),
                    "hermes" => contains_strong_hermes_marker(&lower),
                    _ => false,
                };
                if matched {
                    return true;
                }
            }
            Value::Array(items) => {
                stack.extend(items.iter().map(|item| (item, depth + 1)));
            }
            Value::Object(map) => {
                stack.extend(map.values().map(|item| (item, depth + 1)));
            }
            _ => {}
        }
    }
    false
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

fn profile_for_openai_request(
    source_client: &str,
    request: &ChatRequest,
    compatibility_profile: ModelCompatibilityProfile,
) -> ClientProfile {
    let observed = profile_from_source_client(source_client)
        .unwrap_or_else(|| ClientProfile::from_openai(&HeaderMap::new(), request))
        .effective_for_model(&request.model);
    apply_model_compatibility_profile(observed, compatibility_profile)
}

fn profile_for_anthropic_request(
    source_client: &str,
    request: &AnthropicRequest,
    compatibility_profile: ModelCompatibilityProfile,
) -> ClientProfile {
    let observed = profile_from_source_client(source_client)
        .unwrap_or_else(|| ClientProfile::from_anthropic(&HeaderMap::new(), request))
        .effective_for_model(&request.model);
    apply_model_compatibility_profile(observed, compatibility_profile)
}

fn apply_model_compatibility_profile(
    profile: ClientProfile,
    compatibility_profile: ModelCompatibilityProfile,
) -> ClientProfile {
    match compatibility_profile {
        ModelCompatibilityProfile::StaticFlash => profile,
        ModelCompatibilityProfile::StaticFlashLite => profile,
        ModelCompatibilityProfile::StaticMimo => {
            if matches!(profile.kind, ClientKind::ClaudeCode) {
                profile
            } else {
                ClientProfile::unknown()
            }
        }
        ModelCompatibilityProfile::DynamicClaudeCodeCompatible => {
            if matches!(profile.kind, ClientKind::ClaudeCode) {
                profile
            } else {
                ClientProfile::unknown()
            }
        }
        ModelCompatibilityProfile::DynamicGeneric
        | ModelCompatibilityProfile::DynamicRestricted => ClientProfile::unknown(),
    }
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
    session_pin_hit: bool,
    retry_chain: Vec<RequestAttemptTelemetry>,
    body_bytes_len: u64,
    usage: UsageCounts,
    final_provider_cache: FinalProviderCacheHeaders,
}

#[derive(Debug, Clone, Default)]
struct FinalProviderCacheHeaders {
    body_bytes: u64,
    body_prefix_32k_hash: String,
    cache_control_locations: String,
    cache_control_block_hashes: String,
    cache_policy_match: bool,
    cache_segment_hash: String,
}

impl FinalProviderCacheHeaders {
    fn from_headers(headers: &HeaderMap) -> Self {
        Self {
            body_bytes: extract_header(headers, "x-fmc-final-body-bytes")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or_default(),
            body_prefix_32k_hash: extract_header(headers, "x-fmc-final-body-prefix-32k-hash")
                .unwrap_or_default(),
            cache_control_locations: extract_header(headers, "x-fmc-cache-control-locations")
                .unwrap_or_default(),
            cache_control_block_hashes: extract_header(headers, "x-fmc-cache-control-block-hashes")
                .unwrap_or_default(),
            cache_policy_match: extract_header(headers, "x-fmc-cache-policy-match")
                .is_some_and(|value| value.eq_ignore_ascii_case("true")),
            cache_segment_hash: extract_header(headers, "x-fmc-provider-cache-segment-hash")
                .unwrap_or_default(),
        }
    }

    fn apply_to(&self, telemetry: &mut CacheForensicsTelemetry) {
        telemetry.final_provider_body_bytes = self.body_bytes;
        telemetry.final_provider_body_prefix_32k_hash = self.body_prefix_32k_hash.clone();
        telemetry.final_provider_cache_control_locations = self.cache_control_locations.clone();
        telemetry.final_provider_cache_control_block_hashes =
            self.cache_control_block_hashes.clone();
        telemetry.final_provider_cache_policy_match = self.cache_policy_match;
        telemetry.final_provider_cache_segment_hash = self.cache_segment_hash.clone();
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct UsageCounts {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
    cached_tokens: u32,
    cache_creation_input_tokens: u32,
    cache_read_input_tokens: u32,
    cache_miss_input_tokens: Option<u32>,
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
    compatibility_profile: ModelCompatibilityProfile,
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
    let compatibility_profile = call_context.compatibility_profile;
    let source_client = call_context.source_client;
    let base_max = conf.pool_max_retries;
    let configured_empty_upstream_max = conf.v4_empty_upstream_max_retries.max(base_max);
    let empty_upstream_max =
        effective_empty_upstream_max_retries(path, &request_meta, configured_empty_upstream_max);
    let mut last_status = StatusCode::BAD_GATEWAY;
    let mut was_rate_limited = false;
    let mut dispatch_wait_ms = 0u64;
    let mut retry_chain = Vec::new();
    let retry_budget_ms = effective_retry_budget_ms(path, &request_meta, conf.v4_retry_budget_ms);
    let mut force_direct_next = false;

    for attempt in 0..=empty_upstream_max {
        let dispatch_start = Instant::now();
        let dispatch_result = if force_direct_next {
            force_direct_next = false;
            state.pool_manager.dispatch_direct().map_err(|_| {
                V4CallError::before_dispatch(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "direct fallback is not available",
                )
            })?
        } else {
            dispatch_or_wait(state, &request_meta, attempt, empty_upstream_max).await?
        };
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
            claude_code_stream_initial_fetch_timeout_secs: conf
                .free_model_claude_code_stream_initial_fetch_timeout_secs,
            claude_code_stream_slow_guard_min_input_tokens: conf
                .free_model_claude_code_stream_slow_guard_min_input_tokens,
            claude_code_stream_no_forwardable_retry_secs: conf
                .free_model_claude_code_stream_no_forwardable_retry_secs,
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
                let profile =
                    profile_for_openai_request(source_client, &request, compatibility_profile);
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
                let profile =
                    profile_for_anthropic_request(source_client, &request, compatibility_profile);
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
                    let final_provider_cache =
                        FinalProviderCacheHeaders::from_headers(response.headers());
                    let observed_exit_ip = response
                        .headers()
                        .get("x-zen-observed-exit-ip")
                        .and_then(|value| value.to_str().ok())
                        .map(ToOwned::to_owned);
                    let (response, body_bytes_len, usage, has_output) = if request_meta.stream {
                        (response, 0, UsageCounts::default(), true)
                    } else {
                        buffered_response_with_usage(response, path, public_model).await?
                    };
                    if !request_meta.stream && !has_output {
                        crate::pool::session_pin::clear(
                            &request_meta.upstream_model,
                            &request_meta.session_id,
                        );
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
                        session_pin_hit: dispatch_result.session_pin_hit,
                        retry_chain,
                        body_bytes_len,
                        usage,
                        final_provider_cache,
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
                clear_session_pin_for_failure(&request_meta, status, failure_kind);
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
                if conf.allow_direct_fallback
                    && request_meta.allow_direct_fallback
                    && node_id != "direct"
                    && is_direct_fallback_status(status)
                    && attempt < base_max
                {
                    force_direct_next = true;
                }
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
                let provider_rate_limited = is_provider_rate_limited(status, &err.message);
                if provider_rate_limited {
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
                    clear_session_pin_for_failure(&request_meta, status, error_type);
                    let result = result_kind_for_app_error(&err, error_kind, error_type);
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
                        was_rate_limited || provider_rate_limited,
                        latency,
                        "retry_budget_exhausted",
                        retry_chain,
                    ));
                }
                let max_for_error = max_retries_for_app_error(&err, base_max, empty_upstream_max);
                let (_, _, error_type) = classify_app_error(&err);
                if conf.allow_direct_fallback
                    && request_meta.allow_direct_fallback
                    && node_id != "direct"
                    && is_transport_error_type(error_type)
                    && attempt < max_for_error
                {
                    force_direct_next = true;
                }
                if attempt >= max_for_error {
                    let (error_kind, outcome, error_type) = classify_app_error(&err);
                    let provider_rate_limited = is_provider_rate_limited(status, &err.message);
                    let outcome = if provider_rate_limited {
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
                        was_rate_limited || provider_rate_limited,
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

fn effective_empty_upstream_max_retries(
    path: &str,
    request_meta: &RequestMeta,
    configured_max: u32,
) -> u32 {
    if !is_mimo_messages_request(path, request_meta) {
        return configured_max;
    }
    if request_meta.estimated_input_tokens() >= 10_000 {
        return configured_max.min(2);
    }
    configured_max.min(4)
}

fn effective_retry_budget_ms(
    path: &str,
    request_meta: &RequestMeta,
    configured_budget_ms: u64,
) -> u64 {
    if configured_budget_ms == 0 || !is_mimo_messages_request(path, request_meta) {
        return configured_budget_ms;
    }
    let estimated_tokens = request_meta.estimated_input_tokens();
    if estimated_tokens >= 50_000 {
        return configured_budget_ms.min(20_000);
    }
    if estimated_tokens >= 10_000 {
        return configured_budget_ms.min(30_000);
    }
    configured_budget_ms
}

fn is_mimo_messages_request(path: &str, request_meta: &RequestMeta) -> bool {
    path == "messages"
        && (crate::pool::session_pin::is_mimo_family(&request_meta.model)
            || crate::pool::session_pin::is_mimo_family(&request_meta.upstream_model))
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
        let result = if matches!(status, 502 | 504) {
            ResultKind::Error {
                kind: ErrorKind::Upstream5xx,
            }
        } else {
            ResultKind::SoftFailure {
                kind: ErrorKind::Upstream5xx,
            }
        };
        state
            .pool_manager
            .report(node_id.to_string(), result, latency);
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

fn is_provider_rate_limited(status: StatusCode, message: &str) -> bool {
    if status == StatusCode::TOO_MANY_REQUESTS {
        return true;
    }
    let lower = message.to_ascii_lowercase();
    lower.contains("rate limited")
        || lower.contains("rate_limit")
        || lower.contains("rate-limit")
        || lower.contains("too many requests")
}

fn classify_app_error(err: &AppError) -> (ErrorKind, &'static str, &'static str) {
    let message = err.message.to_ascii_lowercase();
    if is_provider_rate_limited(err.status, &err.message) {
        return (ErrorKind::Upstream5xx, "rate_limited", "upstream_429");
    }
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
    if error_type == "upstream_429" {
        return ResultKind::RateLimited;
    }

    if error_type == "provider_invalid_request" {
        return ResultKind::Success(400);
    }

    if error_type == "empty_output" {
        return ResultKind::EmptyOutput;
    }

    if matches!(
        error_type,
        "timeout" | "connection_refused" | "dns_failure" | "socks_handshake"
    ) {
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

fn result_kind_for_app_error(
    err: &AppError,
    error_kind: ErrorKind,
    error_type: &str,
) -> ResultKind {
    if matches!(
        err.status,
        StatusCode::BAD_GATEWAY | StatusCode::GATEWAY_TIMEOUT
    ) && error_type == "upstream_error"
    {
        return ResultKind::Error {
            kind: ErrorKind::Upstream5xx,
        };
    }
    result_kind_for_classified_error(error_kind, error_type)
}

fn clear_session_pin_for_failure(request_meta: &RequestMeta, status: StatusCode, error_type: &str) {
    if !should_clear_session_pin_for_failure(status, error_type) {
        return;
    }
    crate::pool::session_pin::clear(&request_meta.upstream_model, &request_meta.session_id);
}

fn should_clear_session_pin_for_failure(status: StatusCode, error_type: &str) -> bool {
    if matches!(
        error_type,
        "upstream_429" | "provider_invalid_request" | "upstream_busy" | "client_gone"
    ) {
        return false;
    }
    if matches!(
        error_type,
        "empty_output"
            | "timeout"
            | "network"
            | "connection_refused"
            | "dns_failure"
            | "socks_handshake"
    ) {
        return true;
    }
    matches!(
        status,
        StatusCode::BAD_GATEWAY
            | StatusCode::GATEWAY_TIMEOUT
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::SERVICE_UNAVAILABLE
    )
}

fn is_transport_error_type(error_type: &str) -> bool {
    matches!(
        error_type,
        "timeout" | "network" | "connection_refused" | "dns_failure" | "socks_handshake"
    )
}

fn is_direct_fallback_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::BAD_GATEWAY | StatusCode::GATEWAY_TIMEOUT
    )
}

fn is_empty_upstream_error(err: &AppError) -> bool {
    is_empty_upstream_message(&err.message.to_ascii_lowercase())
}

fn is_provider_invalid_request_error(err: &AppError) -> bool {
    err.upstream_error_kind == Some(UpstreamErrorKind::ProviderInvalidRequest)
}

fn max_retries_for_app_error(err: &AppError, base_max: u32, empty_upstream_max: u32) -> u32 {
    if is_provider_invalid_request_error(err) || is_provider_rate_limited(err.status, &err.message)
    {
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
    public_model: &str,
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
    let usage = extract_usage_counts(path, &bytes);
    let bytes = rewrite_nonstream_response_model(path, bytes, public_model);
    let len = bytes.len() as u64;
    let has_output = response_has_assistant_output(path, &bytes);
    let mut rebuilt = Response::new(Body::from(bytes));
    *rebuilt.status_mut() = status;
    *rebuilt.headers_mut() = headers;
    Ok((rebuilt, len, usage, has_output))
}

fn rewrite_nonstream_response_model(path: &str, bytes: Bytes, public_model: &str) -> Bytes {
    let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
        return bytes;
    };
    if !rewrite_response_model_value(path, &mut value, public_model) {
        return bytes;
    }
    serde_json::to_vec(&value).map(Bytes::from).unwrap_or(bytes)
}

fn rewrite_stream_response_model(path: &str, bytes: Bytes, public_model: &str) -> Bytes {
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return bytes;
    };
    if !text.contains("data:") {
        return bytes;
    }

    let mut changed = false;
    let mut rewritten = String::with_capacity(text.len());
    for line_with_newline in text.split_inclusive('\n') {
        let (line, newline) = line_with_newline
            .strip_suffix('\n')
            .map(|line| (line, "\n"))
            .unwrap_or((line_with_newline, ""));
        let (line, carriage) = line
            .strip_suffix('\r')
            .map(|line| (line, "\r"))
            .unwrap_or((line, ""));
        let leading_len = line.len().saturating_sub(line.trim_start().len());
        let (leading, rest) = line.split_at(leading_len);
        let Some(data) = rest.strip_prefix("data:") else {
            rewritten.push_str(line);
            rewritten.push_str(carriage);
            rewritten.push_str(newline);
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            rewritten.push_str(line);
            rewritten.push_str(carriage);
            rewritten.push_str(newline);
            continue;
        }
        let Ok(mut value) = serde_json::from_str::<Value>(data) else {
            rewritten.push_str(line);
            rewritten.push_str(carriage);
            rewritten.push_str(newline);
            continue;
        };
        if rewrite_response_model_value(path, &mut value, public_model) {
            match serde_json::to_string(&value) {
                Ok(json) => {
                    rewritten.push_str(leading);
                    rewritten.push_str("data: ");
                    rewritten.push_str(&json);
                    rewritten.push_str(carriage);
                    rewritten.push_str(newline);
                    changed = true;
                }
                Err(_) => {
                    rewritten.push_str(line);
                    rewritten.push_str(carriage);
                    rewritten.push_str(newline);
                }
            }
        } else {
            rewritten.push_str(line);
            rewritten.push_str(carriage);
            rewritten.push_str(newline);
        }
    }

    if changed {
        Bytes::from(rewritten)
    } else {
        bytes
    }
}

fn rewrite_response_model_value(path: &str, value: &mut Value, public_model: &str) -> bool {
    let mut changed = false;
    if !public_model.is_empty()
        && matches!(path, "messages" | "chat/completions")
        && value.get("model").and_then(Value::as_str).is_some()
    {
        value["model"] = Value::String(public_model.to_string());
        changed = true;
    }
    if !public_model.is_empty() && path == "messages" {
        if let Some(message) = value.get_mut("message").and_then(Value::as_object_mut) {
            if message.get("model").and_then(Value::as_str).is_some() {
                message.insert("model".to_string(), Value::String(public_model.to_string()));
                changed = true;
            }
        }
    }
    if normalize_downstream_usage_value(path, value) {
        changed = true;
    }
    changed
}

fn normalize_downstream_usage_value(path: &str, value: &mut Value) -> bool {
    let mut changed = false;
    if let Some(usage) = value.get_mut("usage") {
        changed |= normalize_downstream_usage_object(path, usage);
    }
    if path == "messages" {
        if let Some(usage) = value
            .get_mut("message")
            .and_then(|message| message.get_mut("usage"))
        {
            changed |= normalize_downstream_usage_object(path, usage);
        }
    }
    changed
}

fn normalize_downstream_usage_object(path: &str, usage: &mut Value) -> bool {
    if !usage.is_object() {
        return false;
    }

    if path == "messages" {
        let provider_input = usage_u32(usage, "input_tokens");
        let output_tokens = usage_u32(usage, "output_tokens");
        let cache_read = usage_cache_read_u32(usage).unwrap_or(0);
        let cache_creation = usage_u32(usage, "cache_creation_input_tokens");
        let uncached_input =
            downstream_uncached_input_tokens(provider_input, cache_read, cache_creation, usage);
        usage["input_tokens"] = Value::from(provider_input);
        usage["output_tokens"] = Value::from(output_tokens);
        usage["cache_read_input_tokens"] = Value::from(cache_read);
        usage["cache_miss_input_tokens"] = Value::from(uncached_input);
        usage["zenproxy_billable_input_tokens"] = Value::from(uncached_input);
        usage["zenproxy_provider_input_tokens"] = Value::from(provider_input);
        usage["zenproxy_cache_r2_basis_tokens"] = Value::from(provider_input);
        usage["zenproxy_true_cache_read_ratio"] =
            Value::from(cache_ratio(provider_input, cache_read));
        usage["zenproxy_cache_contract_version"] = Value::from(2);
        return cache_read > 0 || usage_cache_miss_u32(usage).is_some();
    }

    let provider_prompt = usage_u32(usage, "prompt_tokens");
    let completion_tokens = usage_u32(usage, "completion_tokens");
    let cache_read = usage_cache_read_u32(usage).unwrap_or(0);
    let cache_creation = usage_u32(usage, "cache_creation_input_tokens");
    let uncached_prompt =
        downstream_uncached_input_tokens(provider_prompt, cache_read, cache_creation, usage);
    usage["prompt_tokens"] = Value::from(provider_prompt);
    usage["completion_tokens"] = Value::from(completion_tokens);
    usage["total_tokens"] = Value::from(provider_prompt.saturating_add(completion_tokens));
    usage["cache_read_input_tokens"] = Value::from(cache_read);
    usage["cache_miss_input_tokens"] = Value::from(uncached_prompt);
    usage["zenproxy_billable_input_tokens"] = Value::from(uncached_prompt);
    usage["zenproxy_provider_prompt_tokens"] = Value::from(provider_prompt);
    usage["zenproxy_cache_r2_basis_tokens"] = Value::from(provider_prompt);
    usage["zenproxy_true_cache_read_ratio"] = Value::from(cache_ratio(provider_prompt, cache_read));
    usage["zenproxy_cache_contract_version"] = Value::from(2);
    ensure_prompt_tokens_details_cached_tokens(usage, cache_read);
    cache_read > 0 || usage_cache_miss_u32(usage).is_some()
}

fn downstream_uncached_input_tokens(
    provider_input_tokens: u32,
    cache_read_tokens: u32,
    cache_creation_tokens: u32,
    usage: &Value,
) -> u32 {
    usage_cache_miss_u32(usage).unwrap_or_else(|| {
        provider_input_tokens
            .saturating_sub(cache_read_tokens)
            .saturating_sub(cache_creation_tokens)
    })
}

fn cache_ratio(provider_input_tokens: u32, cache_read_tokens: u32) -> f64 {
    if provider_input_tokens == 0 {
        0.0
    } else {
        (cache_read_tokens as f64) / (provider_input_tokens as f64)
    }
}

fn ensure_prompt_tokens_details_cached_tokens(usage: &mut Value, cache_read_tokens: u32) {
    if !usage
        .get("prompt_tokens_details")
        .is_some_and(|details| details.is_object())
    {
        usage["prompt_tokens_details"] = serde_json::json!({});
    }
    if let Some(details) = usage
        .get_mut("prompt_tokens_details")
        .and_then(Value::as_object_mut)
    {
        details.insert("cached_tokens".to_string(), Value::from(cache_read_tokens));
    }
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
        let public_model = telemetry.public_model.clone();
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
                    let downstream_bytes =
                        rewrite_stream_response_model(&path, bytes, &public_model);
                    let elapsed_ms = request_start.elapsed().as_millis() as u64;
                    if first_content_token_ms == 0 && !had_content && metrics.has_content_signal() {
                        first_content_token_ms = elapsed_ms;
                    }
                    if first_tool_call_ms == 0 && !had_tool && metrics.has_tool_signal() {
                        first_tool_call_ms = elapsed_ms;
                    }
                    match send_stream_bytes(&tx, downstream_bytes).await {
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
        telemetry.cache_miss_input_tokens = cache_miss_input_tokens(&usage);
        telemetry.provider_cache_observation = provider_cache_observation(&usage);
        telemetry.warmup_state =
            warmup_state_for(&telemetry.usk, &telemetry.provider_cache_observation);
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
            let stream_rate_limited = error_type == "upstream_429";
            let status_code =
                StatusCode::from_u16(telemetry.status).unwrap_or(StatusCode::BAD_GATEWAY);
            clear_session_pin_for_failure(
                &RequestMeta {
                    model: telemetry.public_model.clone(),
                    upstream_model: telemetry.upstream_model.clone(),
                    session_id: telemetry.session_id.clone(),
                    stream: telemetry.is_streaming,
                    body_size: telemetry.bytes_sent,
                    affinity_key: telemetry.affinity_key.clone(),
                    allow_direct_fallback: true,
                },
                status_code,
                &error_type,
            );
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
            let result = if stream_rate_limited {
                ResultKind::RateLimited
            } else {
                ResultKind::SoftFailure {
                    kind: ErrorKind::Other,
                }
            };
            state.pool_manager.report(
                telemetry.selected_node_id.clone(),
                result,
                stream_complete_ms,
            );
            lease_guard.mark_released();
        } else if metrics.has_rate_limited_error() && !metrics.has_assistant_output() {
            telemetry.outcome = "rate_limited".to_string();
            telemetry.failure_kind = "upstream_429".to_string();
            telemetry.failure_message =
                "upstream provider rate limited the stream before assistant output".to_string();
            telemetry.retry_chain.push(RequestAttemptTelemetry {
                attempt: telemetry.retry_count,
                node_id: telemetry.selected_node_id.clone(),
                node_url_redacted: telemetry.selected_node_url_redacted.clone(),
                status: 429,
                latency_ms: stream_complete_ms,
                outcome: "rate_limited".to_string(),
                error_type: "upstream_429".to_string(),
            });
            state.pool_manager.report(
                telemetry.selected_node_id.clone(),
                ResultKind::RateLimited,
                stream_complete_ms,
            );
            lease_guard.mark_released();
        } else if empty_output {
            telemetry.outcome = "empty_output".to_string();
            telemetry.failure_kind = "empty_output".to_string();
            telemetry.failure_message =
                "upstream returned no assistant content or tool call".to_string();
            crate::pool::session_pin::clear(&telemetry.upstream_model, &telemetry.session_id);
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
        record_dynamic_model_traffic_from_telemetry(&state, &telemetry);
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
    if lower.contains("rate limit") || lower.contains("rate_limit") || lower.contains("429") {
        "upstream_429"
    } else if lower.contains("decode") || lower.contains("decoding") {
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
    rate_limited_error_chunks: u64,
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
        if value_has_rate_limit_error(value) {
            self.rate_limited_error_chunks = self.rate_limited_error_chunks.saturating_add(1);
        }
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
                if let Some(input_tokens) = usage_u32_opt(usage, "input_tokens") {
                    self.usage.prompt_tokens = input_tokens;
                }
                if let Some(output_tokens) =
                    usage_u32_opt(usage, "output_tokens").filter(|tokens| *tokens > 0)
                {
                    self.usage.completion_tokens = output_tokens;
                }
                self.usage.total_tokens = self
                    .usage
                    .prompt_tokens
                    .saturating_add(self.usage.completion_tokens);
                if let Some(cache_creation) = usage_u32_opt(usage, "cache_creation_input_tokens") {
                    self.usage.cache_creation_input_tokens =
                        self.usage.cache_creation_input_tokens.max(cache_creation);
                }
                if let Some(cache_read) = usage_cache_read_u32(usage) {
                    self.usage.cache_read_input_tokens =
                        self.usage.cache_read_input_tokens.max(cache_read);
                    self.usage.cached_tokens = self.usage.cached_tokens.max(cache_read);
                }
                if let Some(cache_miss) = usage_cache_miss_u32(usage) {
                    self.usage.cache_miss_input_tokens =
                        max_optional_u32(self.usage.cache_miss_input_tokens, cache_miss);
                }
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
        if let Some(prompt_tokens) =
            usage_u32_opt(usage, "prompt_tokens").filter(|tokens| *tokens > 0)
        {
            self.usage.prompt_tokens = prompt_tokens;
        }
        if let Some(completion_tokens) =
            usage_u32_opt(usage, "completion_tokens").filter(|tokens| *tokens > 0)
        {
            self.usage.completion_tokens = completion_tokens;
        }
        if let Some(total_tokens) =
            usage_u32_opt(usage, "total_tokens").filter(|tokens| *tokens > 0)
        {
            self.usage.total_tokens = total_tokens;
        } else {
            self.usage.total_tokens = self
                .usage
                .prompt_tokens
                .saturating_add(self.usage.completion_tokens);
        }
        if let Some(cached_tokens) = usage_cached_tokens_u32(usage) {
            self.usage.cached_tokens = self.usage.cached_tokens.max(cached_tokens);
        }
        if let Some(cache_creation) = usage_u32_opt(usage, "cache_creation_input_tokens") {
            self.usage.cache_creation_input_tokens =
                self.usage.cache_creation_input_tokens.max(cache_creation);
        }
        if let Some(cache_read) = usage_cache_read_u32(usage) {
            self.usage.cache_read_input_tokens = self.usage.cache_read_input_tokens.max(cache_read);
            self.usage.cached_tokens = self.usage.cached_tokens.max(cache_read);
        }
        if let Some(cache_miss) = usage_cache_miss_u32(usage) {
            self.usage.cache_miss_input_tokens =
                max_optional_u32(self.usage.cache_miss_input_tokens, cache_miss);
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
            cache_miss_input_tokens: max_optional_usage(
                self.usage.cache_miss_input_tokens,
                self.fallback_usage.cache_miss_input_tokens,
            ),
        }
    }

    fn has_assistant_output(&self) -> bool {
        !self.completion_text.trim().is_empty()
            || self.text_output_chunks > 0
            || self.tool_output_chunks > 0
    }

    fn has_content_signal(&self) -> bool {
        !self.completion_text.trim().is_empty() || self.text_output_chunks > 0
    }

    fn has_tool_signal(&self) -> bool {
        self.tool_output_chunks > 0
    }

    fn has_rate_limited_error(&self) -> bool {
        self.rate_limited_error_chunks > 0
    }
}

fn value_has_rate_limit_error(value: &Value) -> bool {
    let Some(error) = value.get("error") else {
        return false;
    };
    let mut haystack = String::new();
    for field in ["type", "code", "message"] {
        if let Some(text) = error.get(field).and_then(Value::as_str) {
            haystack.push_str(text);
            haystack.push('\n');
        }
    }
    let lower = haystack.to_ascii_lowercase();
    lower.contains("rate_limit")
        || lower.contains("rate limit")
        || lower.contains("rate-limited")
        || lower.contains("rate limited")
        || lower.contains("429")
}

fn usage_u32(usage: &Value, name: &str) -> u32 {
    usage_u32_opt(usage, name).unwrap_or(0)
}

fn usage_u32_opt(usage: &Value, name: &str) -> Option<u32> {
    usage
        .get(name)
        .and_then(|value| value.as_u64())
        .map(|value| value.min(u32::MAX as u64) as u32)
}

fn usage_cached_tokens_u32(usage: &Value) -> Option<u32> {
    usage
        .get("prompt_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
        .or_else(|| usage.get("cached_tokens").and_then(Value::as_u64))
        .or_else(|| usage.get("prompt_cache_hit_tokens").and_then(Value::as_u64))
        .map(|value| value.min(u32::MAX as u64) as u32)
}

fn usage_cache_read_u32(usage: &Value) -> Option<u32> {
    usage_u32_opt(usage, "cache_read_input_tokens")
        .or_else(|| usage_u32_opt(usage, "prompt_cache_hit_tokens"))
        .or_else(|| usage_cached_tokens_u32(usage))
}

fn usage_cache_miss_u32(usage: &Value) -> Option<u32> {
    usage_u32_opt(usage, "cache_miss_input_tokens")
        .or_else(|| usage_u32_opt(usage, "prompt_cache_miss_tokens"))
}

fn max_optional_u32(current: Option<u32>, next: u32) -> Option<u32> {
    Some(current.unwrap_or(0).max(next))
}

fn max_optional_usage(left: Option<u32>, right: Option<u32>) -> Option<u32> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
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
        let cache_read = usage_cache_read_u32(usage).unwrap_or(0);
        UsageCounts {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens.saturating_add(completion_tokens),
            cached_tokens: cache_read,
            cache_creation_input_tokens: usage_u32(usage, "cache_creation_input_tokens"),
            cache_read_input_tokens: cache_read,
            cache_miss_input_tokens: usage_cache_miss_u32(usage),
        }
    } else {
        let cached_tokens = usage_cached_tokens_u32(usage).unwrap_or(0);
        UsageCounts {
            prompt_tokens: usage_u32(usage, "prompt_tokens"),
            completion_tokens: usage_u32(usage, "completion_tokens"),
            total_tokens: usage_u32(usage, "total_tokens"),
            cached_tokens,
            cache_creation_input_tokens: usage_u32(usage, "cache_creation_input_tokens"),
            cache_read_input_tokens: usage_u32(usage, "cache_read_input_tokens").max(cached_tokens),
            cache_miss_input_tokens: usage_cache_miss_u32(usage),
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

    macro_rules! affinity_key {
        (
            $public_model:expr,
            $upstream_model:expr,
            $path:expr,
            $source_client:expr,
            $cache_api_key_id:expr,
            $fallback_client_id:expr,
            $body_size:expr,
            $streaming:expr,
            $body:expr $(,)?
        ) => {{
            let _ = $streaming;
            build_affinity_key(AffinityKeyInput {
                public_model: $public_model,
                upstream_model: $upstream_model,
                path: $path,
                source_client: $source_client,
                cache_api_key_id: $cache_api_key_id,
                fallback_client_id: $fallback_client_id,
                body_size: $body_size,
                body: $body,
            })
        }};
    }

    fn request_meta(model: &str, upstream_model: &str, body_size: u64) -> RequestMeta {
        RequestMeta {
            model: model.to_string(),
            upstream_model: upstream_model.to_string(),
            session_id: "session".to_string(),
            stream: true,
            body_size,
            affinity_key: "affinity".to_string(),
            allow_direct_fallback: false,
        }
    }

    #[test]
    fn mimo_large_messages_caps_empty_output_retries() {
        let meta = request_meta("mimo-v2.5", "mimo-v2.5-free", 40_000);

        assert_eq!(
            effective_empty_upstream_max_retries("messages", &meta, 12),
            2
        );
        assert_eq!(effective_retry_budget_ms("messages", &meta, 45_000), 30_000);
    }

    #[test]
    fn mimo_huge_messages_caps_retry_budget_more_aggressively() {
        let meta = request_meta("mimo-v2.5", "mimo-v2.5-free", 220_000);

        assert_eq!(
            effective_empty_upstream_max_retries("messages", &meta, 12),
            2
        );
        assert_eq!(effective_retry_budget_ms("messages", &meta, 45_000), 20_000);
    }

    #[test]
    fn mimo_retry_cap_does_not_affect_chat_or_other_models() {
        let mimo = request_meta("mimo-v2.5", "mimo-v2.5-free", 220_000);
        let deepseek = request_meta("deepseek-v4-flash", "deepseek-v4-flash-free", 220_000);

        assert_eq!(
            effective_empty_upstream_max_retries("chat/completions", &mimo, 12),
            12
        );
        assert_eq!(
            effective_empty_upstream_max_retries("messages", &deepseek, 12),
            12
        );
        assert_eq!(
            effective_retry_budget_ms("messages", &deepseek, 45_000),
            45_000
        );
    }

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
    fn dynamic_claudecode_profile_allows_only_claudecode_client_policy() {
        let claudecode = ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header);
        let openclaw = ClientProfile::new(ClientKind::OpenClaw, ClientProfileSource::Header);
        let hermes = ClientProfile::new(ClientKind::Hermes, ClientProfileSource::Header);

        assert_eq!(
            apply_model_compatibility_profile(
                claudecode,
                ModelCompatibilityProfile::DynamicClaudeCodeCompatible
            )
            .kind,
            ClientKind::ClaudeCode
        );
        assert_eq!(
            apply_model_compatibility_profile(
                openclaw,
                ModelCompatibilityProfile::DynamicClaudeCodeCompatible
            )
            .kind,
            ClientKind::Unknown
        );
        assert_eq!(
            apply_model_compatibility_profile(
                hermes,
                ModelCompatibilityProfile::DynamicClaudeCodeCompatible
            )
            .kind,
            ClientKind::Unknown
        );
        assert_eq!(
            apply_model_compatibility_profile(
                claudecode,
                ModelCompatibilityProfile::DynamicGeneric
            )
            .kind,
            ClientKind::Unknown
        );
    }

    #[test]
    fn static_flash_preserves_explicit_openclaw_and_hermes_profiles() {
        for kind in [ClientKind::OpenClaw, ClientKind::Hermes] {
            let profile = ClientProfile::new(kind, ClientProfileSource::Header);
            assert_eq!(
                apply_model_compatibility_profile(profile, ModelCompatibilityProfile::StaticFlash)
                    .kind,
                kind
            );
        }
    }

    #[test]
    fn static_flash_lite_preserves_claudecode_profile() {
        let profile = ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header);

        assert_eq!(
            apply_model_compatibility_profile(profile, ModelCompatibilityProfile::StaticFlashLite)
                .kind,
            ClientKind::ClaudeCode
        );
    }

    #[test]
    fn deeply_nested_body_marker_scan_is_bounded_and_non_recursive() {
        let mut value = serde_json::json!("leaf");
        for _ in 0..512 {
            value = serde_json::json!([value]);
        }

        assert!(!body_contains_strong_client_marker(&value, "openclaw"));
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
    fn completion_tokens_without_visible_openai_output_is_empty() {
        let body = Bytes::from_static(
            br#"{"choices":[{"message":{"role":"assistant","content":null,"reasoning_content":null,"tool_calls":null}}],"usage":{"prompt_tokens":681,"completion_tokens":814,"total_tokens":1495}}"#,
        );

        assert!(!response_has_assistant_output("chat/completions", &body));
    }

    #[test]
    fn rewrites_openai_usage_for_downstream_cache_contract() {
        let body = Bytes::from_static(
            br#"{"model":"deepseek-v4-flash-free","choices":[{"message":{"role":"assistant","content":"ok"}}],"usage":{"prompt_tokens":1000,"completion_tokens":10,"total_tokens":1010,"prompt_tokens_details":{"cached_tokens":900}}}"#,
        );

        let rewritten = rewrite_nonstream_response_model("chat/completions", body, "deepseek");
        let value: Value = serde_json::from_slice(&rewritten).unwrap();

        assert_eq!(value["model"], "deepseek");
        assert_eq!(value["usage"]["prompt_tokens"], 1000);
        assert_eq!(value["usage"]["completion_tokens"], 10);
        assert_eq!(value["usage"]["total_tokens"], 1010);
        assert_eq!(
            value["usage"]["prompt_tokens_details"]["cached_tokens"],
            900
        );
        assert_eq!(value["usage"]["cache_read_input_tokens"], 900);
        assert_eq!(value["usage"]["cache_miss_input_tokens"], 100);
        assert_eq!(value["usage"]["zenproxy_billable_input_tokens"], 100);
        assert_eq!(value["usage"]["zenproxy_provider_prompt_tokens"], 1000);
        assert_eq!(value["usage"]["zenproxy_cache_contract_version"], 2);
    }

    #[test]
    fn rewrites_anthropic_usage_for_downstream_cache_contract() {
        let body = Bytes::from_static(
            br#"{"type":"message","model":"mimo-v2.5-free","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":1000,"output_tokens":10,"cache_read_input_tokens":900}}"#,
        );

        let rewritten = rewrite_nonstream_response_model("messages", body, "mimo");
        let value: Value = serde_json::from_slice(&rewritten).unwrap();

        assert_eq!(value["model"], "mimo");
        assert_eq!(value["usage"]["input_tokens"], 1000);
        assert_eq!(value["usage"]["output_tokens"], 10);
        assert_eq!(value["usage"]["cache_read_input_tokens"], 900);
        assert_eq!(value["usage"]["cache_miss_input_tokens"], 100);
        assert_eq!(value["usage"]["zenproxy_billable_input_tokens"], 100);
        assert_eq!(value["usage"]["zenproxy_provider_input_tokens"], 1000);
        assert_eq!(value["usage"]["zenproxy_cache_contract_version"], 2);
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
    fn extracts_deepseek_prompt_cache_hit_usage_counts() {
        let body = Bytes::from_static(
            br#"{"usage":{"input_tokens":100,"output_tokens":5,"prompt_cache_hit_tokens":70,"prompt_cache_miss_tokens":30}}"#,
        );

        let usage = extract_usage_counts("messages", &body);

        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.cached_tokens, 70);
        assert_eq!(usage.cache_read_input_tokens, 70);
        assert_eq!(usage.cache_miss_input_tokens, Some(30));
    }

    #[test]
    fn rewrites_anthropic_nonstream_model_to_public_model() {
        let body = Bytes::from_static(
            br#"{"type":"message","model":"mimo-v2.5-free","usage":{"input_tokens":100,"output_tokens":5,"cache_read_input_tokens":70,"cache_miss_input_tokens":30}}"#,
        );

        let rewritten = rewrite_nonstream_response_model("messages", body, "mimo-v2.5");
        let value: Value = serde_json::from_slice(&rewritten).unwrap();

        assert_eq!(value["model"], "mimo-v2.5");
        assert_eq!(value["usage"]["cache_read_input_tokens"], 70);
        assert_eq!(value["usage"]["cache_miss_input_tokens"], 30);
    }

    #[test]
    fn rewrites_anthropic_stream_message_start_model_to_public_model() {
        let body = Bytes::from_static(
            b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"deepseek-v4-flash-free\",\"usage\":{\"input_tokens\":100,\"cache_read_input_tokens\":70,\"cache_miss_input_tokens\":30}}}\n\n",
        );

        let rewritten = rewrite_stream_response_model("messages", body, "deepseek-v4-flash");
        let text = std::str::from_utf8(&rewritten).unwrap();

        assert!(text.contains("\"model\":\"deepseek-v4-flash\""));
        assert!(text.contains("\"cache_miss_input_tokens\":30"));
        assert!(!text.contains("deepseek-v4-flash-free"));
    }

    #[test]
    fn rewrites_anthropic_stream_nested_usage_for_downstream_cache_ratio() {
        let body = Bytes::from_static(
            b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"deepseek-v4-flash-free\",\"usage\":{\"input_tokens\":1000,\"cache_read_input_tokens\":900,\"cache_miss_input_tokens\":100}}}\n\n",
        );

        let rewritten = rewrite_stream_response_model("messages", body, "deepseek");
        let text = std::str::from_utf8(&rewritten).unwrap();
        let data = text
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .unwrap();
        let value: Value = serde_json::from_str(data).unwrap();

        assert_eq!(value["message"]["model"], "deepseek");
        assert_eq!(value["message"]["usage"]["input_tokens"], 1000);
        assert_eq!(value["message"]["usage"]["cache_read_input_tokens"], 900);
        assert_eq!(value["message"]["usage"]["cache_miss_input_tokens"], 100);
        assert_eq!(
            value["message"]["usage"]["zenproxy_billable_input_tokens"],
            100
        );
        assert_eq!(
            value["message"]["usage"]["zenproxy_provider_input_tokens"],
            1000
        );
    }

    #[test]
    fn rewrites_openai_stream_chunk_model_to_public_model() {
        let body = Bytes::from_static(
            b"data: {\"id\":\"chatcmpl_1\",\"model\":\"mimo-v2.5-free\",\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
        );

        let rewritten = rewrite_stream_response_model("chat/completions", body, "mimo-v2.5");
        let text = std::str::from_utf8(&rewritten).unwrap();

        assert!(text.contains("\"model\":\"mimo-v2.5\""));
        assert!(!text.contains("mimo-v2.5-free"));
    }

    #[test]
    fn rewrites_openai_stream_usage_for_downstream_cache_ratio() {
        let body = Bytes::from_static(
            b"data: {\"id\":\"chatcmpl_1\",\"model\":\"mimo-v2.5-free\",\"choices\":[],\"usage\":{\"prompt_tokens\":1000,\"completion_tokens\":10,\"total_tokens\":1010,\"prompt_tokens_details\":{\"cached_tokens\":900}}}\n\n",
        );

        let rewritten = rewrite_stream_response_model("chat/completions", body, "mimo");
        let text = std::str::from_utf8(&rewritten).unwrap();
        let data = text
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .unwrap();
        let value: Value = serde_json::from_str(data).unwrap();

        assert_eq!(value["model"], "mimo");
        assert_eq!(value["usage"]["prompt_tokens"], 1000);
        assert_eq!(value["usage"]["completion_tokens"], 10);
        assert_eq!(value["usage"]["total_tokens"], 1010);
        assert_eq!(value["usage"]["cache_read_input_tokens"], 900);
        assert_eq!(value["usage"]["cache_miss_input_tokens"], 100);
        assert_eq!(value["usage"]["zenproxy_billable_input_tokens"], 100);
        assert_eq!(value["usage"]["zenproxy_provider_prompt_tokens"], 1000);
    }

    #[test]
    fn transport_errors_only_hard_fail_when_proxy_specific() {
        assert!(matches!(
            result_kind_for_classified_error(ErrorKind::Timeout, "timeout"),
            ResultKind::Error {
                kind: ErrorKind::Timeout
            }
        ));
        assert!(matches!(
            result_kind_for_classified_error(ErrorKind::Other, "network"),
            ResultKind::SoftFailure {
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
    fn upstream_connection_error_does_not_bury_proxy_node() {
        let err = AppError {
            status: StatusCode::BAD_GATEWAY,
            message: "upstream connection error: error sending request for url (https://opencode.ai/zen/v1/chat/completions)".to_string(),
            upstream_headers: None,
            upstream_error_kind: None,
        };

        let (kind, outcome, error_type) = classify_app_error(&err);

        assert_eq!(kind, ErrorKind::Other);
        assert_eq!(outcome, "transport_error");
        assert_eq!(error_type, "network");
        assert!(matches!(
            result_kind_for_classified_error(kind, error_type),
            ResultKind::SoftFailure {
                kind: ErrorKind::Other
            }
        ));
    }

    #[test]
    fn provider_rate_limited_text_is_classified_as_rate_limited() {
        let err = AppError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "upstream provider rate limited the request".to_string(),
            upstream_headers: None,
            upstream_error_kind: None,
        };

        let (kind, outcome, error_type) = classify_app_error(&err);

        assert_eq!(kind, ErrorKind::Upstream5xx);
        assert_eq!(outcome, "rate_limited");
        assert_eq!(error_type, "upstream_429");
        assert!(matches!(
            result_kind_for_classified_error(kind, error_type),
            ResultKind::RateLimited
        ));
        assert_eq!(max_retries_for_app_error(&err, 2, 4), 0);
    }

    #[test]
    fn socks_rejection_is_hard_proxy_failure() {
        let err = AppError {
            status: StatusCode::BAD_GATEWAY,
            message: "upstream connection error: error sending request for url (https://opencode.ai/zen/v1/chat/completions); caused by: socks5 server rejected credentials".to_string(),
            upstream_headers: None,
            upstream_error_kind: None,
        };

        let (kind, outcome, error_type) = classify_app_error(&err);

        assert_eq!(kind, ErrorKind::SocksHandshake);
        assert_eq!(outcome, "transport_error");
        assert_eq!(error_type, "socks_handshake");
        assert!(matches!(
            result_kind_for_classified_error(kind, error_type),
            ResultKind::Error {
                kind: ErrorKind::SocksHandshake
            }
        ));
    }

    #[test]
    fn direct_fallback_statuses_cover_proxy_transport_responses() {
        assert!(is_direct_fallback_status(StatusCode::BAD_GATEWAY));
        assert!(is_direct_fallback_status(StatusCode::GATEWAY_TIMEOUT));
        assert!(!is_direct_fallback_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(!is_direct_fallback_status(
            StatusCode::INTERNAL_SERVER_ERROR
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
    fn stream_metrics_merges_anthropic_usage_without_zeroing_prompt_or_cache() {
        let mut metrics = StreamMetrics::new(UsageCounts::default());

        metrics.ingest(
            "messages",
            &Bytes::from_static(
                br#"event: message_start
data: {"type":"message_start","message":{"usage":{"input_tokens":55,"output_tokens":0}}}

"#,
            ),
        );
        metrics.ingest(
            "messages",
            &Bytes::from_static(
                br#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4,"cache_read_input_tokens":2}}

"#,
            ),
        );

        let usage = metrics.final_usage();

        assert_eq!(usage.prompt_tokens, 55);
        assert_eq!(usage.completion_tokens, 4);
        assert_eq!(usage.total_tokens, 59);
        assert_eq!(usage.cached_tokens, 2);
        assert_eq!(usage.cache_read_input_tokens, 2);
    }

    #[test]
    fn stream_metrics_accepts_deepseek_prompt_cache_hit_tokens() {
        let mut metrics = StreamMetrics::new(UsageCounts::default());

        metrics.ingest(
            "messages",
            &Bytes::from_static(
                br#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"input_tokens":120,"output_tokens":8,"prompt_cache_hit_tokens":33,"prompt_cache_miss_tokens":87}}

"#,
            ),
        );

        let usage = metrics.final_usage();

        assert_eq!(usage.prompt_tokens, 120);
        assert_eq!(usage.completion_tokens, 8);
        assert_eq!(usage.cached_tokens, 33);
        assert_eq!(usage.cache_read_input_tokens, 33);
    }

    #[test]
    fn mimo_family_uses_usk_affinity() {
        let body = serde_json::json!({
            "model": "mimo-v2.5-free",
            "messages":[{"role":"user","content":"a".repeat(80_000)}],
            "tools":[{"function":{"name":"Read"}}],
            "tool_choice":"auto"
        });
        let key = affinity_key!(
            "mimo-v2.5",
            "mimo-v2.5-free",
            "messages",
            "claude-code",
            "cache-api-key",
            "client",
            180_000,
            true,
            &body,
        );
        assert!(key.starts_with("mimo-v2.5-free:mimo-v2.5-free:claude-code:"));
        assert!(key.contains(":messages:"));
    }

    #[test]
    fn anthropic_messages_resolve_cache_identity_before_dispatch() {
        let body = serde_json::json!({
            "model": "deepseek-v4-flash-free",
            "system": [{"type": "text", "text": "static system"}],
            "messages": [{"role": "user", "content": "a".repeat(80_000)}],
            "tools": [{
                "name": "Read",
                "description": "read a file",
                "input_schema": {
                    "type": "object",
                    "required": ["file_path"],
                    "properties": {"file_path": {"type": "string"}}
                }
            }],
            "tool_choice": {"type": "auto"},
            "stream": true,
            "max_tokens": 32000
        });

        let (usk, icp_scope, prefix_32k_hash, session_id) = resolve_session_identity(
            "messages",
            &body,
            "deepseek-v4-flash-free",
            "claude-code",
            "cache-api-key",
            "fallback-client",
        );

        assert!(usk.starts_with("usk_v1:"));
        assert!(icp_scope.starts_with("icp:p32k:"));
        assert_eq!(prefix_32k_hash.len(), 16);
        assert!(session_id.starts_with("ses_"));

        let key = affinity_key!(
            "deepseek-v4-flash",
            "deepseek-v4-flash-free",
            "messages",
            "claude-code",
            "cache-api-key",
            "fallback-client",
            180_000,
            true,
            &body,
        );

        assert!(key.starts_with("deepseek-v4-flash-free:deepseek-v4-flash-free:claude-code:"));
        assert!(key.contains(":messages:"));
    }

    #[test]
    fn mimo_messages_get_session_identity_and_affinity() {
        let body = serde_json::json!({
            "model": "mimo-v2.5-free",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        });

        let (usk, _, prefix_32k_hash, session_id) = resolve_session_identity(
            "messages",
            &body,
            "mimo-v2.5-free",
            "claude-code",
            "cache-api-key",
            "fallback-client",
        );
        let key = affinity_key!(
            "mimo-v2.5",
            "mimo-v2.5-free",
            "messages",
            "claude-code",
            "cache-api-key",
            "fallback-client",
            180_000,
            true,
            &body,
        );

        assert!(usk.starts_with("usk_v1:"));
        assert_eq!(prefix_32k_hash.len(), 16);
        assert!(session_id.starts_with("ses_"));
        assert!(key.starts_with("mimo-v2.5-free:mimo-v2.5-free:claude-code:"));
        assert!(key.contains(":messages:"));
    }

    #[test]
    fn affinity_key_is_for_medium_and_large_requests() {
        let body = serde_json::json!({
            "model": "m",
            "messages":[{"role":"user","content":"hello"}]
        });
        assert!(affinity_key!(
            "m",
            "m-up",
            "chat/completions",
            "claude-code",
            "sk",
            "client",
            10,
            true,
            &body
        )
        .is_empty());
        let medium_nonstream_key = affinity_key!(
            "m",
            "m-up",
            "chat/completions",
            "claude-code",
            "sk",
            "client",
            200_000,
            false,
            &body,
        );
        assert!(medium_nonstream_key.starts_with("m-up:m:claude-code:"));
        assert!(medium_nonstream_key.contains("chat/completions"));

        let medium_key = affinity_key!(
            "m",
            "m-up",
            "chat/completions",
            "claude-code",
            "sk",
            "client",
            64_000,
            true,
            &body,
        );
        assert!(medium_key.starts_with("m-up:m:claude-code:"));

        let large_body = serde_json::json!({
            "model": "m",
            "messages":[{"role":"user","content":"a".repeat(80_000)}],
            "tools":[{"type":"function","function":{"name":"Read"}}],
            "tool_choice":"auto"
        });
        let key = affinity_key!(
            "m",
            "m-up",
            "chat/completions",
            "claude-code",
            "sk",
            "client",
            200_000,
            true,
            &large_body,
        );
        assert!(key.starts_with("m-up:m:claude-code:"));
        let without_tools = affinity_key!(
            "m",
            "m-up",
            "chat/completions",
            "claude-code",
            "sk",
            "client",
            200_000,
            true,
            &serde_json::json!({
                "model": "m",
                "messages":[{"role":"user","content":"a".repeat(80_000)}],
                "tool_choice":"auto"
            }),
        );
        assert_eq!(key, without_tools);
    }

    #[test]
    fn affinity_key_uses_stable_prefix_scope() {
        let prefix = "a".repeat(400_000);
        let first = serde_json::json!({
            "model": "m",
            "messages":[{"role":"user","content":prefix}],
            "tools":[{"type":"function","function":{"name":"Read"}}],
            "tool_choice":"auto"
        });
        let mut second = first.clone();
        second["messages"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"role":"user","content":"continue"}));
        let changed_tools = serde_json::json!({
            "model": "m",
            "messages":[{"role":"user","content":"a".repeat(400_000)}],
            "tools":[{"type":"function","function":{"name":"Write"}}],
            "tool_choice":"auto"
        });
        let changed_prefix = serde_json::json!({
            "model": "m",
            "messages":[{"role":"user","content":format!("b{}", "a".repeat(399_999))}],
            "tools":[{"type":"function","function":{"name":"Read"}}],
            "tool_choice":"auto"
        });

        let first_key = affinity_key!(
            "m",
            "m-up",
            "messages",
            "claude-code",
            "cache-api-key",
            "client",
            800_000,
            true,
            &first,
        );
        let second_key = affinity_key!(
            "m",
            "m-up",
            "messages",
            "claude-code",
            "cache-api-key",
            "client",
            820_000,
            true,
            &second,
        );
        let changed_tools_key = affinity_key!(
            "m",
            "m-up",
            "messages",
            "claude-code",
            "cache-api-key",
            "client",
            800_000,
            true,
            &changed_tools,
        );
        let changed_key = affinity_key!(
            "m",
            "m-up",
            "messages",
            "claude-code",
            "cache-api-key",
            "client",
            800_000,
            true,
            &changed_prefix,
        );

        assert_eq!(first_key, second_key);
        assert_eq!(first_key, changed_tools_key);
        assert_ne!(first_key, changed_key);
    }

    #[test]
    fn affinity_key_separates_source_clients() {
        let body = serde_json::json!({
            "messages":[{"role":"user","content":"a".repeat(80_000)}],
            "tools":[{"function":{"name":"Read"}}],
            "tool_choice":"auto"
        });

        let claude_code_key = affinity_key!(
            "m",
            "m-up",
            "messages",
            "claude-code",
            "cache-api-key",
            "client",
            180_000,
            true,
            &body,
        );
        let hermes_key = affinity_key!(
            "m",
            "m-up",
            "messages",
            "hermes",
            "cache-api-key",
            "client",
            180_000,
            true,
            &body,
        );

        assert_ne!(claude_code_key, hermes_key);
    }

    #[test]
    fn affinity_key_keeps_medium_stable_prefix_when_tail_grows() {
        let prefix = "a".repeat(80_000);
        let first = serde_json::json!({
            "model": "m",
            "messages":[{"role":"user","content":prefix}],
            "tools":[{"type":"function","function":{"name":"Read"}}],
            "tool_choice":"auto"
        });
        let mut second = first.clone();
        second["messages"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"role":"user","content":"continue"}));
        let changed_prefix = serde_json::json!({
            "model": "m",
            "messages":[{"role":"user","content":format!("b{}", "a".repeat(79_999))}],
            "tools":[{"type":"function","function":{"name":"Read"}}],
            "tool_choice":"auto"
        });

        let first_key = affinity_key!(
            "m",
            "m-up",
            "messages",
            "claude-code",
            "cache-api-key",
            "client",
            180_000,
            true,
            &first,
        );
        let second_key = affinity_key!(
            "m",
            "m-up",
            "messages",
            "claude-code",
            "cache-api-key",
            "client",
            190_000,
            true,
            &second,
        );
        let changed_key = affinity_key!(
            "m",
            "m-up",
            "messages",
            "claude-code",
            "cache-api-key",
            "client",
            180_000,
            true,
            &changed_prefix,
        );

        assert_eq!(first_key, second_key);
        assert_ne!(first_key, changed_key);
    }

    #[test]
    fn affinity_key_does_not_change_when_context_crosses_body_bucket() {
        let prefix = "a".repeat(80_000);
        let first = serde_json::json!({
            "messages":[{"role":"user","content":prefix}],
            "tools":[{"function":{"name":"Read"}}],
            "tool_choice":"auto"
        });
        let mut second = first.clone();
        second["messages"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"role":"user","content":"x".repeat(120_000)}));

        let first_key = affinity_key!(
            "m",
            "m-up",
            "messages",
            "claude-code",
            "cache-api-key",
            "client",
            120_000,
            true,
            &first,
        );
        let second_key = affinity_key!(
            "m",
            "m-up",
            "messages",
            "claude-code",
            "cache-api-key",
            "client",
            280_000,
            true,
            &second,
        );

        assert_eq!(first_key, second_key);
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
        assert_eq!(
            classify_stream_error_message("upstream provider rate limited the request"),
            "upstream_429"
        );
        assert_eq!(classify_stream_error_message("other"), "stream_error");
    }

    #[test]
    fn stream_metrics_detects_anthropic_rate_limit_error_event() {
        let mut metrics = StreamMetrics::new(UsageCounts::default());
        metrics.ingest(
            "messages",
            &Bytes::from_static(
                br#"event: error
data: {"type":"error","error":{"type":"rate_limit_error","message":"upstream provider rate limited the request"}}

"#,
            ),
        );

        assert!(metrics.has_rate_limited_error());
        assert!(!metrics.has_assistant_output());
    }

    #[test]
    fn stream_metrics_detects_openai_rate_limit_error_event() {
        let mut metrics = StreamMetrics::new(UsageCounts::default());
        metrics.ingest(
            "chat/completions",
            &Bytes::from_static(
                br#"data: {"error":{"message":"upstream provider rate limited the request"}}

data: [DONE]

"#,
            ),
        );

        assert!(metrics.has_rate_limited_error());
        assert!(!metrics.has_assistant_output());
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
