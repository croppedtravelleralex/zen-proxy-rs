pub mod chat;
pub mod health;
pub mod models;

use crate::config::{Config, ModelMapping};
use crate::model_catalog;
use axum::{routing::get, routing::post, Router};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub http_client: reqwest::Client,
    model_catalog_cache: Arc<RwLock<Option<CachedModelCatalog>>>,
}

#[derive(Clone)]
struct CachedModelCatalog {
    expires_at: Instant,
    mappings: Vec<ModelMapping>,
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
            model_catalog_cache: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn model_mappings(&self) -> Vec<ModelMapping> {
        if !self.config.auto_discover_models {
            return self.config.model_mappings.clone();
        }

        let now = Instant::now();
        let cached = self.model_catalog_cache.read().await.clone();
        if let Some(cached) = cached.clone().filter(|entry| entry.expires_at > now) {
            return cached.mappings;
        }

        match model_catalog::discover_model_mappings(&self.http_client, &self.config).await {
            Ok(discovered) => {
                let mappings =
                    model_catalog::merge_model_mappings(&self.config.model_mappings, &discovered);
                *self.model_catalog_cache.write().await = Some(CachedModelCatalog {
                    expires_at: Instant::now() + self.config.model_discovery_cache_ttl,
                    mappings: mappings.clone(),
                });
                mappings
            }
            Err(error) => {
                if let Some(cached) = cached {
                    tracing::warn!(
                        error = %error,
                        "failed to refresh Zen model catalog; using stale cached mappings"
                    );
                    cached.mappings
                } else {
                    tracing::warn!(
                        error = %error,
                        "failed to discover Zen model catalog; using static model mappings"
                    );
                    self.config.model_mappings.clone()
                }
            }
        }
    }

    pub async fn free_model_names(&self) -> Vec<String> {
        self.model_mappings()
            .await
            .into_iter()
            .map(|mapping| mapping.public_name)
            .collect()
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
