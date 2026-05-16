use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

/// Compute a percentile from a deque of u64 samples.
fn percentile(data: &Mutex<VecDeque<u64>>, p: f64) -> f64 {
    let guard = match data.lock() {
        Ok(g) => g,
        Err(_) => return 0.0,
    };
    if guard.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<u64> = guard.iter().copied().collect();
    sorted.sort_unstable();
    let len = sorted.len();
    if len == 1 {
        return sorted[0] as f64;
    }
    let rank = (p / 100.0) * (len as f64 - 1.0);
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    if lower == upper {
        sorted[lower] as f64
    } else {
        let frac = rank - lower as f64;
        sorted[lower] as f64 + frac * (sorted[upper] - sorted[lower]) as f64
    }
}

/// Maximum samples retained for latency/error history.
const MAX_HISTORY: usize = 1024;

// ---------------------------------------------------------------------------
// ProxyNode
// ---------------------------------------------------------------------------

/// A single proxy node with atomic health tracking and per-node metrics.
pub struct ProxyNode {
    pub url: String,
    is_available: AtomicBool,
    blacklisted_until: AtomicU64,
    probed: AtomicBool,
    probed_ok: AtomicBool,
    pub total_requests: AtomicU64,
    pub total_errors: AtomicU64,
    pub total_bytes: AtomicU64,
    consecutive_errors: AtomicU64,
    consecutive_429: AtomicU64,
    pub total_429: AtomicU64,
    exit_ip: RwLock<Option<String>>,
    latency: Mutex<VecDeque<u64>>,
    errors: Mutex<VecDeque<u16>>,
}

impl ProxyNode {
    /// Create a new proxy node for the given URL. Initially available and unprobed.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            is_available: AtomicBool::new(true),
            blacklisted_until: AtomicU64::new(0),
            probed: AtomicBool::new(false),
            probed_ok: AtomicBool::new(false),
            total_requests: AtomicU64::new(0),
            total_errors: AtomicU64::new(0),
            total_bytes: AtomicU64::new(0),
            consecutive_errors: AtomicU64::new(0),
            consecutive_429: AtomicU64::new(0),
            total_429: AtomicU64::new(0),
            exit_ip: RwLock::new(None),
            latency: Mutex::new(VecDeque::with_capacity(MAX_HISTORY)),
            errors: Mutex::new(VecDeque::with_capacity(MAX_HISTORY)),
        }
    }

    /// Record a successful request.
    pub fn record_success(&self, bytes: u64, latency_us: u64) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        self.total_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.consecutive_errors.store(0, Ordering::Relaxed);
        self.consecutive_429.store(0, Ordering::Relaxed);
        if let Ok(mut l) = self.latency.lock() {
            l.push_back(latency_us);
            if l.len() > MAX_HISTORY {
                l.pop_front();
            }
        }
    }

    /// Record a failed request (non-429). Returns the current consecutive error count.
    pub fn record_error(&self, status: u16, latency_us: u64) -> u64 {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        self.total_errors.fetch_add(1, Ordering::Relaxed);
        let ce = self.consecutive_errors.fetch_add(1, Ordering::Relaxed) + 1;
        if let Ok(mut e) = self.errors.lock() {
            e.push_back(status);
            if e.len() > MAX_HISTORY {
                e.pop_front();
            }
        }
        if let Ok(mut l) = self.latency.lock() {
            l.push_back(latency_us);
            if l.len() > MAX_HISTORY {
                l.pop_front();
            }
        }
        ce
    }

    /// Set probed status.
    pub fn set_probed(&self, ok: bool) {
        self.probed.store(true, Ordering::Relaxed);
        self.probed_ok.store(ok, Ordering::Relaxed);
    }

    /// Set the exit IP discovered for this node.
    pub fn set_exit_ip(&self, ip: Option<String>) {
        if let Ok(mut guard) = self.exit_ip.write() {
            *guard = ip;
        }
    }

    /// Blacklist this node until `unix_seconds`.
    pub fn blacklist(&self, until_unix_secs: u64) {
        self.blacklisted_until.store(until_unix_secs, Ordering::Relaxed);
    }

    /// Clear the blacklist.
    pub fn clear_blacklist(&self) {
        self.blacklisted_until.store(0, Ordering::Relaxed);
    }

    /// Whether the node is currently blacklisted.
    pub fn is_blacklisted(&self, now_unix_secs: u64) -> bool {
        self.blacklisted_until.load(Ordering::Relaxed) > now_unix_secs
    }

    /// Whether the node is considered available (not blacklisted).
    pub fn is_available(&self, now_unix_secs: u64) -> bool {
        self.is_available.load(Ordering::Relaxed)
            && !self.is_blacklisted(now_unix_secs)
    }

    // -- Latency percentiles ------------------------------------------------

    pub fn latency_p50(&self) -> f64 { percentile(&self.latency, 50.0) }
    pub fn latency_p95(&self) -> f64 { percentile(&self.latency, 95.0) }
    pub fn latency_p99(&self) -> f64 { percentile(&self.latency, 99.0) }
    pub fn latency_avg(&self) -> f64 {
        let guard = match self.latency.lock() {
            Ok(g) => g,
            Err(_) => return 0.0,
        };
        if guard.is_empty() {
            return 0.0;
        }
        let sum: u64 = guard.iter().sum();
        sum as f64 / guard.len() as f64
    }

    // -- Derived stats ------------------------------------------------------

    /// Overall error rate (errors / requests, 0.0 if no requests).
    pub fn error_rate(&self) -> f64 {
        let reqs = self.total_requests.load(Ordering::Relaxed);
        if reqs == 0 {
            return 0.0;
        }
        self.total_errors.load(Ordering::Relaxed) as f64 / reqs as f64
    }

    pub fn consecutive_errors(&self) -> u64 {
        self.consecutive_errors.load(Ordering::Relaxed)
    }

    pub fn consecutive_429(&self) -> u64 {
        self.consecutive_429.load(Ordering::Relaxed)
    }

    pub fn total_429(&self) -> u64 {
        self.total_429.load(Ordering::Relaxed)
    }

    pub fn probed(&self) -> bool {
        self.probed.load(Ordering::Relaxed)
    }

    pub fn probed_ok(&self) -> bool {
        self.probed.load(Ordering::Relaxed) && self.probed_ok.load(Ordering::Relaxed)
    }

    pub fn exit_ip(&self) -> Option<String> {
        self.exit_ip.read().ok().and_then(|g| g.clone())
    }

    /// Record a 429 response.
    pub fn record_429(&self) {
        self.total_429.fetch_add(1, Ordering::Relaxed);
        self.consecutive_429.fetch_add(1, Ordering::Relaxed);
    }
}

// Manual Clone: atomics don't derive Clone.
impl Clone for ProxyNode {
    fn clone(&self) -> Self {
        Self {
            url: self.url.clone(),
            is_available: AtomicBool::new(self.is_available.load(Ordering::Relaxed)),
            blacklisted_until: AtomicU64::new(self.blacklisted_until.load(Ordering::Relaxed)),
            probed: AtomicBool::new(self.probed.load(Ordering::Relaxed)),
            probed_ok: AtomicBool::new(self.probed_ok.load(Ordering::Relaxed)),
            total_requests: AtomicU64::new(self.total_requests.load(Ordering::Relaxed)),
            total_errors: AtomicU64::new(self.total_errors.load(Ordering::Relaxed)),
            total_bytes: AtomicU64::new(self.total_bytes.load(Ordering::Relaxed)),
            consecutive_errors: AtomicU64::new(self.consecutive_errors.load(Ordering::Relaxed)),
            consecutive_429: AtomicU64::new(self.consecutive_429.load(Ordering::Relaxed)),
            total_429: AtomicU64::new(self.total_429.load(Ordering::Relaxed)),
            exit_ip: RwLock::new(self.exit_ip.read().ok().and_then(|g| g.clone())),
            latency: Mutex::new(self.latency.lock().map(|g| g.clone()).unwrap_or_default()),
            errors: Mutex::new(self.errors.lock().map(|g| g.clone()).unwrap_or_default()),
        }
    }
}

// ---------------------------------------------------------------------------
// SelectorStats
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// StickySessionManager
// ---------------------------------------------------------------------------

struct StickyEntry {
    node_url: String,
    pool_name: String,
    expires_at: Instant,
}

/// Manages sticky (pinned) sessions from source IP to proxy node.
pub struct StickySessionManager {
    inner: RwLock<HashMap<String, StickyEntry>>,
    ttl: Duration,
}

impl StickySessionManager {
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            ttl,
        }
    }

    /// Look up the pinned node for a source IP.
    pub fn get(&self, source_ip: &str) -> Option<(String, String)> {
        let guard = self.inner.read().ok()?;
        let entry = guard.get(source_ip)?;
        if Instant::now() < entry.expires_at {
            Some((entry.node_url.clone(), entry.pool_name.clone()))
        } else {
            None
        }
    }

    /// Pin a source IP to a node+pool.
    pub fn set(&self, source_ip: &str, node_url: &str, pool_name: &str) {
        if let Ok(mut guard) = self.inner.write() {
            guard.insert(
                source_ip.to_string(),
                StickyEntry {
                    node_url: node_url.to_string(),
                    pool_name: pool_name.to_string(),
                    expires_at: Instant::now() + self.ttl,
                },
            );
        }
    }

    /// Remove a sticky entry.
    pub fn clear(&self, source_ip: &str) {
        if let Ok(mut guard) = self.inner.write() {
            guard.remove(source_ip);
        }
    }

    /// Remove all expired entries. Returns the number removed.
    pub fn purge_expired(&self) -> usize {
        let mut to_remove = Vec::new();
        {
            let guard = match self.inner.read() {
                Ok(g) => g,
                Err(_) => return 0,
            };
            let now = Instant::now();
            for (k, v) in guard.iter() {
                if now >= v.expires_at {
                    to_remove.push(k.clone());
                }
            }
        }
        if to_remove.is_empty() {
            return 0;
        }
        if let Ok(mut guard) = self.inner.write() {
            let before = guard.len();
            for k in &to_remove {
                guard.remove(k);
            }
            before - guard.len()
        } else {
            0
        }
    }

    /// Number of active (non-expired) sticky entries.
    pub fn active_count(&self) -> usize {
        let guard = match self.inner.read() {
            Ok(g) => g,
            Err(_) => return 0,
        };
        let now = Instant::now();
        guard.values().filter(|e| now < e.expires_at).count()
    }
}

// ---------------------------------------------------------------------------
// PoolType / PoolConfig / PoolSelector
// ---------------------------------------------------------------------------

/// The type of proxy pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolType {
    Direct,
    Residential,
    Datacenter,
}

/// Configuration for a named proxy pool.
pub struct PoolConfig {
    pub name: String,
    pub weight: u32,
    pub pool_type: PoolType,
    pub nodes: Vec<Arc<ProxyNode>>,
}

impl Clone for PoolConfig {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            weight: self.weight,
            pool_type: self.pool_type,
            nodes: self.nodes.clone(),
        }
    }
}

/// Selects a pool based on weighted random selection, with sticky-session support.
pub struct PoolSelector {
    pools: RwLock<Vec<PoolConfig>>,
    sticky: StickySessionManager,
}

impl PoolSelector {
    pub fn new(sticky_ttl: Duration) -> Self {
        Self {
            pools: RwLock::new(Vec::new()),
            sticky: StickySessionManager::new(sticky_ttl),
        }
    }

    /// Select a pool (and within it, a node) for the given source IP.
    /// Returns `(pool_name, node_url)`.
    pub fn select(&self, _source_ip: Option<&str>) -> Option<(String, String)> {
        let guard = self.pools.read().ok()?;
        if guard.is_empty() {
            return None;
        }
        let total_weight: u32 = guard.iter().map(|p| p.weight).sum();
        if total_weight == 0 {
            return None;
        }
        let mut rng = rand::thread_rng();
        use rand::Rng;
        let pick = rng.gen_range(0..total_weight);
        let mut cumulative = 0u32;
        for pool in guard.iter() {
            cumulative += pool.weight;
            if pick < cumulative {
                // Pick a node from this pool (round-robin within pool)
                let node = pool.nodes.first().map(|n| n.url.clone());
                return node.map(|u| (pool.name.clone(), u));
            }
        }
        None
    }

    /// Return all available node URLs across all pools.
    pub fn all_available(&self) -> Vec<String> {
        let guard = match self.pools.read() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        let now = unix_now();
        guard
            .iter()
            .flat_map(|p| p.nodes.iter())
            .filter(|n| n.is_available(now))
            .map(|n| n.url.clone())
            .collect()
    }

    /// Number of pools.
    pub fn pool_count(&self) -> usize {
        self.pools.read().map(|g| g.len()).unwrap_or(0)
    }

    pub fn sticky(&self) -> &StickySessionManager {
        &self.sticky
    }

    pub fn pools(&self) -> Vec<PoolConfig> {
        self.pools.read().map(|g| g.clone()).unwrap_or_default()
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// ProxySelector
// ---------------------------------------------------------------------------

/// Selects proxy nodes via a 3-pass strategy:
///   1. Probed-and-OK nodes (round-robin)
///   2. Unprobed nodes (round-robin)
///   3. Any available node (round-robin fallback)
///
/// Tracks per-node health, blacklisting, and provides aggregate stats.
pub struct ProxySelector {
    nodes: Vec<Arc<ProxyNode>>,
    node_urls: Vec<String>,
    error_threshold: u32,
    cooldown_seconds: u64,
    recovery_interval: u64,
    next_index: AtomicUsize,
}

impl ProxySelector {
    /// Create a new ProxySelector from a list of node URLs.
    pub fn new(
        node_urls: Vec<String>,
        error_threshold: u32,
        cooldown_seconds: u64,
        recovery_interval: u64,
    ) -> Self {
        let nodes: Vec<Arc<ProxyNode>> = node_urls
            .iter()
            .map(|u| Arc::new(ProxyNode::new(u.clone())))
            .collect();
        Self {
            nodes,
            node_urls,
            error_threshold,
            cooldown_seconds,
            recovery_interval,
            next_index: AtomicUsize::new(0),
        }
    }

    /// Select the next available node using the 3-pass strategy.
    ///
    /// Pass 1: probed + OK nodes
    /// Pass 2: unprobed nodes
    /// Pass 3: any available node
    pub fn next(&self) -> Option<&str> {
        let now = unix_now();
        if self.nodes.is_empty() {
            return None;
        }

        let n = self.nodes.len();
        let start = self.next_index.fetch_add(1, Ordering::Relaxed) % n;

        // Pass 1: probed + OK
        for i in 0..n {
            let idx = (start + i) % n;
            let node = &self.nodes[idx];
            if node.probed_ok() && node.is_available(now) {
                return Some(node.url.as_str());
            }
        }

        // Pass 2: unprobed
        for i in 0..n {
            let idx = (start + i) % n;
            let node = &self.nodes[idx];
            if !node.probed() && node.is_available(now) {
                return Some(node.url.as_str());
            }
        }

        // Pass 3: any available
        for i in 0..n {
            let idx = (start + i) % n;
            let node = &self.nodes[idx];
            if node.is_available(now) {
                return Some(node.url.as_str());
            }
        }

        None
    }

    /// Record a successful request for the given node URL.
    pub fn record_success(&self, url: &str, bytes: u64, latency_us: u64) {
        if let Some(node) = self.find_node(url) {
            node.record_success(bytes, latency_us);
        }
    }

    /// Record an error for the given node URL. Blacklists the node if consecutive errors
    /// exceed the threshold.
    pub fn record_error(&self, url: &str, status: u16, latency_us: u64) {
        if let Some(node) = self.find_node(url) {
            let ce = node.record_error(status, latency_us);
            if ce >= self.error_threshold as u64 {
                let until = unix_now() + self.cooldown_seconds;
                node.blacklist(until);
            }
        }
    }

    /// Record a 429 response for the given node URL.
    pub fn record_429(&self, url: &str) {
        if let Some(node) = self.find_node(url) {
            node.record_429();
        }
    }

    /// Check blacklisted nodes and clear any whose cooldown has expired.
    pub fn recovery_check(&self) {
        let now = unix_now();
        for node in &self.nodes {
            if node.is_blacklisted(now) {
                // still in cooldown — skip
                continue;
            }
            // If the node was previously blacklisted but its time has passed, clear the flag.
            // We detect "previously blacklisted" by checking if blacklisted_until is > 0
            // but <= now (meaning it expired).
            let until = node.blacklisted_until.load(Ordering::Relaxed);
            if until > 0 && until <= now {
                node.clear_blacklist();
                node.consecutive_errors.store(0, Ordering::Relaxed);
                node.consecutive_429.store(0, Ordering::Relaxed);
            }
        }
    }

    // -- Aggregate stats ----------------------------------------------------

    pub fn total_nodes(&self) -> usize {
        self.nodes.len()
    }

    pub fn available_nodes(&self) -> usize {
        let now = unix_now();
        self.nodes.iter().filter(|n| n.is_available(now)).count()
    }

    pub fn blacklisted_nodes(&self) -> usize {
        let now = unix_now();
        self.nodes
            .iter()
            .filter(|n| n.blacklisted_until.load(Ordering::Relaxed) > now)
            .count()
    }

    pub fn probed_ok(&self) -> u64 {
        self.nodes.iter().filter(|n| n.probed_ok()).count() as u64
    }

    pub fn probed_fail(&self) -> u64 {
        self.nodes
            .iter()
            .filter(|n| n.probed() && !n.probed_ok.load(Ordering::Relaxed))
            .count() as u64
    }

    pub fn unprobed(&self) -> usize {
        self.nodes.iter().filter(|n| !n.probed()).count()
    }

    pub fn upstream_backoff(&self) -> bool {
        // Conservative heuristic: backoff if more than half of nodes are unavailable.
        let total = self.nodes.len();
        if total == 0 {
            return false;
        }
        let now = unix_now();
        let available = self.nodes.iter().filter(|n| n.is_available(now)).count();
        available < total / 2
    }

    pub fn upstream_429_rate(&self) -> f64 {
        let total_reqs: u64 = self
            .nodes
            .iter()
            .map(|n| n.total_requests.load(Ordering::Relaxed))
            .sum();
        let total_429: u64 = self
            .nodes
            .iter()
            .map(|n| n.total_429.load(Ordering::Relaxed))
            .sum();
        if total_reqs == 0 {
            0.0
        } else {
            total_429 as f64 / total_reqs as f64
        }
    }

    pub fn stats(&self) -> SelectorStats {
        SelectorStats {
            total_nodes: self.total_nodes(),
            available_nodes: self.available_nodes(),
            blacklisted_nodes: self.blacklisted_nodes(),
            probed_ok: self.probed_ok(),
            probed_fail: self.probed_fail(),
            unprobed: self.unprobed(),
            upstream_backoff: self.upstream_backoff(),
            upstream_429_rate: self.upstream_429_rate(),
        }
    }

    // -- Accessors ----------------------------------------------------------

    /// Return a reference to the internal node list.
    pub fn nodes(&self) -> &[Arc<ProxyNode>] {
        &self.nodes
    }

    /// Return the raw node URL strings.
    pub fn node_urls(&self) -> &[String] {
        &self.node_urls
    }

    /// Serialize per-node pool statistics as a JSON value.
    pub fn pool_stats_json(&self) -> serde_json::Value {
        let now = unix_now();
        let stats: Vec<serde_json::Value> = self
            .nodes
            .iter()
            .map(|n| {
                serde_json::json!({
                    "url": n.url,
                    "available": n.is_available(now),
                    "blacklisted": n.is_blacklisted(now),
                    "probed": n.probed(),
                    "probed_ok": n.probed_ok(),
                    "total_requests": n.total_requests.load(Ordering::Relaxed),
                    "total_errors": n.total_errors.load(Ordering::Relaxed),
                    "total_bytes": n.total_bytes.load(Ordering::Relaxed),
                    "total_429": n.total_429.load(Ordering::Relaxed),
                    "consecutive_errors": n.consecutive_errors.load(Ordering::Relaxed),
                    "consecutive_429": n.consecutive_429.load(Ordering::Relaxed),
                    "error_rate": n.error_rate(),
                    "latency_p50_us": n.latency_p50(),
                    "latency_p95_us": n.latency_p95(),
                    "latency_p99_us": n.latency_p99(),
                    "latency_avg_us": n.latency_avg(),
                    "exit_ip": n.exit_ip(),
                })
            })
            .collect();
        serde_json::json!({
            "total_nodes": self.total_nodes(),
            "available_nodes": self.available_nodes(),
            "blacklisted_nodes": self.blacklisted_nodes(),
            "probed_ok": self.probed_ok(),
            "probed_fail": self.probed_fail(),
            "unprobed": self.unprobed(),
            "upstream_backoff": self.upstream_backoff(),
            "upstream_429_rate": self.upstream_429_rate(),
            "nodes": stats,
        })
    }

    // -- Internal helpers ---------------------------------------------------

    fn find_node(&self, url: &str) -> Option<&Arc<ProxyNode>> {
        self.nodes.iter().find(|n| n.url == url)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- ProxyNode tests ----------------------------------------------------

    #[test]
    fn test_proxy_node_creation() {
        let node = ProxyNode::new("socks5://127.0.0.1:9050");
        assert_eq!(node.url, "socks5://127.0.0.1:9050");
        assert!(node.is_available(unix_now()));
        assert!(!node.probed());
        assert_eq!(node.total_requests.load(Ordering::Relaxed), 0);
        assert_eq!(node.total_errors.load(Ordering::Relaxed), 0);
        assert_eq!(node.total_bytes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_node_record_success() {
        let node = ProxyNode::new("socks5://127.0.0.1:9050");
        node.record_success(1000, 50_000);
        assert_eq!(node.total_requests.load(Ordering::Relaxed), 1);
        assert_eq!(node.total_bytes.load(Ordering::Relaxed), 1000);
        assert_eq!(node.consecutive_errors(), 0);
        assert!(node.latency_avg() > 0.0);
    }

    #[test]
    fn test_node_record_error() {
        let node = ProxyNode::new("socks5://127.0.0.1:9050");
        let ce = node.record_error(502, 30_000);
        assert_eq!(ce, 1);
        assert_eq!(node.total_requests.load(Ordering::Relaxed), 1);
        assert_eq!(node.total_errors.load(Ordering::Relaxed), 1);
        assert_eq!(node.consecutive_errors(), 1);
    }

    #[test]
    fn test_node_blacklist() {
        let node = ProxyNode::new("socks5://127.0.0.1:9050");
        assert!(!node.is_blacklisted(unix_now()));
        node.blacklist(unix_now() + 3600);
        assert!(node.is_blacklisted(unix_now()));
        node.clear_blacklist();
        assert!(!node.is_blacklisted(unix_now()));
    }

    #[test]
    fn test_node_availability() {
        let node = ProxyNode::new("socks5://127.0.0.1:9050");
        assert!(node.is_available(unix_now()));
        node.blacklist(unix_now() + 3600);
        assert!(!node.is_available(unix_now()));
        node.clear_blacklist();
        assert!(node.is_available(unix_now()));
    }

    #[test]
    fn test_node_set_probed() {
        let node = ProxyNode::new("socks5://127.0.0.1:9050");
        assert!(!node.probed());
        node.set_probed(true);
        assert!(node.probed());
        assert!(node.probed_ok());
        node.set_probed(false);
        assert!(node.probed());
        assert!(!node.probed_ok());
    }

    #[test]
    fn test_node_exit_ip() {
        let node = ProxyNode::new("socks5://127.0.0.1:9050");
        assert!(node.exit_ip().is_none());
        node.set_exit_ip(Some("203.0.113.1".into()));
        assert_eq!(node.exit_ip(), Some("203.0.113.1".into()));
    }

    #[test]
    fn test_node_record_429() {
        let node = ProxyNode::new("socks5://127.0.0.1:9050");
        node.record_429();
        assert_eq!(node.total_429(), 1);
        assert_eq!(node.consecutive_429(), 1);
        node.record_429();
        assert_eq!(node.total_429(), 2);
        assert_eq!(node.consecutive_429(), 2);
        // success should reset consecutive_429
        node.record_success(100, 1000);
        assert_eq!(node.consecutive_429(), 0);
    }

    #[test]
    fn test_latency_percentile() {
        let data = Mutex::new(VecDeque::from(vec![10, 20, 30, 40, 50]));
        assert!((percentile(&data, 50.0) - 30.0).abs() < 0.01);
        assert!((percentile(&data, 95.0) - 48.0).abs() < 0.01);
        assert!((percentile(&data, 99.0) - 49.6).abs() < 0.01);
        // empty
        let empty = Mutex::new(VecDeque::new());
        assert!((percentile(&empty, 50.0) - 0.0).abs() < 0.01);
        // single
        let single = Mutex::new(VecDeque::from(vec![42]));
        assert!((percentile(&single, 50.0) - 42.0).abs() < 0.01);
    }

    // -- ProxySelector tests ------------------------------------------------

    #[test]
    fn test_proxy_selector_rr() {
        let urls: Vec<String> = vec![
            "socks5://a:1".into(),
            "socks5://b:2".into(),
            "socks5://c:3".into(),
        ];
        let sel = ProxySelector::new(urls, 3, 60, 300);
        assert_eq!(sel.total_nodes(), 3);
        // All unprobed — first pass should pick one
        let n1 = sel.next().expect("should get node").to_string();
        assert!(!n1.is_empty());
        // Consecutive calls
        let n2 = sel.next().expect("should get node").to_string();
        let n3 = sel.next().expect("should get node").to_string();
        // Should cycle through all three (order may vary since they're all unprobed)
        let mut seen = std::collections::HashSet::new();
        seen.insert(n1);
        seen.insert(n2);
        seen.insert(n3);
        assert_eq!(seen.len(), 3);
    }

    #[test]
    fn test_proxy_selector_empty() {
        let sel = ProxySelector::new(vec![], 3, 60, 300);
        assert!(sel.next().is_none());
        assert_eq!(sel.total_nodes(), 0);
        assert_eq!(sel.available_nodes(), 0);
    }

    #[test]
    fn test_proxy_selector_blacklist() {
        let urls: Vec<String> = vec!["socks5://a:1".into()];
        let sel = ProxySelector::new(urls, 2, 60, 300);
        // Record 2 errors to trigger blacklist
        sel.record_error("socks5://a:1", 502, 1000);
        sel.record_error("socks5://a:1", 502, 1000);
        // Node should be blacklisted
        assert!(sel.blacklisted_nodes() > 0);
        assert_eq!(sel.available_nodes(), 0);
        assert!(sel.next().is_none());
    }

    #[test]
    fn test_proxy_selector_record_success() {
        let urls: Vec<String> = vec!["socks5://a:1".into()];
        let sel = ProxySelector::new(urls, 3, 60, 300);
        sel.record_success("socks5://a:1", 500, 100_000);
        let node = &sel.nodes[0];
        assert_eq!(node.total_requests.load(Ordering::Relaxed), 1);
        assert_eq!(node.total_bytes.load(Ordering::Relaxed), 500);
    }

    #[test]
    fn test_proxy_selector_stats() {
        let urls: Vec<String> = vec![
            "socks5://a:1".into(),
            "socks5://b:2".into(),
        ];
        let sel = ProxySelector::new(urls, 3, 60, 300);
        let s = sel.stats();
        assert_eq!(s.total_nodes, 2);
        assert_eq!(s.available_nodes, 2);
        assert_eq!(s.blacklisted_nodes, 0);
        assert_eq!(s.unprobed, 2);

        sel.nodes[0].set_probed(true);
        let s2 = sel.stats();
        assert_eq!(s2.probed_ok, 1);
        assert_eq!(s2.unprobed, 1);
    }

    #[test]
    fn test_recovery_check() {
        let urls: Vec<String> = vec!["socks5://a:1".into()];
        let sel = ProxySelector::new(urls, 1, 1, 300);
        sel.record_error("socks5://a:1", 502, 1000);
        assert!(sel.blacklisted_nodes() > 0);
        // recovery check should not clear while within cooldown
        sel.recovery_check();
        assert!(sel.blacklisted_nodes() > 0);
    }

    // -- StickySessionManager tests -----------------------------------------

    #[test]
    fn test_sticky_basic() {
        let mgr = StickySessionManager::new(Duration::from_secs(60));
        assert!(mgr.get("1.2.3.4").is_none());
        mgr.set("1.2.3.4", "socks5://a:1", "residential");
        let result = mgr.get("1.2.3.4");
        assert!(result.is_some());
        let (url, pool) = result.unwrap();
        assert_eq!(url, "socks5://a:1");
        assert_eq!(pool, "residential");
    }

    #[test]
    fn test_sticky_purge_expired() {
        let mgr = StickySessionManager::new(Duration::from_millis(1));
        mgr.set("1.2.3.4", "socks5://a:1", "residential");
        std::thread::sleep(Duration::from_millis(5));
        let removed = mgr.purge_expired();
        assert!(removed > 0);
        assert!(mgr.get("1.2.3.4").is_none());
    }

    #[test]
    fn test_sticky_clear() {
        let mgr = StickySessionManager::new(Duration::from_secs(60));
        mgr.set("1.2.3.4", "socks5://a:1", "residential");
        assert!(mgr.get("1.2.3.4").is_some());
        mgr.clear("1.2.3.4");
        assert!(mgr.get("1.2.3.4").is_none());
    }

    #[test]
    fn test_sticky_active_count() {
        let mgr = StickySessionManager::new(Duration::from_secs(60));
        assert_eq!(mgr.active_count(), 0);
        mgr.set("1.2.3.4", "socks5://a:1", "residential");
        mgr.set("5.6.7.8", "socks5://b:2", "datacenter");
        assert_eq!(mgr.active_count(), 2);
        mgr.clear("1.2.3.4");
        assert_eq!(mgr.active_count(), 1);
    }

    #[test]
    fn test_sticky_session_expired() {
        let mgr = StickySessionManager::new(Duration::from_millis(1));
        mgr.set("1.2.3.4", "socks5://a:1", "residential");
        std::thread::sleep(Duration::from_millis(5));
        // Entry still exists but should return None because TTL expired
        assert!(mgr.get("1.2.3.4").is_none());
    }

    // -- PoolSelector tests -------------------------------------------------

    #[test]
    fn test_pool_selector_empty() {
        let ps = PoolSelector::new(Duration::from_secs(60));
        assert_eq!(ps.pool_count(), 0);
        assert!(ps.all_available().is_empty());
    }

    // -- ProxySelector extra tests ------------------------------------------

    #[test]
    fn test_proxy_selector_3pass_prefers_probed_ok() {
        let urls: Vec<String> = vec![
            "socks5://a:1".into(),
            "socks5://b:2".into(),
            "socks5://c:3".into(),
        ];
        let sel = ProxySelector::new(urls, 3, 60, 300);
        // Mark node a as probed_ok
        sel.nodes[0].set_probed(true);
        // next() should pick the probed_ok node first (a:1)
        let picked = sel.next().expect("should get node");
        assert_eq!(picked, "socks5://a:1");
    }

    #[test]
    fn test_proxy_selector_3pass_falls_back_to_unprobed() {
        let urls: Vec<String> = vec![
            "socks5://a:1".into(),
            "socks5://b:2".into(),
        ];
        let sel = ProxySelector::new(urls, 3, 60, 300);
        // probed_ok the first node, but blacklist it
        sel.nodes[0].set_probed(true);
        sel.nodes[0].blacklist(unix_now() + 3600);
        // Should fall through to unprobed (b)
        let picked = sel.next().expect("should get node");
        assert!(!picked.contains("a:1"), "should not pick blacklisted node");
    }

    #[test]
    fn test_proxy_selector_3pass_falls_back_to_any() {
        let urls: Vec<String> = vec![
            "socks5://a:1".into(),
            "socks5://b:2".into(),
            "socks5://c:3".into(),
        ];
        let sel = ProxySelector::new(urls, 3, 60, 300);
        // Blacklist all nodes
        let now = unix_now();
        for node in &sel.nodes {
            node.set_probed(true); // mark as probed
            node.blacklist(now + 3600);
        }
        // Should return None since all are blacklisted
        assert!(sel.next().is_none());
    }

    #[test]
    fn test_pool_stats_json() {
        let urls: Vec<String> = vec!["socks5://a:1".into()];
        let sel = ProxySelector::new(urls, 3, 60, 300);
        let json = sel.pool_stats_json();
        assert_eq!(json["total_nodes"], 1);
        assert_eq!(json["nodes"][0]["url"], "socks5://a:1");
    }

    #[test]
    fn test_node_health_after_success_resets_errors() {
        let node = ProxyNode::new("socks5://a:1");
        node.record_error(502, 1000);
        node.record_error(503, 1000);
        assert_eq!(node.consecutive_errors(), 2);
        node.record_success(100, 1000);
        assert_eq!(node.consecutive_errors(), 0);
    }

    #[test]
    fn test_error_rate() {
        let node = ProxyNode::new("socks5://a:1");
        assert!((node.error_rate() - 0.0).abs() < f64::EPSILON);
        node.record_error(502, 1000);
        node.record_success(100, 1000);
        node.record_success(100, 1000);
        assert!((node.error_rate() - 1.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn test_selector_upstream_backoff_heuristic() {
        let urls: Vec<String> = vec![
            "socks5://a:1".into(),
            "socks5://b:2".into(),
        ];
        let sel = ProxySelector::new(urls, 3, 60, 300);
        // Both available — no backoff
        assert!(!sel.upstream_backoff());
        // Blacklist one — still less than half, no backoff
        sel.nodes[0].blacklist(unix_now() + 3600);
        assert!(!sel.upstream_backoff());
        // Blacklist both — should backoff
        sel.nodes[1].blacklist(unix_now() + 3600);
        assert!(sel.upstream_backoff());
    }
}
