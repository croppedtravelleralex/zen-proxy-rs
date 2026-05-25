use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde::Serialize;
use serde_json::{json, Value};

use crate::collector::RequestFilter;

use crate::state::AppState;
use crate::v4::model::{ModelRegistry, StaticModelRegistry};

pub struct AdminService;

impl AdminService {
    pub fn check_auth(headers: &HeaderMap, state: &AppState) -> Result<(), StatusCode> {
        let cfg = state.config.read().unwrap();
        let key = match &cfg.admin_api_key {
            Some(k) if !k.is_empty() => k,
            _ => return Err(StatusCode::UNAUTHORIZED),
        };
        let provided = headers
            .get(http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(|s| s.to_string())
            .or_else(|| {
                headers
                    .get("x-api-key")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string())
            });
        match provided {
            Some(ref p) if p == key => Ok(()),
            _ => Err(StatusCode::UNAUTHORIZED),
        }
    }

    pub fn ok_response<T: Serialize>(data: T) -> Response {
        Json(json!({ "success": true, "data": data })).into_response()
    }
    pub fn ok_status() -> Response {
        Json(json!({ "success": true, "status": "ok" })).into_response()
    }
    pub fn error_response<S: Into<String>>(code: StatusCode, msg: S) -> Response {
        (code, Json(json!({ "success": false, "error": msg.into() }))).into_response()
    }

    // --- Health ---
    pub fn health(state: &AppState) -> Response {
        let pools = state.pool_manager.pool_stats();
        Self::ok_response(json!({
            "status":"ok", "version": env!("CARGO_PKG_VERSION"),
            "uptime_secs": state.startup_time.elapsed().as_secs(),
            "pid": std::process::id(),
            "pools": {
                "dispatch": pools.dispatch_size, "active": pools.active_size,
                "ratelimited": pools.ratelimited_size, "dead": pools.dead_size,
                "total": pools.total(), "fuse": pools.fuse,
            },
            "upstream": { "backoff": state.upstream_health.is_backoff() }
        }))
    }
    pub fn health_live() -> Response {
        Self::ok_response(json!({ "status":"alive" }))
    }
    pub fn health_ready(state: &AppState) -> Response {
        let pools = state.pool_manager.pool_stats();
        let cfg = state.config.read().unwrap();
        let payload = json!({
            "proxy_ready": pools.dispatch_size > 0,
            "direct_fallback_active": cfg.allow_direct_fallback,
            "dispatch_size": pools.dispatch_size,
            "ratelimited_size": pools.ratelimited_size,
            "dead_size": pools.dead_size,
            "nodes_file": cfg.nodes_file,
        });
        if pools.dispatch_size > 0 {
            Self::ok_response(json!({ "status":"ready", "details": payload }))
        } else if cfg.allow_direct_fallback {
            Self::ok_response(json!({ "status":"direct_fallback_only", "details": payload }))
        } else {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "success": false,
                    "error": "no healthy upstream nodes",
                    "data": {
                        "status": "not_ready",
                        "details": payload
                    }
                })),
            )
                .into_response()
        }
    }

    // --- Stats ---
    pub fn stats(state: &AppState) -> Response {
        let s = state.collector.snapshot();
        Self::ok_response(json!({
            "total_requests": s.requests.total, "success": s.requests.success,
            "count_429": s.requests.count_429, "count_4xx": s.requests.count_4xx,
            "count_5xx": s.requests.count_5xx,
            "bytes_sent": s.requests.bytes_sent, "bytes_received": s.requests.bytes_received,
            "rpm": s.requests.rpm, "avg_latency_ms": s.requests.avg_latency_ms,
            "current_bps": s.system.current_bps,
        }))
    }
    pub fn stats_models(state: &AppState) -> Response {
        let agg = state.collector.aggregator_snapshot();
        Self::ok_response(agg)
    }
    pub fn stats_nodes(state: &AppState) -> Response {
        let summary = state.ledger.summary();
        Self::ok_response(summary.get("by_node").cloned().unwrap_or(json!({})))
    }
    pub fn stats_pools(state: &AppState) -> Response {
        let p = state.pool_manager.pool_stats();
        Self::ok_response(json!({
            "dispatch": p.dispatch_size, "active": p.active_size,
            "ratelimited": p.ratelimited_size, "dead": p.dead_size,
            "cooldown": p.cooldown_size, "budget_limited": p.budget_limited_size,
            "total": p.total(), "transitions": p.pool_transitions,
            "concurrency": p.active_concurrency, "leased": p.leased_count, "fuse": p.fuse,
        }))
    }
    pub fn stats_upstream(state: &AppState) -> Response {
        Self::ok_response(json!({ "backoff": state.upstream_health.is_backoff() }))
    }
    pub fn routes(_state: &AppState) -> Response {
        Self::ok_response(json!({
            "public": [
                {"method":"GET","path":"/"},
                {"method":"GET","path":"/health"},
                {"method":"GET","path":"/metrics"},
                {"method":"GET","path":"/models"},
                {"method":"GET","path":"/v1/models"},
                {"method":"GET","path":"/v1/models/{model_id}"},
                {"method":"POST","path":"/v1/chat/completions"},
                {"method":"POST","path":"/v1/messages"}
            ],
            "admin": [
                {"method":"GET","path":"/admin/health"},
                {"method":"GET","path":"/admin/health/live"},
                {"method":"GET","path":"/admin/health/ready"},
                {"method":"GET","path":"/admin/routes"},
                {"method":"GET","path":"/admin/runtime"},
                {"method":"GET","path":"/admin/models"},
                {"method":"GET","path":"/admin/models/{model_id}"},
                {"method":"GET","path":"/admin/budget"},
                {"method":"GET","path":"/admin/budget/nodes"},
                {"method":"GET","path":"/admin/stats"},
                {"method":"GET","path":"/admin/stats/models"},
                {"method":"GET","path":"/admin/stats/nodes"},
                {"method":"GET","path":"/admin/stats/pools"},
                {"method":"GET","path":"/admin/stats/upstream"},
                {"method":"GET","path":"/admin/pools"},
                {"method":"GET","path":"/admin/pools/{name}"},
                {"method":"GET,POST","path":"/admin/fuse"},
                {"method":"GET","path":"/admin/requests"},
                {"method":"GET","path":"/admin/requests/recent"},
                {"method":"GET","path":"/admin/requests/summary"},
                {"method":"GET","path":"/admin/requests/timings"},
                {"method":"GET","path":"/admin/requests/models"},
                {"method":"GET","path":"/admin/requests/nodes"},
                {"method":"GET","path":"/admin/requests/{rid}"},
                {"method":"GET","path":"/admin/requests/export"},
                {"method":"GET","path":"/admin/events"},
                {"method":"GET","path":"/admin/events/recent"},
                {"method":"GET","path":"/admin/events/probes"},
                {"method":"GET","path":"/admin/ledger"},
                {"method":"GET","path":"/admin/ledger/models"},
                {"method":"GET","path":"/admin/ledger/keys"},
                {"method":"GET","path":"/admin/ledger/streams"},
                {"method":"GET","path":"/admin/config"},
                {"method":"POST","path":"/admin/config/reload"},
                {"method":"GET","path":"/admin/config/validation"},
                {"method":"GET","path":"/admin/system/uptime"},
                {"method":"GET","path":"/admin/system/info"},
                {"method":"POST","path":"/admin/system/log-level/{level}"},
                {"method":"GET,POST","path":"/admin/nodes"},
                {"method":"DELETE","path":"/admin/nodes/{node_id}"},
                {"method":"GET","path":"/admin/nodes/{node_id}/budget"},
                {"method":"POST","path":"/admin/nodes/{node_id}/probe"},
                {"method":"POST","path":"/admin/nodes/{node_id}/recover"},
                {"method":"POST","path":"/admin/probe/now"}
            ]
        }))
    }
    pub fn runtime(state: &AppState) -> Response {
        let cfg = state.config.read().unwrap();
        let p = state.pool_manager.pool_stats();
        Self::ok_response(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
            "uptime_secs": state.startup_time.elapsed().as_secs(),
            "bind_address": cfg.bind_address,
            "port": cfg.port,
            "provider_mode": cfg.zen_provider_mode.to_string(),
            "v4_model_registry_active": cfg.v4_model_registry_active(),
            "upstream_base": cfg.upstream_base,
            "allow_direct_fallback": cfg.allow_direct_fallback,
            "context_governance": {
                "request_body_limit_mb": cfg.request_body_limit_mb,
                "compactor_mode": cfg.zen_compactor_mode.to_string(),
                "artifact_cache_mode": cfg.zen_artifact_cache_mode.to_string(),
                "warn_body_mb": cfg.context_warn_body_mb,
                "compact_body_mb": cfg.context_compact_body_mb,
                "target_body_mb": cfg.context_target_body_mb,
                "upstream_body_limit_mb": cfg.context_upstream_body_limit_mb,
                "token_warn": cfg.context_token_warn,
                "token_compact": cfg.context_token_compact,
                "token_target": cfg.context_token_target,
            },
            "global_budget": cfg.global_budget_redis_url.as_ref().map(|_| json!({
                "configured": true,
                "instance_id": cfg.instance_id,
                "window_secs": cfg.node_budget_window_secs,
                "lease_ttl_secs": cfg.node_lease_ttl_secs,
            })).unwrap_or_else(|| json!({"configured": false})),
            "pools": {
                "dispatch": p.dispatch_size,
                "active": p.active_size,
                "ratelimited": p.ratelimited_size,
                "dead": p.dead_size,
                "cooldown": p.cooldown_size,
                "budget_limited": p.budget_limited_size,
                "leased": p.leased_count,
                "fuse": p.fuse,
            }
        }))
    }
    pub fn models(state: &AppState) -> Response {
        let cfg = state.config.read().unwrap();
        if cfg.v4_model_registry_active() {
            let registry = StaticModelRegistry;
            let models: Vec<Value> = registry
                .public_models()
                .into_iter()
                .map(|model| {
                    json!({
                        "id": model.id,
                        "upstream_id": model.upstream_id,
                        "owned_by": "deepseek",
                        "endpoints": ["openai_chat_completions", "anthropic_messages"]
                    })
                })
                .collect();
            Self::ok_response(json!({"mode": "v4", "models": models}))
        } else {
            Self::ok_response(json!({
                "mode": "legacy",
                "models": [
                    {"id":"deepseek-v4-flash","upstream_id":"deepseek-v4-flash"},
                    {"id":"deepseek-v4-pro","upstream_id":"deepseek-v4-pro"}
                ]
            }))
        }
    }
    pub fn model_detail(state: &AppState, model_id: &str) -> Response {
        let cfg = state.config.read().unwrap();
        if cfg.v4_model_registry_active() {
            let registry = StaticModelRegistry;
            match registry.resolve(model_id) {
                Ok(resolved) => Self::ok_response(json!({
                    "id": resolved.public_model,
                    "upstream_id": resolved.upstream_model,
                    "mode": "v4",
                    "endpoints": ["openai_chat_completions", "anthropic_messages"]
                })),
                Err(_) => Self::error_response(StatusCode::NOT_FOUND, "model not found"),
            }
        } else {
            match model_id {
                "deepseek-v4-flash" | "deepseek-v4-pro" => Self::ok_response(json!({
                    "id": model_id,
                    "upstream_id": model_id,
                    "mode": "legacy"
                })),
                _ => Self::error_response(StatusCode::NOT_FOUND, "model not found"),
            }
        }
    }
    pub fn budget(state: &AppState) -> Response {
        let cfg = state.config.read().unwrap();
        let p = state.pool_manager.pool_stats();
        Self::ok_response(json!({
            "global": {
                "redis_configured": cfg.global_budget_redis_url.is_some(),
                "instance_id": cfg.instance_id,
                "window_secs": cfg.node_budget_window_secs,
                "lease_ttl_secs": cfg.node_lease_ttl_secs,
            },
            "limits": {
                "max_calls_per_window": cfg.node_max_calls_per_window,
                "max_tokens_per_window": cfg.node_max_tokens_per_window,
                "max_kb_per_window": cfg.node_max_kb_per_window,
                "cooldown_secs": cfg.node_budget_cooldown_secs,
            },
            "current": {
                "dispatch": p.dispatch_size,
                "cooldown": p.cooldown_size,
                "budget_limited": p.budget_limited_size,
                "leased": p.leased_count,
                "active": p.active_size,
            }
        }))
    }
    pub fn budget_nodes(state: &AppState) -> Response {
        Self::ok_response(json!({
            "nodes": state.pool_manager.budget_details()
        }))
    }
    pub fn node_budget(state: &AppState, node_id: &str) -> Response {
        match state.pool_manager.node_budget_detail(node_id) {
            Some(detail) => Self::ok_response(detail),
            None => Self::error_response(StatusCode::NOT_FOUND, "node not found"),
        }
    }
    pub fn pools(state: &AppState) -> Response {
        Self::stats_pools(state)
    }
    pub fn nodes(state: &AppState) -> Response {
        let p = state.pool_manager.pool_stats();
        let cfg = state.config.read().unwrap();
        Self::ok_response(json!({
            "nodes_file": cfg.nodes_file,
            "allow_direct_fallback": cfg.allow_direct_fallback,
            "pools": {
                "dispatch": p.dispatch_size,
                "active": p.active_size,
                "ratelimited": p.ratelimited_size,
                "dead": p.dead_size,
                "cooldown": p.cooldown_size,
                "budget_limited": p.budget_limited_size,
                "leased": p.leased_count,
                "total": p.total(),
                "fuse": p.fuse,
            }
        }))
    }
    pub fn pool_by_name(state: &AppState, name: &str) -> Response {
        let p = state.pool_manager.pool_stats();
        match name {
            "dispatch" => Self::ok_response(
                json!({"name":"dispatch","size":p.dispatch_size,"cooldown":p.cooldown_size,"budget_limited":p.budget_limited_size,"leased":p.leased_count}),
            ),
            "active" => Self::ok_response(json!({"name":"active","size":p.active_size})),
            "ratelimited" => {
                Self::ok_response(json!({"name":"ratelimited","size":p.ratelimited_size}))
            }
            "dead" => Self::ok_response(json!({"name":"dead","size":p.dead_size})),
            _ => Self::error_response(StatusCode::NOT_FOUND, "unknown pool"),
        }
    }

    // --- Fuse ---
    pub fn fuse_status(state: &AppState) -> Response {
        let p = state.pool_manager.pool_stats();
        Self::ok_response(
            json!({"fuse":p.fuse,"pools":{"dispatch":p.dispatch_size,"active":p.active_size,"ratelimited":p.ratelimited_size,"dead":p.dead_size,"cooldown":p.cooldown_size,"budget_limited":p.budget_limited_size,"leased":p.leased_count}}),
        )
    }
    pub fn fuse_set(state: &AppState, open: bool) -> Response {
        if open {
            state.pool_manager.fuse_all();
        } else {
            state.pool_manager.unfuse_all();
        }
        Self::fuse_status(state)
    }

    // --- Requests ---
    pub fn requests_list(state: &AppState, filter: &RequestFilter) -> Response {
        let result = state.collector.query_requests(filter);
        let items: Vec<Value> = result.items.iter().map(|r| json!(r)).collect();
        let mut resp = json!({ "success": true, "data": items });
        if let Some(c) = result.next_cursor {
            resp["meta"] = json!({ "next_cursor": c });
        }
        Json(resp).into_response()
    }
    pub fn request_detail(state: &AppState, rid: &str) -> Response {
        let filter = RequestFilter {
            rid: Some(rid.into()),
            ..Default::default()
        };
        let result = state.collector.query_requests(&filter);
        match result.items.into_iter().next() {
            Some(r) => Self::ok_response(json!(r)),
            None => Self::error_response(StatusCode::NOT_FOUND, "request not found"),
        }
    }
    pub fn requests_recent(state: &AppState) -> Response {
        Self::ok_response(json!(
            state
                .collector
                .query_requests(&RequestFilter {
                    limit: 100,
                    ..Default::default()
                })
                .items
        ))
    }
    pub fn requests_summary(state: &AppState) -> Response {
        let s = state.collector.snapshot();
        Self::ok_response(
            json!({"total":s.requests.total,"success":s.requests.success,"count_429":s.requests.count_429,"count_4xx":s.requests.count_4xx,"count_5xx":s.requests.count_5xx,"rpm":s.requests.rpm,"avg_latency_ms":s.requests.avg_latency_ms}),
        )
    }
    pub fn requests_timings(state: &AppState) -> Response {
        let result = state.collector.query_requests(&RequestFilter {
            limit: 100,
            ..Default::default()
        });
        let count = result.items.len() as u64;
        let mut sums = serde_json::Map::new();
        let mut add_avg = |name: &str, sum: u64| {
            sums.insert(
                name.to_string(),
                json!(if count > 0 {
                    sum as f64 / count as f64
                } else {
                    0.0
                }),
            );
        };
        add_avg(
            "dispatch_wait_ms",
            result
                .items
                .iter()
                .map(|r| r.timings.dispatch_wait_ms)
                .sum(),
        );
        add_avg(
            "upstream_response_ms",
            result
                .items
                .iter()
                .map(|r| r.timings.upstream_response_ms)
                .sum(),
        );
        add_avg(
            "first_chunk_ms",
            result.items.iter().map(|r| r.timings.first_chunk_ms).sum(),
        );
        add_avg(
            "stream_complete_ms",
            result
                .items
                .iter()
                .map(|r| r.timings.stream_complete_ms)
                .sum(),
        );
        add_avg(
            "total_ms",
            result.items.iter().map(|r| r.timings.total_ms).sum(),
        );
        let recent: Vec<Value> = result
            .items
            .iter()
            .map(|r| {
                json!({
                    "rid": r.rid,
                    "ts": r.ts,
                    "model": r.model,
                    "status": r.status,
                    "stream": r.is_streaming,
                    "node": r.selected_node_id,
                    "failure_kind": r.failure_kind,
                    "retry_chain": r.retry_chain,
                    "timings": r.timings,
                    "legacy": {
                        "latency_total_ms": r.latency_total_ms,
                        "upstream_ms": r.upstream_ms,
                        "ttft_ms": r.ttft_ms
                    }
                })
            })
            .collect();
        Self::ok_response(json!({
            "count": count,
            "avg": sums,
            "recent": recent
        }))
    }
    pub fn requests_models(state: &AppState) -> Response {
        Self::stats_models(state)
    }
    pub fn requests_nodes(state: &AppState) -> Response {
        Self::stats_nodes(state)
    }
    pub fn requests_export(state: &AppState, limit: usize) -> Response {
        let result = state.collector.query_requests(&RequestFilter {
            limit,
            ..Default::default()
        });
        let mut body = String::new();
        for item in &result.items {
            if let Ok(line) = serde_json::to_string(item) {
                body.push_str(&line);
                body.push('\n');
            }
        }
        Response::builder()
            .header("content-type", "application/x-ndjson")
            .body(axum::body::Body::from(body))
            .unwrap()
    }

    // --- Events ---
    pub fn events(state: &AppState) -> Response {
        let s = state.collector.snapshot();
        Self::ok_response(json!({"pool_transitions":s.pools.pool_transitions}))
    }
    pub fn events_recent(state: &AppState) -> Response {
        Self::ok_response(state.collector.recent_events(100))
    }
    pub fn events_probes(state: &AppState) -> Response {
        let all = state.collector.recent_events(500);
        let probes: Vec<_> = all
            .into_iter()
            .filter(|e| e.reason.starts_with("probe_"))
            .collect();
        Self::ok_response(probes)
    }

    // --- Ledger ---
    pub fn ledger_summary(state: &AppState) -> Response {
        Self::ok_response(state.ledger.summary())
    }
    pub fn ledger_models(state: &AppState) -> Response {
        let m = state.ledger.by_model_summary();
        Self::ok_response(m.into_iter().map(|(k,v)| (k,json!({"requests":v.requests,"success":v.success,"429":v.count_429,"5xx":v.count_5xx}))).collect::<Value>())
    }
    pub fn ledger_keys(state: &AppState) -> Response {
        let m = state.ledger.by_key_summary();
        Self::ok_response(m.into_iter().map(|(k,v)| (k,json!({"requests":v.requests,"success":v.success,"429":v.count_429,"5xx":v.count_5xx}))).collect::<Value>())
    }
    pub fn ledger_streams(state: &AppState) -> Response {
        let m = state.ledger.by_stream_summary();
        Self::ok_response(m.into_iter().map(|(k,v)| (if k{"stream"}else{"non_stream"},json!({"requests":v.requests,"success":v.success,"429":v.count_429,"5xx":v.count_5xx}))).collect::<Value>())
    }

    // --- Config ---
    pub fn config(state: &AppState) -> Response {
        let cfg = state.config.read().unwrap();
        Self::ok_response(json!({
            "upstream_base": cfg.upstream_base,
            "pool_max_retries": cfg.pool_max_retries,
            "v4_retry_budget_ms": cfg.v4_retry_budget_ms,
            "probe_timeout_secs": cfg.probe_timeout_secs,
            "allow_direct_fallback": cfg.allow_direct_fallback,
            "pool_starvation_retry_after_secs": cfg.pool_starvation_retry_after_secs,
            "zen_provider_mode": cfg.zen_provider_mode.to_string(),
            "v4_model_registry_enabled": cfg.v4_model_registry_enabled,
            "admin_api_key_configured": cfg.admin_api_key.is_some(),
            "proxy_api_key_configured": cfg.proxy_api_key.is_some(),
            "instance_id": cfg.instance_id,
            "global_budget_redis_configured": cfg.global_budget_redis_url.is_some(),
            "context_governance": {
                "request_body_limit_mb": cfg.request_body_limit_mb,
                "compactor_mode": cfg.zen_compactor_mode.to_string(),
                "artifact_cache_mode": cfg.zen_artifact_cache_mode.to_string(),
                "artifact_cache_dir": cfg.artifact_cache_dir,
                "artifact_cache_max_mb": cfg.artifact_cache_max_mb,
                "artifact_cache_ttl_hours": cfg.artifact_cache_ttl_hours,
                "warn_body_mb": cfg.context_warn_body_mb,
                "compact_body_mb": cfg.context_compact_body_mb,
                "target_body_mb": cfg.context_target_body_mb,
                "upstream_body_limit_mb": cfg.context_upstream_body_limit_mb,
                "token_warn": cfg.context_token_warn,
                "token_compact": cfg.context_token_compact,
                "token_target": cfg.context_token_target,
                "large_chunk_bytes": cfg.context_large_chunk_bytes,
                "preserve_recent_messages": cfg.context_preserve_recent_messages,
            },
            "node_budget": {
                "max_calls_per_window": cfg.node_max_calls_per_window,
                "max_tokens_per_window": cfg.node_max_tokens_per_window,
                "max_kb_per_window": cfg.node_max_kb_per_window,
                "cooldown_secs": cfg.node_budget_cooldown_secs,
                "window_secs": cfg.node_budget_window_secs,
                "lease_ttl_secs": cfg.node_lease_ttl_secs,
            },
        }))
    }
    pub fn config_reload(state: &AppState) -> Response {
        *state.config.write().unwrap() = crate::config::Config::from_env();
        tracing::info!("config reloaded from env");
        Self::ok_status()
    }
    pub fn config_validation(state: &AppState) -> Response {
        let cfg = state.config.read().unwrap();
        let mut warnings: Vec<String> = Vec::new();
        if cfg.admin_api_key.is_none() {
            warnings.push("ADMIN_API_KEY is not set — admin endpoints reject all requests".into());
        }
        if cfg.proxy_api_key.is_none() {
            warnings.push("PROXY_API_KEY is not set — proxy is open to all tokens".into());
        }
        if cfg.pool_max_retries == 0 {
            warnings.push("POOL_MAX_RETRIES is 0 — no retries on failure".into());
        }
        if cfg.allow_direct_fallback {
            warnings
                .push("ALLOW_DIRECT_FALLBACK is enabled — requests may bypass proxy pool".into());
        }
        if cfg.context_target_body_mb >= cfg.context_upstream_body_limit_mb {
            warnings.push(
                "CONTEXT_TARGET_BODY_MB should stay below CONTEXT_UPSTREAM_BODY_LIMIT_MB".into(),
            );
        }
        if cfg.zen_compactor_mode.to_string() == "off"
            && cfg.request_body_limit_mb > cfg.context_upstream_body_limit_mb
        {
            warnings.push(
                "ZEN_COMPACTOR_MODE=off with a large ingress limit may forward overlarge upstream requests"
                    .into(),
            );
        }
        Self::ok_response(json!({
            "valid": warnings.is_empty(),
            "warnings": warnings,
        }))
    }

    // --- System ---
    pub fn system_uptime(state: &AppState) -> Response {
        Self::ok_response(
            json!({"uptime_secs":state.startup_time.elapsed().as_secs(),"pid":std::process::id(),"version":env!("CARGO_PKG_VERSION")}),
        )
    }
    pub fn system_info(state: &AppState) -> Response {
        let p = state.pool_manager.pool_stats();
        let s = state.collector.snapshot();
        Self::ok_response(
            json!({"version":env!("CARGO_PKG_VERSION"),"uptime_secs":state.startup_time.elapsed().as_secs(),"pid":std::process::id(),"pools":{"dispatch":p.dispatch_size,"active":p.active_size,"ratelimited":p.ratelimited_size,"dead":p.dead_size,"cooldown":p.cooldown_size,"budget_limited":p.budget_limited_size,"leased":p.leased_count,"fuse":p.fuse},"requests":{"total":s.requests.total,"success":s.requests.success,"rpm":s.requests.rpm},"upstream":{"backoff":state.upstream_health.is_backoff()}}),
        )
    }
    pub fn system_log_level(level: &str) -> Response {
        match crate::set_log_level(level) {
            Ok(()) => Self::ok_status(),
            Err(e) => Self::error_response(StatusCode::BAD_REQUEST, e),
        }
    }

    // --- Node operations (via sub-pools) ---
    pub fn node_add(state: &AppState, url: &str) -> Response {
        state.pool_manager.add_node(url);
        Self::ok_status()
    }
    pub fn node_delete(state: &AppState, node_id: &str) -> Response {
        state.pool_manager.remove_node(node_id);
        Self::ok_status()
    }
    pub fn node_probe(state: &AppState, node_id: &str) -> Response {
        match state.pool_manager.probe_node(node_id) {
            Some(result) => {
                Self::ok_response(json!({"success":result.success,"latency_ms":result.latency_ms}))
            }
            None => Self::error_response(StatusCode::NOT_FOUND, "node not found"),
        }
    }
    pub fn node_recover(state: &AppState, node_id: &str) -> Response {
        state.pool_manager.recover_node(node_id);
        Self::ok_status()
    }
    pub fn probe_now(state: &AppState) -> Response {
        state.pool_manager.probe_all();
        Self::ok_status()
    }
}
