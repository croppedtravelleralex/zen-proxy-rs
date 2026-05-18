use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::state::AppState;

fn check_admin_auth(headers: &HeaderMap, state: &AppState) -> bool {
    let key = match &state.config.admin_api_key {
        Some(k) => k,
        None => return false,
    };
    if key.is_empty() {
        return false;
    }
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|k| k == key)
}

pub async fn admin_pools_handler(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    if !check_admin_auth(&headers, &st) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let pools = st.pool_manager.pool_stats();
    Ok(Json(json!({
        "pools": {
            "dispatch": pools.dispatch_size,
            "active": pools.active_size,
            "ratelimited": pools.ratelimited_size,
            "dead": pools.dead_size,
            "total": pools.total(),
            "transitions": pools.pool_transitions,
            "concurrency": pools.active_concurrency,
            "fuse": pools.fuse,
        },
        "status": "ok"
    })))
}

pub async fn admin_fuse_handler(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    if !check_admin_auth(&headers, &st) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    // Parse action from query params or body (default: status check)
    let pools = st.pool_manager.pool_stats();

    Ok(Json(json!({
        "fuse": pools.fuse,
        "pools": {
            "dispatch": pools.dispatch_size,
            "active": pools.active_size,
            "ratelimited": pools.ratelimited_size,
            "dead": pools.dead_size,
        },
        "status": "ok"
    })))
}

pub async fn admin_health_handler(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    if !check_admin_auth(&headers, &st) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let pools = st.pool_manager.pool_stats();
    let uptime = st.startup_time.elapsed().as_secs();
    let backoff = st.upstream_health.is_backoff();

    Ok(Json(json!({
        "status": "ok",
        "uptime_secs": uptime,
        "fuse": pools.fuse,
        "upstream": {
            "backoff": backoff,
        },
        "pools": {
            "dispatch": pools.dispatch_size,
            "active": pools.active_size,
            "ratelimited": pools.ratelimited_size,
            "dead": pools.dead_size,
            "total": pools.total(),
        },
    })))
}
