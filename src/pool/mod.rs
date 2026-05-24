pub mod active;
pub mod dead;
pub mod dispatch;
pub mod global_budget;
pub mod manager;
pub mod probe_period;
pub mod ratelimited;

use std::fmt::Debug;
use std::hash::Hash;

pub type NodeId = String;

#[derive(Debug, Clone)]
pub struct NodeRef<T = NodeId> {
    pub id: T,
    pub url: String,
}

impl NodeRef {
    pub fn new(url: String) -> Self {
        let id = sha256_first8(&url);
        Self { id, url }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultKind {
    Success(u16),
    RateLimited,
    Error { kind: ErrorKind },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Timeout,
    ConnectionRefused,
    DnsFailure,
    SocksHandshake,
    Upstream5xx,
    Other,
}

#[derive(Debug, Clone)]
pub struct DispatchResult {
    pub node: NodeRef,
    pub client: reqwest::Client,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchError {
    NoResource,
    CircuitOpen,
}

#[derive(Debug, Clone)]
pub struct PoolStats {
    pub dispatch_size: usize,
    pub active_size: usize,
    pub ratelimited_size: usize,
    pub dead_size: usize,
    pub pool_transitions: u64,
    pub active_concurrency: usize,
    pub fuse: bool,
    pub cooldown_size: usize,
    pub budget_limited_size: usize,
    pub leased_count: usize,
}

impl PoolStats {
    pub fn total(&self) -> usize {
        self.dispatch_size + self.active_size + self.ratelimited_size + self.dead_size
    }
}

#[derive(Debug, Clone)]
pub struct RequestMeta {
    pub model: String,
    pub stream: bool,
    pub body_size: u64,
}

impl RequestMeta {
    pub fn estimated_input_tokens(&self) -> u64 {
        (self.body_size / 4).max(1)
    }

    pub fn request_kb(&self) -> u64 {
        self.body_size.div_ceil(1024).max(1)
    }
}

pub trait Pool: Send + Sync {
    fn acquire(&self) -> Option<NodeRef>;
    fn acquire_for(&self, _meta: &RequestMeta) -> Option<NodeRef> {
        self.acquire()
    }
    fn budget_counts(&self) -> (usize, usize, usize) {
        (0, 0, 0)
    }
    fn try_acquire_sticky(
        &self,
        _meta: &RequestMeta,
        _node_id: &NodeId,
    ) -> Result<NodeRef, DispatchError> {
        Err(DispatchError::NoResource)
    }
    fn release(&self, node_id: &NodeId, result: &ResultKind);
    fn remove(&self, node_id: &NodeId);
    fn add(&self, node: NodeRef);
    fn available(&self) -> usize;
    fn name(&self) -> &'static str;
}

pub trait PoolManager: Send + Sync {
    fn dispatch(&self, req: &RequestMeta) -> Result<DispatchResult, DispatchError>;
    fn dispatch_sticky(
        &self,
        meta: &RequestMeta,
        node_id: &str,
    ) -> Result<DispatchResult, DispatchError>;
    fn report(&self, node_id: NodeId, result: ResultKind, latency_us: u64);
    fn pool_stats(&self) -> PoolStats;
    fn fuse_all(&self);
    fn unfuse_all(&self);
    fn add_node(&self, url: &str);
    fn remove_node(&self, node_id: &str);
    fn probe_node(&self, node_id: &str) -> Option<ProbeResult>;
    fn recover_node(&self, node_id: &str);
    fn probe_all(&self);
    fn probe_dead_adaptive(&self);
}

pub trait RateLimitedPool: Pool {
    fn quarantine(&self, node_id: NodeId);
    fn select_for_probe(&self, batch_size: usize) -> Vec<NodeId>;
    fn select_all_for_probe(&self, batch_size: usize) -> Vec<NodeId>;
    fn recover(&self, node_id: &NodeId);
    fn quarantined_today(&self) -> usize;
    fn get_node_ref(&self, node_id: &NodeId) -> Option<NodeRef>;
}

pub trait DeadPool: Pool {
    fn bury(&self, node_id: NodeId);
    fn select_all_for_probe(&self) -> Vec<NodeId>;
    fn dead_age_secs(&self, node_id: &NodeId) -> Option<u64>;
    fn last_probe_age_secs(&self, node_id: &NodeId) -> Option<u64>;
    fn record_probe_result(&self, node_id: &NodeId, success: bool) -> u8;
    fn recover(&self, node_id: &NodeId);
    fn dead_count(&self, node_id: &NodeId) -> u32;
    fn get_node_ref(&self, node_id: &NodeId) -> Option<NodeRef>;
}

pub trait NodeProvider: Send + Sync {
    type NodeId: Clone + Hash + Eq + Debug;
    fn all_urls(&self) -> Vec<String>;
    fn id_for_url(&self, url: &str) -> Self::NodeId;
    fn name(&self) -> &'static str;
}

pub struct ProbeResult {
    pub success: bool,
    pub latency_ms: u64,
}

pub fn sha256_first8(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(input.as_bytes());
    hex::encode(&hash[..4])
}
