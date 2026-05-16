use std::sync::atomic::{AtomicU64, Ordering};
use std::collections::VecDeque;
use std::sync::RwLock;
use std::time::Instant;

pub struct Metrics {
    pub total_requests: AtomicU64,
    pub success_count: AtomicU64,
    pub error_count: AtomicU64,
    pub count_429: AtomicU64,
    pub bytes_received: AtomicU64,
    rpm_window: RwLock<VecDeque<Instant>>,
    latency_buckets: RwLock<[u64; 16]>,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            total_requests: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            error_count: AtomicU64::new(0),
            count_429: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            rpm_window: RwLock::new(VecDeque::with_capacity(4096)),
            latency_buckets: RwLock::new([0u64; 16]),
        }
    }

    pub fn record_request(&self, _latency_ms: u64, bytes: u64, status: u16, is_error: bool) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        if is_error { self.error_count.fetch_add(1, Ordering::Relaxed); }
        else { self.success_count.fetch_add(1, Ordering::Relaxed); }
        if status == 429 { self.count_429.fetch_add(1, Ordering::Relaxed); }
        self.bytes_received.fetch_add(bytes, Ordering::Relaxed);
        if let Ok(mut win) = self.rpm_window.write() {
            win.push_back(Instant::now());
            if win.len() > 4096 { win.pop_front(); }
        }
    }

    pub fn rpm(&self) -> u64 {
        if let Ok(win) = self.rpm_window.read() {
            if win.len() < 2 { return 0; }
            let now = Instant::now();
            let oldest = win.front().copied().unwrap_or(now);
            let d = now.duration_since(oldest).as_secs_f64().max(0.001);
            (win.len() as f64 / d * 60.0) as u64
        } else { 0 }
    }

    pub fn encode(&self) -> String {
        let t = self.total_requests.load(Ordering::Relaxed);
        let ok = self.success_count.load(Ordering::Relaxed);
        let err = self.error_count.load(Ordering::Relaxed);
        let c429 = self.count_429.load(Ordering::Relaxed);
        let bytes = self.bytes_received.load(Ordering::Relaxed);
        let rpm = self.rpm();
        format!(
            "# HELP zen_proxy_requests_total Total proxy requests\n# TYPE zen_proxy_requests_total counter\nzen_proxy_requests_total {t}\n             # HELP zen_proxy_requests_ok Successful proxy requests\n# TYPE zen_proxy_requests_ok counter\nzen_proxy_requests_ok {ok}\n             # HELP zen_proxy_requests_error Failed proxy requests\n# TYPE zen_proxy_requests_error counter\nzen_proxy_requests_error {err}\n             # HELP zen_proxy_requests_429 Rate-limited (429) requests\n# TYPE zen_proxy_requests_429 counter\nzen_proxy_requests_429 {c429}\n             # HELP zen_proxy_bytes_received Total response bytes received\n# TYPE zen_proxy_bytes_received counter\nzen_proxy_bytes_received {bytes}\n             # HELP zen_proxy_rpm Requests per minute\n# TYPE zen_proxy_rpm gauge\nzen_proxy_rpm {rpm}\n"
        )
    }
}
