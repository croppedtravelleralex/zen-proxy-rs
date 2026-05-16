use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};
use tracing::{info, warn, trace};

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IPStats {
    pub pool_name: String,
    pub total_requests: u64,
    pub total_errors: u64,
    pub total_bytes: u64,
}

pub struct NodeDB {
    nodes: RwLock<HashMap<String, NodeStats>>,
    ip_stats: RwLock<HashMap<String, IPStats>>,
    path: PathBuf,
    ip_path: PathBuf,
}

fn load_or_default(path: &str) -> HashMap<String, NodeStats> {
    match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
            warn!(error = %e, "failed to parse node_db file");
            HashMap::new()
        }),
        Err(_) => {
            info!(path, "node_db file not found, starting empty");
            HashMap::new()
        }
    }
}

fn load_ip_or_default(path: &str) -> HashMap<String, IPStats> {
    match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

impl NodeDB {
    pub fn new(path: &str, ip_path: &str) -> Self {
        let nodes = load_or_default(path);
        let ip_stats = load_ip_or_default(ip_path);
        Self {
            nodes: RwLock::new(nodes),
            ip_stats: RwLock::new(ip_stats),
            path: PathBuf::from(path),
            ip_path: PathBuf::from(ip_path),
        }
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
        if let Ok(nodes) = self.nodes.read() {
            if let Ok(json) = serde_json::to_string_pretty(&*nodes) {
                if let Err(e) = std::fs::write(&self.path, &json) {
                    warn!(error = %e, "failed to persist node_db");
                }
            }
        }
        if let Ok(ip_stats) = self.ip_stats.read() {
            if let Ok(json) = serde_json::to_string_pretty(&*ip_stats) {
                if let Err(e) = std::fs::write(&self.ip_path, &json) {
                    warn!(error = %e, "failed to persist ip_stats");
                }
            }
        }
    }

    pub fn purge_stale(&self, max_age_secs: u64) {
        if let Ok(nodes) = self.nodes.write() {
            let _cutoff = Instant::now() - Duration::from_secs(max_age_secs);
            let before = nodes.len();
            if before > 0 {
                trace!("node_db purge_stale: {} nodes, keeping all", before);
            }
        }
    }

    pub fn node_count(&self) -> usize {
        self.nodes.read().map(|n| n.len()).unwrap_or(0)
    }
}

pub struct IPStatsTracker {
    stats: RwLock<HashMap<String, IPStats>>,
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
