use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};
use tracing::{info, trace};

pub struct NodeDB {
    nodes: RwLock<HashMap<String, NodeStats>>,
    ip_stats: RwLock<HashMap<String, IPStats>>,
}

#[derive(Debug, Clone)]
pub struct NodeStats {
    pub url: String,
    pub pool: String,
    pub tier: String,
    pub alive: bool,
    pub score: f64,
    pub total_requests: u64,
    pub total_errors: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct IPStats {
    pub pool_name: String,
    pub total_requests: u64,
    pub total_errors: u64,
    pub total_bytes: u64,
}

pub struct IPStatsTracker {
    stats: RwLock<HashMap<String, IPStats>>,
}

impl NodeDB {
    pub fn new() -> Self {
        Self { nodes: RwLock::new(HashMap::new()), ip_stats: RwLock::new(HashMap::new()) }
    }
    pub fn record(&self, url: &str, success: bool, bytes: u64) {
        if let Ok(mut nodes) = self.nodes.write() {
            let entry = nodes.entry(url.to_string()).or_insert(NodeStats {
                url: url.to_string(), pool: String::new(), tier: "L3".into(),
                alive: true, score: 100.0, total_requests: 0, total_errors: 0, total_bytes: 0,
            });
            entry.total_requests += 1; entry.total_bytes += bytes;
            if !success { entry.total_errors += 1; entry.score = (entry.score * 0.8).max(0.0); }
            else { entry.score = (entry.score * 0.2 + 100.0 * 0.8).min(100.0); }
        }
    }
    pub fn persist(&self) {
        // TODO: serialize nodes to disk file
        info!("node_db persist skipped (disk persistence not yet wired)");
    }
    pub fn purge_stale(&self, max_age_secs: u64) {
        if let Ok(nodes) = self.nodes.write() {
            let _cutoff = Instant::now() - Duration::from_secs(max_age_secs);
            // Stale purge not implemented yet - nodes are kept
            let before = nodes.len();
            // For now just log
            if before > 0 {
                trace!("node_db purge_stale: {} nodes, keeping all", before);
            }
        }
    }
    pub fn node_count(&self) -> usize {
        self.nodes.read().map(|n| n.len()).unwrap_or(0)
    }
}

impl IPStatsTracker {
    pub fn new() -> Self { Self { stats: RwLock::new(HashMap::new()) } }
    pub fn record(&self, exit_ip: &str, pool_name: &str, success: bool, bytes: u64) {
        if let Ok(mut stats) = self.stats.write() {
            let entry = stats.entry(exit_ip.to_string()).or_insert(IPStats {
                pool_name: pool_name.to_string(), total_requests: 0, total_errors: 0, total_bytes: 0,
            });
            entry.total_requests += 1; entry.total_bytes += bytes;
            if !success { entry.total_errors += 1; }
        }
    }
    pub fn flush(&self) {}
}
