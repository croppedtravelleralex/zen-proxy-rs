use crate::collector::DataCollector;
use crate::config::Config;
use crate::health::UpstreamHealth;
use crate::ledger::LedgerCounters;

use crate::pool::{DeadPool, Pool, PoolManager, RateLimitedPool};
use std::sync::{Arc, RwLock};
use std::time::Instant;

pub struct AppState {
    pub config: RwLock<Config>,
    pub pool_manager: Arc<dyn PoolManager>,
    pub collector: Arc<dyn DataCollector>,
    pub upstream_health: Arc<UpstreamHealth>,
    pub ledger: LedgerCounters,
    pub startup_time: Instant,
    pub dead_pool: Arc<dyn DeadPool>,
    pub ratelimited_pool: Arc<dyn RateLimitedPool>,
    pub active_pool: Arc<dyn Pool>,
}
