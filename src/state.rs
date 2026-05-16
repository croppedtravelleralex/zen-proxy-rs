use std::sync::Arc;
use std::time::Instant;
use crate::config::Config;
use crate::token_bucket::TokenBucket;
use crate::selector::ProxySelector;
use crate::pool::SessionPool;

pub struct AppState {
    pub config: Config,
    pub token_bucket: TokenBucket,
    pub proxy_selector: ProxySelector,
    pub session_pool: SessionPool,
    pub startup_time: Instant,
    pub node_urls: Vec<String>,
    pub upstream_health: Arc<crate::health::UpstreamHealth>,
    pub model_health: Arc<crate::health::ModelHealth>,
    pub metrics: Arc<crate::metrics::Metrics>,
    pub node_db: Arc<crate::node_db::NodeDB>,
    pub ip_stats_tracker: Arc<crate::node_db::IPStatsTracker>,
    pub bandwidth: Arc<crate::bandwidth::BandwidthCollector>,
    pub admin: Arc<crate::admin::AdminState>,
}
