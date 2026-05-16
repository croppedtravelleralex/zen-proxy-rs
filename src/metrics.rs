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
    pub count_4xx: AtomicU64,
    pub count_5xx: AtomicU64,
    pub count_timeout: AtomicU64,
    pub node_errors: AtomicU64,
    pub node_successes: AtomicU64,
    pub node_blacklist_count: AtomicU64,
    pub bytes_sent: AtomicU64,
    pub active_requests: AtomicU64,
    pub pool_hits: AtomicU64,
    pub pool_misses: AtomicU64,
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
            count_4xx: AtomicU64::new(0),
            count_5xx: AtomicU64::new(0),
            count_timeout: AtomicU64::new(0),
            node_errors: AtomicU64::new(0),
            node_successes: AtomicU64::new(0),
            node_blacklist_count: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            active_requests: AtomicU64::new(0),
            pool_hits: AtomicU64::new(0),
            pool_misses: AtomicU64::new(0),
        }
    }

    pub fn record_request(&self, _latency_ms: u64, bytes: u64, status: u16, is_error: bool) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        if is_error {
            self.error_count.fetch_add(1, Ordering::Relaxed);
        } else {
            self.success_count.fetch_add(1, Ordering::Relaxed);
        }
        if status == 429 {
            self.count_429.fetch_add(1, Ordering::Relaxed);
        } else if (400..500).contains(&status) {
            self.count_4xx.fetch_add(1, Ordering::Relaxed);
        } else if status >= 500 {
            self.count_5xx.fetch_add(1, Ordering::Relaxed);
        }
        self.bytes_received.fetch_add(bytes, Ordering::Relaxed);
        if let Ok(mut win) = self.rpm_window.write() {
            win.push_back(Instant::now());
            if win.len() > 4096 { win.pop_front(); }
        }
    }

    pub fn record_bytes_sent(&self, n: u64) {
        self.bytes_sent.fetch_add(n, Ordering::Relaxed);
    }

    pub fn record_active_request(&self, delta: i32) {
        if delta > 0 {
            self.active_requests.fetch_add(delta as u64, Ordering::Relaxed);
        } else {
            self.active_requests.fetch_sub(delta.unsigned_abs() as u64, Ordering::Relaxed);
        }
    }

    pub fn record_pool_hit(&self) { self.pool_hits.fetch_add(1, Ordering::Relaxed); }
    pub fn record_pool_miss(&self) { self.pool_misses.fetch_add(1, Ordering::Relaxed); }
    pub fn record_node_error(&self) { self.node_errors.fetch_add(1, Ordering::Relaxed); }
    pub fn record_node_success(&self) { self.node_successes.fetch_add(1, Ordering::Relaxed); }
    pub fn record_timeout(&self) { self.count_timeout.fetch_add(1, Ordering::Relaxed); }
    pub fn record_blacklist(&self) { self.node_blacklist_count.fetch_add(1, Ordering::Relaxed); }

    pub fn rpm(&self) -> u64 {
        if let Ok(win) = self.rpm_window.read() {
            if win.len() < 2 { return 0; }
            let now = Instant::now();
            let oldest = win.front().copied().unwrap_or(now);
            let d = now.duration_since(oldest).as_secs_f64().max(0.001);
            (win.len() as f64 / d * 60.0) as u64
        } else {
            0
        }
    }

    pub fn encode(&self) -> String {
        let req = self.total_requests.load(Ordering::Relaxed);
        let ok = self.success_count.load(Ordering::Relaxed);
        let err = self.error_count.load(Ordering::Relaxed);
        let c429 = self.count_429.load(Ordering::Relaxed);
        let c4xx = self.count_4xx.load(Ordering::Relaxed);
        let c5xx = self.count_5xx.load(Ordering::Relaxed);
        let bs = self.bytes_received.load(Ordering::Relaxed);
        let timeouts = self.count_timeout.load(Ordering::Relaxed);
        let active = self.active_requests.load(Ordering::Relaxed);
        let pool_h = self.pool_hits.load(Ordering::Relaxed);
        let pool_m = self.pool_misses.load(Ordering::Relaxed);
        let rpm = self.rpm();
        let n_err = self.node_errors.load(Ordering::Relaxed);
        let n_ok = self.node_successes.load(Ordering::Relaxed);
        let bl = self.node_blacklist_count.load(Ordering::Relaxed);
        let bs_sent = self.bytes_sent.load(Ordering::Relaxed);

        format!(
            "# HELP zen_proxy_requests_total Total proxy requests\n\
             # TYPE zen_proxy_requests_total counter\n\
             zen_proxy_requests_total {req}\n\
             # HELP zen_proxy_requests_ok Successful proxy requests\n\
             # TYPE zen_proxy_requests_ok counter\n\
             zen_proxy_requests_ok {ok}\n\
             # HELP zen_proxy_requests_error Failed proxy requests\n\
             # TYPE zen_proxy_requests_error counter\n\
             zen_proxy_requests_error {err}\n\
             # HELP zen_proxy_requests_429 Rate limited by upstream\n\
             # TYPE zen_proxy_requests_429 counter\n\
             zen_proxy_requests_429 {c429}\n\
             # HELP zen_proxy_requests_4xx Client error responses\n\
             # TYPE zen_proxy_requests_4xx counter\n\
             zen_proxy_requests_4xx {c4xx}\n\
             # HELP zen_proxy_requests_5xx Server error responses\n\
             # TYPE zen_proxy_requests_5xx counter\n\
             zen_proxy_requests_5xx {c5xx}\n\
             # HELP zen_proxy_timeouts Request timeouts\n\
             # TYPE zen_proxy_timeouts counter\n\
             zen_proxy_timeouts {timeouts}\n\
             # HELP zen_proxy_bytes_received Total bytes received\n\
             # TYPE zen_proxy_bytes_received counter\n\
             zen_proxy_bytes_received {bs}\n\
             # HELP zen_proxy_bytes_sent Total bytes sent\n\
             # TYPE zen_proxy_bytes_sent counter\n\
             zen_proxy_bytes_sent {bs_sent}\n\
             # HELP zen_proxy_rpm Requests per minute\n\
             # TYPE zen_proxy_rpm gauge\n\
             zen_proxy_rpm {rpm}\n\
             # HELP zen_proxy_active_requests Currently active requests\n\
             # TYPE zen_proxy_active_requests gauge\n\
             zen_proxy_active_requests {active}\n\
             # HELP zen_proxy_pool_hits Session pool cache hits\n\
             # TYPE zen_proxy_pool_hits counter\n\
             zen_proxy_pool_hits {pool_h}\n\
             # HELP zen_proxy_pool_misses Session pool cache misses\n\
             # TYPE zen_proxy_pool_misses counter\n\
             zen_proxy_pool_misses {pool_m}\n\
             # HELP zen_proxy_node_errors Proxy node-level errors\n\
             # TYPE zen_proxy_node_errors counter\n\
             zen_proxy_node_errors {n_err}\n\
             # HELP zen_proxy_node_successes Proxy node-level successes\n\
             # TYPE zen_proxy_node_successes counter\n\
             zen_proxy_node_successes {n_ok}\n\
             # HELP zen_proxy_node_blacklisted Nodes currently blacklisted\n\
             # TYPE zen_proxy_node_blacklisted gauge\n\
             zen_proxy_node_blacklisted {bl}\n")
    }
}
