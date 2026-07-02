pub mod chat;
pub mod health;
pub mod models;

use crate::config::Config;
use axum::{routing::get, routing::post, Router};
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub http_client: reqwest::Client,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let http_client = reqwest::Client::builder()
            .no_proxy()
            .pool_max_idle_per_host(32)
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .timeout(config.timeout)
            .build()
            .expect("Failed to build HTTP client");
        Self {
            config,
            http_client,
        }
    }
}

pub fn create_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health::health_handler))
        .route("/v1/models", get(models::models_handler))
        .route("/models", get(models::models_handler))
        .route("/v1/chat/completions", post(chat::chat_handler))
        .route("/chat/completions", post(chat::chat_handler))
        .route("/v1/messages", post(chat::messages_handler))
        .route("/messages", post(chat::messages_handler))
        .with_state(Arc::new(state))
}
