#![allow(dead_code)]
mod auth;
mod config;
mod error;
mod protocol;
mod proxy;
mod routes;
mod synthesis;
mod zen;

use axum::http::StatusCode;
use config::Config;
use routes::{create_router, AppState};
use std::time::Duration;
use tower_http::cors::{Any, CorsLayer};
use tower_http::timeout::TimeoutLayer;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = Config::from_env();
    let bind_addr = format!("{}:{}", config.host, config.port);
    let state = AppState::new(config.clone());

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers(Any);

    let app = create_router(state)
        .layer(cors)
        .layer(TimeoutLayer::with_status_code(
            StatusCode::GATEWAY_TIMEOUT,
            Duration::from_secs(130),
        ));

    tracing::info!(
        "free-model-client-rs http://{} kernel=direct-zen zen={}",
        bind_addr,
        config.zen_chat_url
    );

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .expect("Failed to bind");
    axum::serve(listener, app).await.expect("Server error");
}
