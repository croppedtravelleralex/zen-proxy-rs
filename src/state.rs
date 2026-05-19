use crate::collector::DataCollector;
use crate::config::Config;
use crate::health::UpstreamHealth;
use crate::ledger::LedgerCounters;

use crate::pool::PoolManager;
use std::sync::Arc;
use std::time::Instant;

pub struct AppState {
    pub config: Config,
    pub pool_manager: Arc<dyn PoolManager>,
    pub collector: Arc<dyn DataCollector>,
    pub upstream_health: Arc<UpstreamHealth>,
    pub ledger: LedgerCounters,
    pub startup_time: Instant,
}
