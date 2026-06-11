use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::collector::RequestFilter;
use crate::ledger::{sanitize_json_value, sanitize_text};

use crate::state::AppState;
use crate::v4::model::{EffectiveModelRegistry, ModelRegistry};
use crate::v4::model_discovery::DiscoveredModelState;

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
        Json(sanitize_json_value(
            json!({ "success": true, "data": data }),
        ))
        .into_response()
    }
    pub fn ok_status() -> Response {
        Json(json!({ "success": true, "status": "ok" })).into_response()
    }
    pub fn error_response<S: Into<String>>(code: StatusCode, msg: S) -> Response {
        (
            code,
            Json(json!({ "success": false, "error": sanitize_text(&msg.into()) })),
        )
            .into_response()
    }

    pub fn audit_filter(params: &std::collections::HashMap<String, String>) -> RequestFilter {
        RequestFilter {
            rid: params.get("rid").cloned(),
            model: params.get("model").cloned(),
            node_url: params
                .get("node")
                .cloned()
                .or_else(|| params.get("node_id").cloned()),
            status: params.get("status").and_then(|v| v.parse().ok()),
            since: params
                .get("from")
                .or_else(|| params.get("since"))
                .and_then(|v| parse_ts(v)),
            until: params
                .get("to")
                .or_else(|| params.get("until"))
                .and_then(|v| parse_ts(v)),
            limit: params
                .get("limit")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1000),
            cursor: None,
        }
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
            "nodes_file": sanitize_text(&cfg.nodes_file),
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
                {"method":"POST","path":"/admin/models/{model_id}/promote"},
                {"method":"POST","path":"/admin/models/{model_id}/demote"},
                {"method":"POST","path":"/admin/models/{model_id}/quarantine"},
                {"method":"GET","path":"/admin/budget"},
                {"method":"GET","path":"/admin/budget/nodes"},
                {"method":"GET","path":"/admin/stats"},
                {"method":"GET","path":"/admin/stats/models"},
                {"method":"GET","path":"/admin/stats/nodes"},
                {"method":"GET","path":"/admin/stats/pools"},
                {"method":"GET","path":"/admin/stats/upstream"},
                {"method":"GET","path":"/admin/pools"},
                {"method":"GET","path":"/admin/pools/{name}"},
                {"method":"GET","path":"/admin/pool/state"},
                {"method":"GET,POST","path":"/admin/fuse"},
                {"method":"GET","path":"/admin/requests"},
                {"method":"GET","path":"/admin/requests/recent"},
                {"method":"GET","path":"/admin/requests/summary"},
                {"method":"GET","path":"/admin/requests/timings"},
                {"method":"GET","path":"/admin/requests/models"},
                {"method":"GET","path":"/admin/requests/nodes"},
                {"method":"GET","path":"/admin/requests/{rid}"},
                {"method":"GET","path":"/admin/requests/export"},
                {"method":"GET","path":"/admin/audit/summary"},
                {"method":"GET","path":"/admin/audit/requests"},
                {"method":"GET","path":"/admin/audit/requests/{rid}"},
                {"method":"GET","path":"/admin/audit/models"},
                {"method":"GET","path":"/admin/audit/nodes"},
                {"method":"GET","path":"/admin/audit/anomalies"},
                {"method":"GET","path":"/admin/audit/export"},
                {"method":"GET","path":"/admin/errors/summary"},
                {"method":"GET","path":"/admin/latency/summary"},
                {"method":"GET","path":"/admin/ttft/summary"},
                {"method":"GET","path":"/admin/protocol-guard/events"},
                {"method":"GET","path":"/admin/compactor/events"},
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
            "upstream_base": sanitize_text(&cfg.upstream_base),
            "allow_direct_fallback": cfg.allow_direct_fallback,
            "context_governance": {
                "request_body_limit_mb": cfg.request_body_limit_mb,
                "v1_max_concurrent_requests": cfg.v1_max_concurrent_requests,
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
            "protocol_guard": {
                "mode": cfg.protocol_guard_mode.to_string(),
                "orphan_policy": cfg.protocol_guard_orphan_policy.to_string(),
                "synthetic_ids": cfg.protocol_guard_synthetic_ids,
                "log_sample_rate": cfg.protocol_guard_log_sample_rate,
                "max_ms": cfg.protocol_guard_max_ms,
                "max_graph_messages": cfg.protocol_guard_max_graph_messages,
                "max_repair_actions": cfg.protocol_guard_max_repair_actions,
            },
            "v43_lanes": {
                "enabled": cfg.v43_lanes_enabled,
                "short_nonstream_concurrency": cfg.v43_short_nonstream_concurrency,
                "stream_concurrency": cfg.v43_stream_concurrency,
                "large_context_concurrency": cfg.v43_large_context_concurrency,
                "huge_context_concurrency": cfg.v43_huge_context_concurrency,
                "large_context_body_mb": cfg.v43_large_context_body_mb,
                "huge_context_body_mb": cfg.v43_huge_context_body_mb,
                "wait_timeout_ms": cfg.v43_lane_wait_timeout_ms,
                "async_collector_enabled": cfg.v43_async_collector_enabled,
                "collector_queue_capacity": cfg.v43_collector_queue_capacity,
                "dispatch_shards": cfg.v43_dispatch_shards,
                "node_min_concurrency": cfg.v43_node_min_concurrency,
                "node_max_concurrency": cfg.v43_node_max_concurrency,
                "aimd_success_step": cfg.v43_aimd_success_step,
                "aimd_failure_percent": cfg.v43_aimd_failure_percent,
                "aimd_slow_latency_ms": cfg.v43_aimd_slow_latency_ms,
                "global_budget_mode": cfg.v43_global_budget_mode.to_string(),
                "global_budget_fail_open": cfg.v43_global_budget_fail_open,
                "runtime": state.lanes.snapshot(),
            },
            "global_budget": cfg.global_budget_redis_url.as_ref().map(|_| json!({
                "configured": true,
                "instance_id": cfg.instance_id,
                "window_secs": cfg.node_budget_window_secs,
                "lease_ttl_secs": cfg.node_lease_ttl_secs,
                "mode": cfg.v43_global_budget_mode.to_string(),
                "fail_open": cfg.v43_global_budget_fail_open,
            })).unwrap_or_else(|| json!({"configured": false})),
            "data_plane": state.pool_manager.runtime_details(),
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
        let v4_model_registry_active = {
            let cfg = state.config.read().unwrap();
            cfg.v4_model_registry_active()
        };
        let (discovery, public_mode) = {
            let cfg = state.config.read().unwrap();
            (
                state.dynamic_models.snapshot(),
                cfg.dynamic_model_public_mode,
            )
        };
        if v4_model_registry_active {
            let registry = EffectiveModelRegistry::new(public_mode, discovery.clone());
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
            Self::ok_response(json!({
                "mode": "v4",
                "models": models,
                "dynamic_discovery": discovery,
                "safety": {
                    "dynamic_model_public_mode": public_mode.to_string(),
                    "candidates_are_public": false,
                    "auto_promote": false,
                    "public_models_source": "effective_registry"
                }
            }))
        } else {
            Self::ok_response(json!({
                "mode": "legacy",
                "models": [
                    {"id":"deepseek-v4-flash","upstream_id":"deepseek-v4-flash"},
                    {"id":"deepseek-v4-pro","upstream_id":"deepseek-v4-pro"}
                ],
                "dynamic_discovery": discovery,
                "safety": {
                    "dynamic_model_public_mode": public_mode.to_string(),
                    "candidates_are_public": false,
                    "auto_promote": false,
                    "public_models_source": "legacy_static"
                }
            }))
        }
    }
    pub fn model_detail(state: &AppState, model_id: &str) -> Response {
        let v4_model_registry_active = {
            let cfg = state.config.read().unwrap();
            cfg.v4_model_registry_active()
        };
        if v4_model_registry_active {
            let (public_mode, discovery) = {
                let cfg = state.config.read().unwrap();
                (
                    cfg.dynamic_model_public_mode,
                    state.dynamic_models.snapshot(),
                )
            };
            let registry = EffectiveModelRegistry::new(public_mode, discovery);
            match registry.resolve(model_id) {
                Ok(resolved) => Self::ok_response(json!({
                    "id": resolved.public_model,
                    "upstream_id": resolved.upstream_model,
                    "mode": "v4",
                    "endpoints": ["openai_chat_completions", "anthropic_messages"]
                })),
                Err(_) => match state.dynamic_models.get(model_id) {
                    Some(model) => Self::ok_response(json!({
                        "id": model.id,
                        "upstream_id": model.upstream_id,
                        "mode": "dynamic_candidate",
                        "state": model.state,
                        "reason": model.reason,
                        "probe_required": model.probe_required,
                        "auto_promoted": model.auto_promoted,
                        "public": false,
                        "routable": false
                    })),
                    None => Self::error_response(StatusCode::NOT_FOUND, "model not found"),
                },
            }
        } else {
            match model_id {
                "deepseek-v4-flash" | "deepseek-v4-pro" => Self::ok_response(json!({
                    "id": model_id,
                    "upstream_id": model_id,
                    "mode": "legacy"
                })),
                _ => match state.dynamic_models.get(model_id) {
                    Some(model) => Self::ok_response(json!({
                        "id": model.id,
                        "upstream_id": model.upstream_id,
                        "mode": "dynamic_candidate",
                        "state": model.state,
                        "reason": model.reason,
                        "probe_required": model.probe_required,
                        "auto_promoted": model.auto_promoted,
                        "public": false
                    })),
                    None => Self::error_response(StatusCode::NOT_FOUND, "model not found"),
                },
            }
        }
    }
    pub fn model_promote(state: &AppState, model_id: &str, target: Option<&str>) -> Response {
        let target_state = match target.unwrap_or("canary") {
            "canary" => DiscoveredModelState::Canary,
            "active" => DiscoveredModelState::Active,
            other => {
                return Self::error_response(
                    StatusCode::BAD_REQUEST,
                    format!("unsupported promotion target: {other}"),
                )
            }
        };
        match state.dynamic_models.set_model_state(
            model_id,
            target_state,
            format!("manual admin promotion to {}", target.unwrap_or("canary")),
        ) {
            Some(model) => Self::ok_response(json!({
                "id": model.id,
                "upstream_id": model.upstream_id,
                "state": model.state,
                "public": model.public,
                "routable": model.routable,
                "reason": model.reason
            })),
            None => Self::error_response(StatusCode::NOT_FOUND, "model not found"),
        }
    }
    pub fn model_demote(state: &AppState, model_id: &str) -> Response {
        match state.dynamic_models.set_model_state(
            model_id,
            DiscoveredModelState::Candidate,
            "manual admin demotion; probe required before exposure",
        ) {
            Some(model) => Self::ok_response(json!({
                "id": model.id,
                "upstream_id": model.upstream_id,
                "state": model.state,
                "public": model.public,
                "routable": model.routable,
                "reason": model.reason
            })),
            None => Self::error_response(StatusCode::NOT_FOUND, "model not found"),
        }
    }
    pub fn model_quarantine(state: &AppState, model_id: &str) -> Response {
        match state.dynamic_models.set_model_state(
            model_id,
            DiscoveredModelState::Quarantined,
            "manual admin quarantine",
        ) {
            Some(model) => Self::ok_response(json!({
                "id": model.id,
                "upstream_id": model.upstream_id,
                "state": model.state,
                "public": model.public,
                "routable": model.routable,
                "reason": model.reason
            })),
            None => Self::error_response(StatusCode::NOT_FOUND, "model not found"),
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
            limit: 1,
            ..Default::default()
        };
        let result = state.collector.query_requests(&filter);
        match result.items.into_iter().next() {
            Some(r) => Self::ok_response(json!(r)),
            None => {
                let audit = state.collector.query_audit_requests(&filter);
                match audit.items.into_iter().next() {
                    Some(r) => Self::ok_response(json!(r)),
                    None => Self::error_response(StatusCode::NOT_FOUND, "request not found"),
                }
            }
        }
    }
    pub fn requests_recent(state: &AppState) -> Response {
        let filter = RequestFilter {
            limit: 100,
            ..Default::default()
        };
        let audit = state.collector.query_audit_requests(&filter);
        if !audit.items.is_empty() {
            return Self::ok_response(json!(audit.items));
        }
        Self::ok_response(json!(state.collector.query_requests(&filter).items))
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
            "protocol_first_byte_ms",
            result
                .items
                .iter()
                .map(|r| r.timings.protocol_first_byte_ms)
                .sum(),
        );
        add_avg(
            "first_content_token_ms",
            result
                .items
                .iter()
                .map(|r| r.timings.first_content_token_ms)
                .sum(),
        );
        add_avg(
            "first_tool_call_ms",
            result
                .items
                .iter()
                .map(|r| r.timings.first_tool_call_ms)
                .sum(),
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

    pub fn errors_summary(state: &AppState, filter: &RequestFilter) -> Response {
        let audit_failures = state.collector.audit_failures(filter);
        let snapshot = state.collector.snapshot();
        Self::ok_response(json!({
            "snapshot": {
                "count_429": snapshot.requests.count_429,
                "count_4xx": snapshot.requests.count_4xx,
                "count_5xx": snapshot.requests.count_5xx,
                "count_timeout": snapshot.requests.count_timeout,
                "by_outcome": snapshot.requests.by_outcome,
                "by_failure_kind": snapshot.requests.by_failure_kind,
            },
            "durable": audit_failures,
        }))
    }

    pub fn latency_summary(state: &AppState, filter: &RequestFilter) -> Response {
        let summary = state.collector.audit_summary(filter);
        let top = state.collector.audit_top_requests(filter, "latency");
        let timeseries = state.collector.audit_timeseries(filter, 300_000);
        Self::ok_response(json!({
            "summary": summary,
            "top_latency_requests": top,
            "timeseries_5m": timeseries,
        }))
    }

    pub fn ttft_summary(state: &AppState, filter: &RequestFilter) -> Response {
        let summary = state.collector.audit_summary(filter);
        let top = state.collector.audit_top_requests(filter, "ttft");
        Self::ok_response(json!({
            "summary": summary,
            "top_ttft_requests": top,
        }))
    }

    pub fn protocol_guard_events(state: &AppState, filter: &RequestFilter) -> Response {
        let result = state.collector.query_audit_requests(filter);
        let mut counters = BTreeMap::<String, u64>::new();
        let mut recent = Vec::new();
        for item in result.items.into_iter() {
            let Some(guard) = item.protocol_guard else {
                continue;
            };
            *counters.entry("requests".into()).or_default() += 1;
            if guard.applied {
                *counters.entry("applied".into()).or_default() += 1;
            }
            if guard.pre_invalid {
                *counters.entry("pre_invalid".into()).or_default() += 1;
            }
            if guard.post_valid {
                *counters.entry("post_valid".into()).or_default() += 1;
            }
            add_counter(
                &mut counters,
                "missing_tool_call_id",
                guard.missing_tool_call_id_count as u64,
            );
            add_counter(
                &mut counters,
                "missing_tool_use_id",
                guard.missing_tool_use_id_count as u64,
            );
            add_counter(
                &mut counters,
                "synthetic_tool_id",
                guard.synthetic_tool_id_count as u64,
            );
            add_counter(
                &mut counters,
                "paired_tool_result",
                guard.paired_tool_result_count as u64,
            );
            add_counter(
                &mut counters,
                "orphan_tool_result",
                guard.orphan_tool_result_count as u64,
            );
            add_counter(
                &mut counters,
                "downgraded_tool_result",
                guard.downgraded_tool_result_count as u64,
            );
            add_counter(
                &mut counters,
                "orphan_assistant_call",
                guard.orphan_assistant_call_count as u64,
            );
            recent.push(json!({
                "rid": item.rid,
                "external_request_id": item.external_request_id,
                "ts": item.ts,
                "model": item.model,
                "status": item.status,
                "failure_kind": item.failure_kind,
                "guard": guard,
            }));
        }
        Self::ok_response(json!({
            "summary": counters,
            "recent": recent,
        }))
    }

    pub fn compactor_events(state: &AppState, filter: &RequestFilter) -> Response {
        let result = state.collector.query_audit_requests(filter);
        let mut counters = BTreeMap::<String, u64>::new();
        let mut recent = Vec::new();
        for item in result.items.into_iter() {
            let Some(context) = item.context else {
                continue;
            };
            if context.action == "pass" && !context.trimmed {
                continue;
            }
            *counters.entry("requests".into()).or_default() += 1;
            if context.trimmed {
                *counters.entry("trimmed".into()).or_default() += 1;
            }
            add_counter(&mut counters, "trimmed_bytes", context.trimmed_bytes);
            add_counter(
                &mut counters,
                "artifact_cache_hits",
                context.artifact_cache_hits as u64,
            );
            add_counter(
                &mut counters,
                "artifact_cache_writes",
                context.artifact_cache_writes as u64,
            );
            recent.push(json!({
                "rid": item.rid,
                "external_request_id": item.external_request_id,
                "ts": item.ts,
                "model": item.model,
                "status": item.status,
                "context": context,
            }));
        }
        Self::ok_response(json!({
            "summary": counters,
            "recent": recent,
        }))
    }

    pub fn pool_state(state: &AppState) -> Response {
        let p = state.pool_manager.pool_stats();
        Self::ok_response(json!({
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
            },
            "runtime": state.pool_manager.runtime_details(),
            "budget": state.pool_manager.budget_details(),
            "recent_events": state.collector.recent_events(100),
        }))
    }

    // --- Durable audit ---
    pub fn audit_summary(state: &AppState, filter: &RequestFilter) -> Response {
        Self::ok_response(state.collector.audit_summary(filter))
    }

    pub fn audit_requests(state: &AppState, filter: &RequestFilter) -> Response {
        let result = state.collector.query_audit_requests(filter);
        let items: Vec<Value> = result.items.iter().map(|r| json!(r)).collect();
        Self::ok_response(items)
    }

    pub fn audit_request_detail(state: &AppState, rid: &str) -> Response {
        let result = state.collector.query_audit_requests(&RequestFilter {
            rid: Some(rid.to_string()),
            limit: 1,
            ..Default::default()
        });
        match result.items.into_iter().next() {
            Some(item) => Self::ok_response(item),
            None => Self::error_response(StatusCode::NOT_FOUND, "audit request not found"),
        }
    }

    pub fn audit_models(state: &AppState, filter: &RequestFilter) -> Response {
        Self::ok_response(state.collector.audit_models(filter))
    }

    pub fn audit_nodes(state: &AppState, filter: &RequestFilter) -> Response {
        Self::ok_response(state.collector.audit_nodes(filter))
    }

    pub fn audit_anomalies(state: &AppState, filter: &RequestFilter) -> Response {
        Self::ok_response(state.collector.audit_anomalies(filter))
    }

    pub fn audit_export(state: &AppState, filter: &RequestFilter) -> Response {
        Response::builder()
            .header("content-type", "application/x-ndjson")
            .body(axum::body::Body::from(state.collector.audit_export(filter)))
            .unwrap()
    }

    pub fn audit_timeseries(state: &AppState, filter: &RequestFilter, bucket_ms: i64) -> Response {
        Self::ok_response(state.collector.audit_timeseries(filter, bucket_ms))
    }

    pub fn audit_top_requests(state: &AppState, filter: &RequestFilter, by: &str) -> Response {
        Self::ok_response(state.collector.audit_top_requests(filter, by))
    }

    pub fn audit_top_nodes(state: &AppState, filter: &RequestFilter, by: &str) -> Response {
        Self::ok_response(state.collector.audit_top_nodes(filter, by))
    }

    pub fn audit_failures(state: &AppState, filter: &RequestFilter) -> Response {
        Self::ok_response(state.collector.audit_failures(filter))
    }

    pub fn audit_node_detail(state: &AppState, filter: &RequestFilter, node_id: &str) -> Response {
        Self::ok_response(state.collector.audit_node_detail(filter, node_id))
    }

    pub fn audit_by_external_id(state: &AppState, external_id: &str, limit: usize) -> Response {
        Self::ok_response(state.collector.audit_by_external_id(external_id, limit))
    }

    pub fn audit_reconcile(state: &AppState, filter: &RequestFilter) -> Response {
        Self::ok_response(state.collector.audit_reconcile(filter))
    }

    pub fn audit_budget_history(
        state: &AppState,
        filter: &RequestFilter,
        bucket_ms: i64,
    ) -> Response {
        Self::ok_response(state.collector.audit_budget_history(filter, bucket_ms))
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
            "upstream_base": sanitize_text(&cfg.upstream_base),
            "pool_max_retries": cfg.pool_max_retries,
            "v4_retry_budget_ms": cfg.v4_retry_budget_ms,
            "connect_timeout_secs": cfg.connect_timeout_secs,
            "request_timeout_secs": cfg.request_timeout_secs,
            "probe_timeout_secs": cfg.probe_timeout_secs,
            "allow_direct_fallback": cfg.allow_direct_fallback,
            "pool_starvation_retry_after_secs": cfg.pool_starvation_retry_after_secs,
            "zen_provider_mode": cfg.zen_provider_mode.to_string(),
            "v4_model_registry_enabled": cfg.v4_model_registry_enabled,
            "dynamic_model_discovery": {
                "enabled": cfg.dynamic_model_discovery_enabled,
                "url": sanitize_text(&cfg.dynamic_model_discovery_url),
                "interval_secs": cfg.dynamic_model_discovery_interval_secs,
                "auto_promote": false,
            },
            "audit": {
                "enabled": cfg.audit_log_enabled,
                "log_dir": sanitize_text(&cfg.audit_log_dir),
            },
            "admin_api_key_configured": cfg.admin_api_key.is_some(),
            "proxy_api_key_configured": cfg.proxy_api_key.is_some(),
            "instance_id": cfg.instance_id,
            "global_budget_redis_configured": cfg.global_budget_redis_url.is_some(),
            "context_governance": {
                "request_body_limit_mb": cfg.request_body_limit_mb,
                "v1_max_concurrent_requests": cfg.v1_max_concurrent_requests,
                "compactor_mode": cfg.zen_compactor_mode.to_string(),
                "artifact_cache_mode": cfg.zen_artifact_cache_mode.to_string(),
                "artifact_cache_dir": sanitize_text(&cfg.artifact_cache_dir),
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
            "protocol_guard": {
                "mode": cfg.protocol_guard_mode.to_string(),
                "orphan_policy": cfg.protocol_guard_orphan_policy.to_string(),
                "synthetic_ids": cfg.protocol_guard_synthetic_ids,
                "log_sample_rate": cfg.protocol_guard_log_sample_rate,
                "max_ms": cfg.protocol_guard_max_ms,
                "max_graph_messages": cfg.protocol_guard_max_graph_messages,
                "max_repair_actions": cfg.protocol_guard_max_repair_actions,
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
        let cfg = crate::config::Config::from_env();
        state.dynamic_models.set_config(
            cfg.dynamic_model_discovery_enabled,
            cfg.dynamic_model_discovery_url.clone(),
        );
        *state.config.write().unwrap() = cfg;
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
        if cfg.dynamic_model_discovery_enabled {
            warnings.push("DYNAMIC_MODEL_DISCOVERY_ENABLED is startup-scoped in Phase 1; restart is required when enabling it from a previously disabled process".into());
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

fn parse_ts(value: &str) -> Option<i64> {
    let parsed = value.parse::<i64>().ok()?;
    if parsed < 10_000_000_000 {
        Some(parsed.saturating_mul(1000))
    } else {
        Some(parsed)
    }
}

fn add_counter(counters: &mut BTreeMap<String, u64>, key: &str, value: u64) {
    if value > 0 {
        *counters.entry(key.to_string()).or_default() += value;
    }
}
