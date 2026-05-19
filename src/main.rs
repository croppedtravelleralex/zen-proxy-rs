#![allow(dead_code)]

mod collector;
mod config;
mod health;
mod ledger;
mod opencode_headers;
mod pool;
mod provider;
mod proxy;
mod server;
mod sse;
mod state;
mod utils;

use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::State,
    response::Json,
    routing::{any, get},
    Router,
};
use serde_json::{json, Value};
use tower_http::cors::CorsLayer;
use tracing_subscriber::EnvFilter;

use collector::default::DefaultCollector;
use collector::export::JsonBackend;
use collector::DataCollector;
use pool::active::ActivePool;
use pool::dead::DeadPoolImpl;
use pool::dispatch::DispatchPool;
use pool::manager::PoolManagerImpl;
use pool::ratelimited::RateLimitedPoolImpl;
use pool::{NodeRef, Pool};
use provider::webshare::WebShareProvider;
use state::AppState;

async fn health_handler(State(st): State<Arc<AppState>>) -> Json<Value> {
    let uptime = st.startup_time.elapsed().as_secs();
    let pools = st.pool_manager.pool_stats();
    let backoff = st.upstream_health.is_backoff();
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": uptime,
        "pid": std::process::id(),
        "pools": {
            "dispatch": pools.dispatch_size,
            "active": pools.active_size,
            "ratelimited": pools.ratelimited_size,
            "dead": pools.dead_size,
            "total": pools.total(),
            "fuse": pools.fuse,
        },
        "upstream": { "backoff": backoff }
    }))
}

async fn index_handler() -> Json<Value> {
    Json(json!({"service": "zen-proxy-rs", "version": "0.2.0", "status": "ok"}))
}

async fn metrics_handler(State(st): State<Arc<AppState>>) -> String {
    let snapshot = st.collector.snapshot();
    let backend = collector::export::PrometheusBackend;
    backend.encode(&snapshot)
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
    let node_urls = config.load_nodes();

    tracing::info!(count = node_urls.len(), "loaded proxy nodes");

    let _provider = Arc::new(WebShareProvider::new(node_urls.clone()));
    let dispatch = DispatchPool::new();
    let active = ActivePool::new();
    let ratelimited = RateLimitedPoolImpl::new();
    let dead = DeadPoolImpl::new();

    for url in &node_urls {
        dispatch.add(NodeRef::new(url.clone()));
    }
    tracing::info!(count = node_urls.len(), "nodes added to dispatch pool");

    let collector = Arc::new(DefaultCollector::new());
    {
        let json_backend = JsonBackend::new("/tmp/zen-proxy-snapshot.json");
        collector.set_backend(Box::new(json_backend));
    }

    let pool_manager = Arc::new(PoolManagerImpl::new(
        dispatch,
        active,
        ratelimited,
        dead,
        collector.clone(),
        config.upstream_base.clone(),
        config.probe_timeout_secs,
        config.allow_direct_fallback,
    ));

    let upstream_health = Arc::new(health::UpstreamHealth::new(1000));

    let app_state = Arc::new(AppState {
        config: config.clone(),
        pool_manager,
        collector,
        upstream_health,
        ledger: ledger::LedgerCounters::new(),
        startup_time: Instant::now(),
    });

    // Background: snapshot persist every 60s
    {
        let state = app_state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                let _snap = state.collector.snapshot();
            }
        });
    }

    // SIGHUP hot-reload
    {
        tokio::spawn(async move {
            #[cfg(unix)]
            {
                let Ok(mut stream) =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
                else {
                    tracing::error!("failed to install SIGHUP handler");
                    return;
                };
                loop {
                    stream.recv().await;
                    tracing::info!("SIGHUP received, reloading config from env");
                    let _new_config = config::Config::from_env();
                }
            }
            #[cfg(not(unix))]
            std::future::pending::<()>().await;
        });
    }

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/health", get(health_handler))
        .route("/metrics", get(metrics_handler))
        .route("/v1/models", get(models_handler))
        .route("/v1/{*path}", any(proxy::proxy_handler))
        .route("/admin/pools", get(server::admin_pools_handler))
        .route("/admin/fuse", get(server::admin_fuse_handler))
        .route("/admin/health", get(server::admin_health_handler))
        .route("/admin/nodes", get(server::admin_nodes_handler))
        .layer(CorsLayer::permissive())
        .with_state(app_state);

    let addr = config.bind_addr();
    tracing::info!("starting on {}", addr);

    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_reuseaddr(true).unwrap();
    socket
        .bind(addr.parse::<std::net::SocketAddr>().unwrap())
        .unwrap();
    let listener = socket.listen(1024).unwrap();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}
