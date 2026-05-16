use std::time::Instant;
use crate::config::Config;
use crate::token_bucket::TokenBucket;
use crate::selector::ProxySelector;
use crate::pool::SessionPool;
use crate::utils;

pub struct AppState {
    pub config: Config,
    pub token_bucket: TokenBucket,
    pub proxy_selector: ProxySelector,
    pub session_pool: SessionPool,
    pub startup_time: Instant,
    pub node_urls: Vec<String>,
}
