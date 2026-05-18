pub mod active;
pub mod dead;
pub mod dispatch;
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

pub trait Pool: Send + Sync {
    fn acquire(&self) -> Option<NodeRef>;
    fn release(&self, node_id: &NodeId, result: &ResultKind);
    fn remove(&self, node_id: &NodeId);
    fn add(&self, node: NodeRef);
    fn available(&self) -> usize;
    fn name(&self) -> &'static str;
}

pub trait PoolManager: Send + Sync {
    fn dispatch(&self, req: &RequestMeta) -> Result<DispatchResult, DispatchError>;
    fn report(&self, node_id: NodeId, result: ResultKind, latency_us: u64);
    fn pool_stats(&self) -> PoolStats;
    fn fuse_all(&self);
    fn unfuse_all(&self);
}

pub trait RateLimitedPool: Pool {
    fn quarantine(&self, node_id: NodeId);
    fn select_for_probe(&self, batch_size: usize) -> Vec<NodeId>;
    fn recover(&self, node_id: &NodeId);
    fn quarantined_today(&self) -> usize;
}

pub trait DeadPool: Pool {
    fn bury(&self, node_id: NodeId);
    fn select_all_for_probe(&self) -> Vec<NodeId>;
    fn recover(&self, node_id: &NodeId);
    fn dead_count(&self, node_id: &NodeId) -> u32;
}

pub trait NodeProvider: Send + Sync {
    type NodeId: Clone + Hash + Eq + Debug;
    fn all_urls(&self) -> Vec<String>;
    fn id_for_url(&self, url: &str) -> Self::NodeId;
    fn name(&self) -> &'static str;
}

pub fn sha256_first8(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(input.as_bytes());
    hex::encode(&hash[..4])
}
