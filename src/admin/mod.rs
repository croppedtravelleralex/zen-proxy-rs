pub mod service;

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::Response,
    routing::{delete, get, post},
    Json, Router,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use crate::collector::RequestFilter;
use crate::state::AppState;
use service::AdminService;

fn err(msg: &str) -> Response {
    AdminService::error_response(axum::http::StatusCode::UNAUTHORIZED, msg)
}

macro_rules! auth_h {
    ($name:ident, $body:expr) => {
        async fn $name(State(st): State<Arc<AppState>>, h: HeaderMap) -> Response {
            if AdminService::check_auth(&h, &st).is_err() {
                return err("unauthorized");
            }
            $body(&st)
        }
    };
    ($name:ident, $body:expr, $extra:ty) => {
        async fn $name(State(st): State<Arc<AppState>>, h: HeaderMap, e: $extra) -> Response {
            if AdminService::check_auth(&h, &st).is_err() {
                return err("unauthorized");
            }
            $body(&st, e)
        }
    };
}

auth_h!(health_h, AdminService::health);
auth_h!(health_live_h, |_| AdminService::health_live());
auth_h!(health_ready_h, AdminService::health_ready);
auth_h!(stats_h, AdminService::stats);
auth_h!(stats_models_h, AdminService::stats_models);
auth_h!(stats_nodes_h, AdminService::stats_nodes);
auth_h!(stats_pools_h, AdminService::stats_pools);
auth_h!(stats_upstream_h, AdminService::stats_upstream);
auth_h!(routes_h, AdminService::routes);
auth_h!(runtime_h, AdminService::runtime);
auth_h!(models_h, AdminService::models);
auth_h!(budget_h, AdminService::budget);
auth_h!(pools_h, AdminService::pools);
auth_h!(fuse_get_h, AdminService::fuse_status);
auth_h!(requests_recent_h, AdminService::requests_recent);
auth_h!(requests_summary_h, AdminService::requests_summary);
auth_h!(requests_models_h, AdminService::requests_models);
auth_h!(requests_nodes_h, AdminService::requests_nodes);
auth_h!(events_h, AdminService::events);
auth_h!(ledger_h, AdminService::ledger_summary);
auth_h!(ledger_models_h, AdminService::ledger_models);
auth_h!(ledger_keys_h, AdminService::ledger_keys);
auth_h!(ledger_streams_h, AdminService::ledger_streams);
auth_h!(config_h, AdminService::config);
auth_h!(config_validation_h, AdminService::config_validation);
auth_h!(config_reload_h, AdminService::config_reload);
auth_h!(sys_uptime_h, AdminService::system_uptime);
auth_h!(sys_info_h, AdminService::system_info);
auth_h!(events_recent_h, AdminService::events_recent);
auth_h!(events_probes_h, AdminService::events_probes);
auth_h!(nodes_list_h, AdminService::nodes);

async fn pool_by_name_h(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Path(n): Path<String>,
) -> Response {
    if AdminService::check_auth(&h, &st).is_err() {
        return err("unauthorized");
    }
    AdminService::pool_by_name(&st, &n)
}
async fn request_detail_h(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Path(rid): Path<String>,
) -> Response {
    if AdminService::check_auth(&h, &st).is_err() {
        return err("unauthorized");
    }
    AdminService::request_detail(&st, &rid)
}
async fn model_detail_h(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Path(model_id): Path<String>,
) -> Response {
    if AdminService::check_auth(&h, &st).is_err() {
        return err("unauthorized");
    }
    AdminService::model_detail(&st, &model_id)
}
async fn requests_h(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Query(p): Query<HashMap<String, String>>,
) -> Response {
    if AdminService::check_auth(&h, &st).is_err() {
        return err("unauthorized");
    }
    AdminService::requests_list(
        &st,
        &RequestFilter {
            model: p.get("model").cloned(),
            status: p.get("status").and_then(|v| v.parse().ok()),
            limit: p.get("limit").and_then(|v| v.parse().ok()).unwrap_or(100),
            ..Default::default()
        },
    )
}
async fn fuse_post_h(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Json(b): Json<Value>,
) -> Response {
    if AdminService::check_auth(&h, &st).is_err() {
        return err("unauthorized");
    }
    AdminService::fuse_set(
        &st,
        b.get("open").and_then(|v| v.as_bool()).unwrap_or(false),
    )
}
async fn node_add_h(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Json(b): Json<Value>,
) -> Response {
    if AdminService::check_auth(&h, &st).is_err() {
        return err("unauthorized");
    }
    let url = match b.get("url").and_then(|v| v.as_str()) {
        Some(u) => u,
        None => {
            return AdminService::error_response(axum::http::StatusCode::BAD_REQUEST, "missing url")
        }
    };
    AdminService::node_add(&st, url)
}
async fn node_delete_h(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Path(nid): Path<String>,
) -> Response {
    if AdminService::check_auth(&h, &st).is_err() {
        return err("unauthorized");
    }
    AdminService::node_delete(&st, &nid)
}
async fn node_probe_h(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Path(nid): Path<String>,
) -> Response {
    if AdminService::check_auth(&h, &st).is_err() {
        return err("unauthorized");
    }
    AdminService::node_probe(&st, &nid)
}
async fn node_recover_h(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Path(nid): Path<String>,
) -> Response {
    if AdminService::check_auth(&h, &st).is_err() {
        return err("unauthorized");
    }
    AdminService::node_recover(&st, &nid)
}
async fn probe_now_h(State(st): State<Arc<AppState>>, h: HeaderMap) -> Response {
    if AdminService::check_auth(&h, &st).is_err() {
        return err("unauthorized");
    }
    AdminService::probe_now(&st)
}
async fn requests_export_h(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Query(p): Query<HashMap<String, String>>,
) -> Response {
    if AdminService::check_auth(&h, &st).is_err() {
        return err("unauthorized");
    }
    let limit = p.get("limit").and_then(|v| v.parse().ok()).unwrap_or(10000);
    AdminService::requests_export(&st, limit)
}
async fn sys_log_level_h(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Path(level): Path<String>,
) -> Response {
    if AdminService::check_auth(&h, &st).is_err() {
        return err("unauthorized");
    }
    AdminService::system_log_level(&level)
}

pub fn admin_router() -> Router<Arc<AppState>> {
    let r = Router::new()
        .route("/admin/health", get(health_h))
        .route("/admin/health/live", get(health_live_h))
        .route("/admin/health/ready", get(health_ready_h))
        .route("/admin/routes", get(routes_h))
        .route("/admin/runtime", get(runtime_h))
        .route("/admin/models", get(models_h))
        .route("/admin/models/{model_id}", get(model_detail_h))
        .route("/admin/budget", get(budget_h))
        .route("/admin/stats", get(stats_h))
        .route("/admin/stats/models", get(stats_models_h))
        .route("/admin/stats/nodes", get(stats_nodes_h))
        .route("/admin/stats/pools", get(stats_pools_h))
        .route("/admin/stats/upstream", get(stats_upstream_h))
        .route("/admin/pools", get(pools_h))
        .route("/admin/pools/{name}", get(pool_by_name_h))
        .route("/admin/fuse", get(fuse_get_h).post(fuse_post_h))
        .route("/admin/requests", get(requests_h))
        .route("/admin/requests/recent", get(requests_recent_h))
        .route("/admin/requests/summary", get(requests_summary_h))
        .route("/admin/requests/models", get(requests_models_h))
        .route("/admin/requests/nodes", get(requests_nodes_h))
        .route("/admin/requests/{rid}", get(request_detail_h))
        .route("/admin/events", get(events_h))
        .route("/admin/ledger", get(ledger_h))
        .route("/admin/ledger/models", get(ledger_models_h))
        .route("/admin/ledger/keys", get(ledger_keys_h))
        .route("/admin/ledger/streams", get(ledger_streams_h))
        .route("/admin/config", get(config_h))
        .route("/admin/config/reload", post(config_reload_h))
        .route("/admin/requests/export", get(requests_export_h))
        .route("/admin/events/recent", get(events_recent_h))
        .route("/admin/events/probes", get(events_probes_h))
        .route("/admin/config/validation", get(config_validation_h))
        .route("/admin/system/uptime", get(sys_uptime_h))
        .route("/admin/system/info", get(sys_info_h))
        .route("/admin/system/log-level/{level}", post(sys_log_level_h))
        .route("/admin/probe/now", post(probe_now_h));

    // Static /admin/nodes and parameterized /admin/nodes/{node_id} in separate
    // routers to avoid matchit route conflict in axum 0.8.
    let nodes_static = Router::new().route("/admin/nodes", get(nodes_list_h).post(node_add_h));
    let nodes_param = Router::new()
        .route("/admin/nodes/{node_id}", delete(node_delete_h))
        .route("/admin/nodes/{node_id}/probe", post(node_probe_h))
        .route("/admin/nodes/{node_id}/recover", post(node_recover_h));

    r.merge(nodes_static).merge(nodes_param)
}
