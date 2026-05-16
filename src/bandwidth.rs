use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub struct BandwidthCollector {
    bytes_since_sample: AtomicU64,
    last_sample: std::sync::Mutex<Instant>,
    current_bps: std::sync::Mutex<f64>,
}

impl BandwidthCollector {
    pub fn new() -> Self {
        Self {
            bytes_since_sample: AtomicU64::new(0),
            last_sample: std::sync::Mutex::new(Instant::now()),
            current_bps: std::sync::Mutex::new(0.0),
        }
    }
    pub fn record_bytes(&self, n: u64) {
        self.bytes_since_sample.fetch_add(n, Ordering::Relaxed);
    }
    pub fn sample(&self) -> f64 {
        if let Ok(mut last) = self.last_sample.lock() {
            if let Ok(mut bps) = self.current_bps.lock() {
                let now = Instant::now();
                let elapsed = now.duration_since(*last).as_secs_f64().max(0.001);
                let bytes = self.bytes_since_sample.swap(0, Ordering::Relaxed);
                *bps = bytes as f64 / elapsed;
                *last = now;
                return *bps;
            }
        }
        0.0
    }
    pub fn bps(&self) -> f64 { *self.current_bps.lock().unwrap_or_else(|e| e.into_inner()) }
}
