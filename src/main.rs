#![allow(dead_code)]
mod admin;
mod bandwidth;
mod config;
mod health;
mod metrics;
mod node_db;
mod node_probe;
mod pool;
mod proxy;
mod selector;
mod state;
mod token_bucket;
mod utils;

use std::sync::Arc;
use std::time::Instant;
use axum::{
    Router,
    routing::{get, any},
    response::Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use serde_json::{json, Value};
use tower_http::cors::CorsLayer;
use tracing_subscriber::EnvFilter;
use state::AppState;

async fn health_handler(State(st): State<Arc<AppState>>) -> Json<Value> {
    let uptime = st.startup_time.elapsed().as_secs();
    let stats = st.proxy_selector.stats();
    let backoff = st.upstream_health.is_backoff();
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": uptime,
        "pid": std::process::id(),
        "nodes": {
            "total": stats.total_nodes,
            "available": stats.available_nodes,
            "blacklisted": stats.blacklisted_nodes
        },
        "upstream": {
            "backoff": backoff
        }
    }))
}

async fn index_handler() -> Json<Value> {
    Json(json!({"service": "zen-proxy-rs", "status": "ok"}))
}

async fn metrics_handler(State(st): State<Arc<AppState>>) -> String {
    st.metrics.encode()
}

async fn models_handler() -> Json<Value> {
    Json(json!({
        "object": "list",
        "data": [
            {"id": "deepseek-v4-flash", "object": "model"},
            {"id": "deepseek-v4-pro", "object": "model"}
        ]
    }))
}

fn check_admin_auth(headers: &HeaderMap, state: &AppState) -> bool {
    if state.admin.api_key.is_empty() {
        return true;
    }
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map_or(false, |key| key == state.admin.api_key)
}

async fn admin_models_handler(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    if !check_admin_auth(&headers, &st) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let models = st.model_health.get_all();
    Ok(Json(json!({"models": models, "status": "ok"})))
}

async fn admin_pools_handler(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    if !check_admin_auth(&headers, &st) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let stats_value = st.proxy_selector.pool_stats_json();
    Ok(Json(json!({"pools": stats_value, "status": "ok"})))
}

async fn admin_stats_handler(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    if !check_admin_auth(&headers, &st) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let stats_value = admin::build_admin_stats(&st.node_db, &st.metrics, &st.bandwidth, &st.upstream_health, Some(&st.proxy_selector));
    Ok(Json(json!({"stats": stats_value, "status": "ok"})))
}

async fn admin_events_handler(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    if !check_admin_auth(&headers, &st) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let events: Vec<serde_json::Value> = Vec::new();
    Ok(Json(json!({"events": events, "status": "ok"})))
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.expect("Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => { tracing::info!("received Ctrl+C, shutting down"); }
        _ = terminate => { tracing::info!("received SIGTERM, shutting down"); }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .init();

    let config = config::Config::from_env();
    let mode = config.parse_token_mode();
    let token_bucket = token_bucket::TokenBucket::new(
        config.token_rate, config.token_burst, mode,
        config.adaptive_min_rate, config.adaptive_max_rate, config.adaptive_window,
    );
    let node_urls = config.load_nodes();
    tracing::info!(count = node_urls.len(), "loaded proxy nodes");
    let proxy_selector = selector::ProxySelector::new(
        node_urls.clone(),
        config.proxy_error_threshold,
        config.proxy_cooldown_seconds,
        config.proxy_recovery_interval,
    );
    let session_pool = pool::SessionPool::new(
        config.pool_max_size,
        config.request_timeout_secs,
        config.connect_timeout_secs,
    );

    let upstream_health = Arc::new(health::UpstreamHealth::new(1000));
    let model_health = Arc::new(health::ModelHealth::new());
    let metrics = Arc::new(metrics::Metrics::new());

    let node_db_path = std::env::var("NODE_DB_PATH").unwrap_or_else(|_| "/tmp/zen-proxy-node-db.json".into());
    let ip_stats_path = std::env::var("IP_STATS_PATH").unwrap_or_else(|_| "/tmp/zen-proxy-ip-stats.json".into());
    let node_db = Arc::new(node_db::NodeDB::new(&node_db_path, &ip_stats_path));

    let ip_stats_tracker = Arc::new(node_db::IPStatsTracker::new());
    let bandwidth = Arc::new(bandwidth::BandwidthCollector::new());
    let admin = Arc::new(admin::AdminState::new());

    let addr = config.bind_addr();
    let app_state = Arc::new(AppState {
        config,
        token_bucket,
        proxy_selector,
        session_pool,
        startup_time: Instant::now(),
        node_urls,
        upstream_health,
        model_health,
        metrics,
        node_db: node_db.clone(),
        ip_stats_tracker,
        bandwidth,
        admin,
    });

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/health", get(health_handler))
        .route("/metrics", get(metrics_handler))
        .route("/v1/models", get(models_handler))
        .route("/v1/{*path}", any(proxy::proxy_handler))
        .route("/admin/models", get(admin_models_handler))
        .route("/admin/pools", get(admin_pools_handler))
        .route("/admin/stats", get(admin_stats_handler))
        .route("/admin/events", get(admin_events_handler))
        .layer(CorsLayer::permissive())
        .with_state(app_state);

    // Background: persist node_db every 60s
    {
        let node_db = node_db.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                node_db.persist();
            }
        });
    }

    // Background: purge stale nodes every 300s
    {
        let node_db = node_db.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
                node_db.purge_stale(300);
            }
        });
    }

    // SIGHUP hot-reload: re-read config env vars without restart
    {
        tokio::spawn(async move {
            #[cfg(unix)]
            {
                let Ok(mut stream) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) else {
                    tracing::error!("failed to install SIGHUP handler");
                    return;
                };
                loop {
                    stream.recv().await;
                    tracing::info!("SIGHUP received, reloading config from env");
                    let new_config = config::Config::from_env();
                    tracing::info!(
                        "config reloaded: port={}, log_level={}",
                        new_config.port, new_config.log_level
                    );
                }
            }
            #[cfg(not(unix))]
            std::future::pending::<()>().await;
        });
    }

            tracing::info!("starting on {}", addr);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await.unwrap();
}
