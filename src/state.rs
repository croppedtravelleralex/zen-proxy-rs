use crate::collector::DataCollector;
use crate::config::Config;
use crate::health::UpstreamHealth;
use crate::lanes::LaneLimiter;
use crate::ledger::LedgerCounters;
use crate::v4::model_discovery::DynamicModelRegistry;

use crate::pool::{DeadPool, Pool, PoolManager, RateLimitedPool};
use std::sync::{Arc, RwLock};
use std::time::Instant;

pub struct AppState {
    pub config: RwLock<Config>,
    pub pool_manager: Arc<dyn PoolManager>,
    pub collector: Arc<dyn DataCollector>,
    pub upstream_health: Arc<UpstreamHealth>,
    pub lanes: Arc<LaneLimiter>,
    pub ledger: LedgerCounters,
    pub dynamic_models: Arc<DynamicModelRegistry>,
    pub startup_time: Instant,
    pub dead_pool: Arc<dyn DeadPool>,
    pub ratelimited_pool: Arc<dyn RateLimitedPool>,
    pub active_pool: Arc<dyn Pool>,
}
