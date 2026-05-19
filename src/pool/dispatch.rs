use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::Duration;

use crate::pool::*;
use reqwest::Client;

const SCORE_SCALE: u64 = 100;

struct PoolNode {
    node: NodeRef,
    base_score: AtomicU64,
    consecutive_successes: AtomicU32,
    recent_results: RwLock<VecDeque<bool>>,
    avg_latency_ms: AtomicU64,
    idle_since: AtomicI64,
    max_concurrent: AtomicU32,
    client: Client,
}

impl PoolNode {
    fn new(node: NodeRef, client: Client) -> Self {
        Self {
            node,
            base_score: AtomicU64::new(80 * SCORE_SCALE),
            consecutive_successes: AtomicU32::new(0),
            recent_results: RwLock::new(VecDeque::with_capacity(20)),
            avg_latency_ms: AtomicU64::new(0),
            idle_since: AtomicI64::new(chrono::Utc::now().timestamp()),
            max_concurrent: AtomicU32::new(5),
            client,
        }
    }

    fn score(&self) -> f64 {
        let base_pct = self.base_score.load(Ordering::Relaxed) as f64 / SCORE_SCALE as f64;
        let health = (base_pct / 100.0).min(1.0).max(0.0) * 0.50;

        let recent = self.recent_results.read().unwrap();
        let total = recent.len();
        let successes = recent.iter().filter(|&&r| r).count();
        let success_rate = if total > 0 {
            successes as f64 / total as f64 * 0.20
        } else {
            0.0
        };
        drop(recent);

        let now = chrono::Utc::now().timestamp();
        let idle_secs = now - self.idle_since.load(Ordering::Relaxed);
        let idle_factor = (idle_secs as f64 / 60.0).min(1.0) * 0.15;

        let avg_lat = self.avg_latency_ms.load(Ordering::Relaxed) as f64;
        let latency_factor = (1.0 - (avg_lat / 5000.0).min(1.0)).max(0.0) * 0.10;

        let consec = self.consecutive_successes.load(Ordering::Relaxed) as f64;
        let momentum = (consec / 10.0).min(1.0) * 0.05;

        health + success_rate + idle_factor + latency_factor + momentum
    }

    fn record_result(&self, success: bool, latency_ms: u64) {
        {
            let mut recent = self.recent_results.write().unwrap();
            recent.push_back(success);
            while recent.len() > 20 {
                recent.pop_front();
            }
        }

        self.avg_latency_ms.store(latency_ms, Ordering::Relaxed);

        if success {
            let prev = self.consecutive_successes.fetch_add(1, Ordering::Relaxed);
            let _ = prev;
        } else {
            self.consecutive_successes.store(0, Ordering::Relaxed);
        }
    }
}

pub struct DispatchPool {
    nodes: RwLock<Vec<PoolNode>>,
    idle_since: AtomicI64,
}

impl DispatchPool {
    pub fn new() -> Self {
        Self {
            nodes: RwLock::new(Vec::new()),
            idle_since: AtomicI64::new(chrono::Utc::now().timestamp()),
        }
    }
}

impl Default for DispatchPool {
    fn default() -> Self {
        Self::new()
    }
}

impl Pool for DispatchPool {
    fn acquire(&self) -> Option<NodeRef> {
        let nodes = self.nodes.read().unwrap();
        if nodes.is_empty() {
            return None;
        }

        let total: f64 = nodes.iter().map(|n| n.score()).sum();
        if total <= 0.0 {
            return None;
        }

        let threshold = fastrand::f64() * total;
        let mut cumulative = 0.0;
        for n in nodes.iter() {
            cumulative += n.score();
            if cumulative >= threshold {
                return Some(n.node.clone());
            }
        }

        None
    }

    fn try_acquire_sticky(&self, _meta: &RequestMeta, node_id: &NodeId) -> Result<NodeRef, DispatchError> {
        let nodes = self.nodes.read().unwrap();
        nodes
            .iter()
            .find(|n| n.node.id == *node_id)
            .map(|n| n.node.clone())
            .ok_or(DispatchError::NoResource)
    }

    fn release(&self, node_id: &NodeId, result: &ResultKind) {
        let mut nodes = self.nodes.write().unwrap();
        if let Some(pn) = nodes.iter_mut().find(|n| n.node.id == *node_id) {
            match result {
                ResultKind::Success(_) => {
                    pn.record_result(true, 0);
                    pn.idle_since
                        .store(chrono::Utc::now().timestamp(), Ordering::Relaxed);
                }
                ResultKind::RateLimited => {
                    pn.record_result(false, 0);
                }
                ResultKind::Error { .. } => {
                    pn.record_result(false, 0);
                }
            }
        }
    }

    fn remove(&self, node_id: &NodeId) {
        let mut nodes = self.nodes.write().unwrap();
        nodes.retain(|n| n.node.id != *node_id);
    }

    fn add(&self, node: NodeRef) {
        let mut nodes = self.nodes.write().unwrap();
        if !nodes.iter().any(|n| n.node.id == node.id) {
            let client = reqwest::Client::builder()
                .proxy(reqwest::Proxy::all(&node.url).unwrap())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap();
            nodes.push(PoolNode::new(node, client));
        }
    }

    fn available(&self) -> usize {
        self.nodes.read().unwrap().len()
    }

    fn name(&self) -> &'static str {
        "dispatch"
    }
}
