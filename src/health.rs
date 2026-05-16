use std::sync::atomic::AtomicU64;
use std::sync::RwLock;
use std::collections::VecDeque;
use std::time::Instant;

pub struct UpstreamHealth {
    pub timestamps: RwLock<VecDeque<(Instant, u16)>>,
    pub backoff_until: AtomicU64,
    pub soft_backoff_until: AtomicU64,
    pub window_size: usize,
}
impl UpstreamHealth {
    pub fn new(window_size: usize) -> Self {
        Self {
            timestamps: RwLock::new(VecDeque::with_capacity(window_size)),
            backoff_until: AtomicU64::new(0),
            soft_backoff_until: AtomicU64::new(0),
            window_size,
        }
    }
    pub fn record(&self, _status: u16) {}
    pub fn is_backoff(&self) -> bool { false }
    pub fn stats(&self) -> UpstreamStats {
        UpstreamStats { backoff: false, rate_429: 0.0, total_requests: 0, success_rate: 100.0 }
    }
}
pub struct UpstreamStats { pub backoff: bool, pub rate_429: f64, pub total_requests: u64, pub success_rate: f64 }
pub struct ModelHealth;
impl ModelHealth { pub fn new() -> Self { Self } }
