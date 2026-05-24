#![allow(dead_code)]

mod admin;
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
mod v4;

use std::sync::{Arc, OnceLock, RwLock};
use std::time::Instant;

use axum::{
    extract::State,
    response::Json,
    routing::{any, get},
    Router,
};
use serde_json::{json, Value};
use tower_http::cors::CorsLayer;
use tracing_subscriber::{prelude::*, reload, EnvFilter, Registry};

use collector::default::DefaultCollector;
use collector::export::JsonBackend;
use collector::DataCollector;
use pool::active::ActivePool;
use pool::dead::DeadPoolImpl;
use pool::dispatch::{DispatchPool, NodeBudgetLimits};
use pool::global_budget::{GlobalBudgetConfig, GlobalBudgetRegistry};
use pool::manager::PoolManagerImpl;
use pool::ratelimited::RateLimitedPoolImpl;
use pool::{DeadPool, NodeRef, Pool, RateLimitedPool};
use provider::webshare::WebShareProvider;
use state::AppState;
use v4::model::ModelRegistry;

static LOG_RELOAD: OnceLock<reload::Handle<EnvFilter, Registry>> = OnceLock::new();

pub(crate) fn set_log_level(level: &str) -> Result<(), &'static str> {
    let handle = LOG_RELOAD.get().ok_or("log reload not initialized")?;
    let new_filter = match level.to_lowercase().as_str() {
        "off" => EnvFilter::new("off"),
        "error" => EnvFilter::new("error"),
        "warn" => EnvFilter::new("warn"),
        "info" => EnvFilter::new("info"),
        "debug" => EnvFilter::new("debug"),
        "trace" => EnvFilter::new("trace"),
        _ => return Err("invalid log level, use: off/error/warn/info/debug/trace"),
    };
    handle
        .modify(|f| *f = new_filter)
        .map_err(|_| "reload failed")
}

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

async fn models_handler(State(st): State<Arc<AppState>>) -> Json<Value> {
    let cfg = st.config.read().unwrap();
    let data = if cfg.v4_model_registry_active() {
        let registry = v4::model::StaticModelRegistry;
        registry
            .public_models()
            .into_iter()
            .map(|model| json!({"id": model.id, "object": "model", "owned_by": "deepseek"}))
            .collect::<Vec<_>>()
    } else {
        vec![
            json!({"id": "deepseek-v4-flash", "object": "model", "owned_by": "deepseek"}),
            json!({"id": "deepseek-v4-pro", "object": "model", "owned_by": "deepseek"}),
        ]
    };
    Json(json!({
        "object": "list",
        "data": data
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
    let log_filter = EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into());
    let (log_filter, log_handle) = reload::Layer::new(log_filter);
    LOG_RELOAD.set(log_handle).ok();
    tracing_subscriber::registry()
        .with(log_filter)
        .with(tracing_subscriber::fmt::Layer::new())
        .init();

    let config = config::Config::from_env();
    let node_urls = config.load_nodes();

    tracing::info!(count = node_urls.len(), "loaded proxy nodes");

    let _provider = Arc::new(WebShareProvider::new(node_urls.clone()));
    let mut dispatch = DispatchPool::new_with_limits(NodeBudgetLimits {
        max_calls_per_window: config.node_max_calls_per_window,
        max_tokens_per_window: config.node_max_tokens_per_window,
        max_kb_per_window: config.node_max_kb_per_window,
        cooldown_secs: config.node_budget_cooldown_secs,
    });
    if let Some(redis_url) = config.global_budget_redis_url.clone() {
        match GlobalBudgetRegistry::new(GlobalBudgetConfig {
            redis_url,
            instance_id: config.instance_id.clone(),
            window_secs: config.node_budget_window_secs,
            lease_ttl_secs: config.node_lease_ttl_secs,
            max_calls_per_window: config.node_max_calls_per_window,
            max_tokens_per_window: config.node_max_tokens_per_window,
            max_kb_per_window: config.node_max_kb_per_window,
            max_concurrent: 5,
            cooldown_secs: config.node_budget_cooldown_secs,
        }) {
            Ok(registry) => {
                tracing::info!(instance_id = %config.instance_id, "global Redis budget registry enabled");
                dispatch = dispatch.with_global_budget(registry);
            }
            Err(err) => {
                tracing::warn!(error = %err, "global Redis budget registry unavailable; using local budgets");
            }
        }
    }
    let active = Arc::new(ActivePool::new());
    let ratelimited = Arc::new(RateLimitedPoolImpl::new());
    let dead = Arc::new(DeadPoolImpl::new());

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
        Arc::new(dispatch),
        active.clone(),
        ratelimited.clone(),
        dead.clone(),
        collector.clone(),
        config.upstream_base.clone(),
        config.upstream_api_key.clone(),
        config.probe_timeout_secs,
        config.allow_direct_fallback,
    ));

    let upstream_health = Arc::new(health::UpstreamHealth::new(1000));

    let ledger = ledger::LedgerCounters::new();
    ledger.set_events_path(Some(config.ledger_events_path.clone()));

    let app_state = Arc::new(AppState {
        config: RwLock::new(config.clone()),
        pool_manager,
        dead_pool: dead.clone() as Arc<dyn DeadPool>,
        ratelimited_pool: ratelimited.clone() as Arc<dyn RateLimitedPool>,
        active_pool: active.clone() as Arc<dyn Pool>,
        collector,
        upstream_health,
        ledger,
        startup_time: Instant::now(),
    });

    // Background: snapshot persist every 60s
    {
        let state = app_state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                state.collector.persist();
            }
        });
    }

    // Background: low-frequency adaptive Dead-pool recovery.
    {
        let state = app_state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(60 * 60)).await;
                state.pool_manager.probe_dead_adaptive();
            }
        });
    }

    // SIGHUP hot-reload
    {
        let state = app_state.clone();
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
                    *state.config.write().unwrap() = config::Config::from_env();
                }
            }
            #[cfg(not(unix))]
            std::future::pending::<()>().await;
        });
    }

    let app = Router::new()
        .merge(admin::admin_router())
        .route("/", get(index_handler))
        .route("/health", get(health_handler))
        .route("/metrics", get(metrics_handler))
        .route("/models", get(models_handler))
        .route("/v1/models", get(models_handler))
        .route("/v1/{*path}", any(proxy::proxy_handler))
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
