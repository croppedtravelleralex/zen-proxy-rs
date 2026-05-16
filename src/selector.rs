use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Represents a snapshot of the proxy selector state.
#[derive(Debug, Clone)]
pub struct SelectorStats {
    pub total_nodes: usize,
    pub available_nodes: usize,
    pub blacklisted_nodes: usize,
    pub probed_ok: u64,
    pub probed_fail: u64,
    pub unprobed: usize,
    pub upstream_backoff: bool,
    pub upstream_429_rate: f64,
}

/// Selects proxy nodes from a pool, tracking health and blacklist state.
pub struct ProxySelector {
    total_nodes: usize,
    blacklisted_nodes: usize,
    probed_ok: AtomicU64,
    probed_fail: AtomicU64,
    unprobed: usize,
    upstream_backoff: bool,
    upstream_429_rate: f64,
    node_urls: Vec<String>,
    _error_threshold: u32,
    _cooldown_seconds: u64,
    _recovery_interval: u64,
    next_index: AtomicUsize,
}

impl ProxySelector {
    pub fn new(
        node_urls: Vec<String>,
        error_threshold: u32,
        cooldown_seconds: u64,
        recovery_interval: u64,
    ) -> Self {
        Self {
            total_nodes: node_urls.len(),
            blacklisted_nodes: 0,
            probed_ok: AtomicU64::new(0),
            probed_fail: AtomicU64::new(0),
            unprobed: node_urls.len(),
            upstream_backoff: false,
            upstream_429_rate: 0.0,
            node_urls,
            _error_threshold: error_threshold,
            _cooldown_seconds: cooldown_seconds,
            _recovery_interval: recovery_interval,
            next_index: AtomicUsize::new(0),
        }
    }

    pub fn next(&self) -> Option<&str> {
        if self.node_urls.is_empty() {
            return None;
        }
        let idx = self.next_index.fetch_add(1, Ordering::Relaxed) % self.node_urls.len();
        self.node_urls.get(idx).map(|s| s.as_str())
    }

    pub fn record_success(&self) {
        self.probed_ok.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_error(&self) {
        self.probed_fail.fetch_add(1, Ordering::Relaxed);
    }

    pub fn stats(&self) -> SelectorStats {
        SelectorStats {
            total_nodes: self.total_nodes,
            available_nodes: self.total_nodes.saturating_sub(self.blacklisted_nodes),
            blacklisted_nodes: self.blacklisted_nodes,
            probed_ok: self.probed_ok.load(Ordering::Relaxed),
            probed_fail: self.probed_fail.load(Ordering::Relaxed),
            unprobed: self.unprobed,
            upstream_backoff: self.upstream_backoff,
            upstream_429_rate: self.upstream_429_rate,
        }
    }

    pub fn total_nodes(&self) -> usize { self.total_nodes }
    pub fn available_nodes(&self) -> usize { self.total_nodes.saturating_sub(self.blacklisted_nodes) }
    pub fn blacklisted_nodes(&self) -> usize { self.blacklisted_nodes }
    pub fn probed_ok(&self) -> u64 { self.probed_ok.load(Ordering::Relaxed) }
    pub fn probed_fail(&self) -> u64 { self.probed_fail.load(Ordering::Relaxed) }
    pub fn unprobed(&self) -> usize { self.unprobed }
    pub fn upstream_backoff(&self) -> bool { self.upstream_backoff }
    pub fn upstream_429_rate(&self) -> f64 { self.upstream_429_rate }
}
