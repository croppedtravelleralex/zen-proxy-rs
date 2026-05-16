use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const BACKOFF_BASE_MS: u64 = 1000;
const SOFT_BACKOFF_MS: u64 = 500;
const WINDOW_SECS: u64 = 60;

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

    pub fn record(&self, status: u16) {
        let now = now_ns();
        if let Ok(mut ts) = self.timestamps.write() {
            ts.push_back((Instant::now(), status));
            while ts.len() > self.window_size { ts.pop_front(); }
        }
        if status == 429 {
            let consecutive = self.consecutive_429();
            let backoff = BACKOFF_BASE_MS * (1u64 << (consecutive.min(10).saturating_sub(5)));
            let until = now + backoff * 1_000_000;
            self.backoff_until.store(until, Ordering::Release);
        }
    }

    pub fn is_backoff(&self) -> bool {
        let now = now_ns();
        let hard = self.backoff_until.load(Ordering::Acquire);
        if hard > 0 && now < hard { return true; }
        let soft = self.soft_backoff_until.load(Ordering::Acquire);
        if soft > 0 && now < soft { return true; }
        false
    }

    pub fn stats(&self) -> UpstreamStats {
        let (total, count_429) = self.window_stats();
        let rate = if total == 0 { 0.0 } else { count_429 as f64 / total as f64 };
        let success_rate = if total == 0 { 100.0 } else { (total - count_429) as f64 / total as f64 * 100.0 };
        UpstreamStats {
            backoff: self.is_backoff(),
            rate_429: rate,
            total_requests: total as u64,
            success_rate,
        }
    }

    fn window_stats(&self) -> (usize, usize) {
        if let Ok(ts) = self.timestamps.read() {
            let cutoff = Instant::now() - Duration::from_secs(WINDOW_SECS);
            let total = ts.iter().filter(|(t, _)| *t > cutoff).count();
            let c429 = ts.iter().filter(|(t, s)| *t > cutoff && *s == 429).count();
            (total, c429)
        } else { (0, 0) }
    }

    fn consecutive_429(&self) -> u64 {
        if let Ok(ts) = self.timestamps.read() {
            let mut count = 0u64;
            for (_, s) in ts.iter().rev() {
                if *s == 429 { count += 1; } else { break; }
            }
            count
        } else { 0 }
    }
}

pub struct UpstreamStats {
    pub backoff: bool,
    pub rate_429: f64,
    pub total_requests: u64,
    pub success_rate: f64,
}

pub struct ModelHealth {
    models: RwLock<std::collections::HashMap<String, bool>>,
}

impl ModelHealth {
    pub fn new() -> Self {
        Self { models: RwLock::new(std::collections::HashMap::new()) }
    }

    pub fn probe(&self, model_name: &str) -> bool {
        // Simplified: mark model as healthy
        if let Ok(mut m) = self.models.write() {
            m.insert(model_name.to_string(), true);
        }
        true
    }

    pub fn get_all(&self) -> std::collections::HashMap<String, bool> {
        self.models.read().map(|m| m.clone()).unwrap_or_default()
    }
}

fn now_ns() -> u64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let epoch = EPOCH.get_or_init(Instant::now);
    Instant::now().duration_since(*epoch).as_nanos() as u64
}
