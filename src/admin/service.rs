use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde::Serialize;
use serde_json::{json, Value};

use crate::collector::RequestFilter;

use crate::state::AppState;

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
            "probe_timeout_secs": cfg.probe_timeout_secs,
            "allow_direct_fallback": cfg.allow_direct_fallback,
            "pool_starvation_retry_after_secs": cfg.pool_starvation_retry_after_secs,
            "zen_provider_mode": cfg.zen_provider_mode.to_string(),
            "v4_model_registry_enabled": cfg.v4_model_registry_enabled,
            "admin_api_key_configured": cfg.admin_api_key.is_some(),
            "proxy_api_key_configured": cfg.proxy_api_key.is_some(),
            "node_budget": {
                "max_calls_per_window": cfg.node_max_calls_per_window,
                "max_tokens_per_window": cfg.node_max_tokens_per_window,
                "max_kb_per_window": cfg.node_max_kb_per_window,
                "cooldown_secs": cfg.node_budget_cooldown_secs,
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
