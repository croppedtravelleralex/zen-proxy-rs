pub mod active;
pub mod dead;
pub mod dispatch;
pub mod global_budget;
pub mod manager;
pub mod node_registry;
pub mod probe_period;
pub mod ratelimited;
pub mod transport;

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
    EmptyOutput,
    SoftFailure { kind: ErrorKind },
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
    pub affinity_hit: bool,
    pub affinity_node_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchError {
    NoResource,
    CircuitOpen,
    RequestTooLarge,
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
    pub affinity_key: String,
}

impl RequestMeta {
    pub fn estimated_input_tokens(&self) -> u64 {
        (self.body_size / 4).max(1)
    }

    pub fn token_bucket(&self) -> &'static str {
        token_bucket(self.estimated_input_tokens())
    }

    pub fn request_kb(&self) -> u64 {
        self.body_size.div_ceil(1024).max(1)
    }

    pub fn body_size_bucket(&self) -> &'static str {
        body_size_bucket(self.body_size)
    }
}

pub fn body_size_bucket(body_size: u64) -> &'static str {
    match body_size {
        0..=131_071 => "tiny",
        131_072..=262_143 => "small",
        262_144..=524_287 => "medium",
        524_288..=1_048_575 => "large",
        _ => "huge",
    }
}

pub fn token_bucket(tokens: u64) -> &'static str {
    match tokens {
        0..=49_999 => "under_50k",
        50_000..=99_999 => "50k_100k",
        100_000..=199_999 => "100k_200k",
        200_000..=399_999 => "200k_400k",
        _ => "400k_plus",
    }
}

pub trait Pool: Send + Sync {
    fn acquire(&self) -> Option<NodeRef>;
    fn preflight(&self, _meta: &RequestMeta) -> Result<(), DispatchError> {
        Ok(())
    }
    fn acquire_for(&self, _meta: &RequestMeta) -> Option<NodeRef> {
        self.acquire()
    }
    fn budget_counts(&self) -> (usize, usize, usize) {
        (0, 0, 0)
    }
    fn budget_details(&self) -> Vec<serde_json::Value> {
        Vec::new()
    }
    fn node_budget_detail(&self, _node_id: &NodeId) -> Option<serde_json::Value> {
        None
    }
    fn try_acquire_sticky(
        &self,
        _meta: &RequestMeta,
        _node_id: &NodeId,
    ) -> Result<NodeRef, DispatchError> {
        Err(DispatchError::NoResource)
    }
    fn release_with_latency(&self, node_id: &NodeId, result: &ResultKind, latency_ms: u64) {
        let _ = latency_ms;
        self.release(node_id, result);
    }
    fn record_latency_hint(&self, _node_id: &NodeId, _latency_ms: u64) {}
    fn record_bucket_latency_hint(&self, _node_id: &NodeId, _bucket: &str, _latency_ms: u64) {}
    fn try_acquire_affinity(
        &self,
        _meta: &RequestMeta,
    ) -> Result<(NodeRef, NodeId), DispatchError> {
        Err(DispatchError::NoResource)
    }
    fn record_affinity_success(&self, _affinity_key: &str, _node_id: &NodeId) {}
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
    fn record_latency_hint(&self, node_id: NodeId, latency_ms: u64);
    fn record_bucket_latency_hint(&self, node_id: NodeId, bucket: &str, latency_ms: u64);
    fn record_affinity_success(&self, affinity_key: &str, node_id: NodeId);
    fn pool_stats(&self) -> PoolStats;
    fn budget_details(&self) -> Vec<serde_json::Value>;
    fn node_budget_detail(&self, node_id: &str) -> Option<serde_json::Value>;
    fn fuse_all(&self);
    fn unfuse_all(&self);
    fn add_node(&self, url: &str);
    fn remove_node(&self, node_id: &str);
    fn probe_node(&self, node_id: &str) -> Option<ProbeResult>;
    fn recover_node(&self, node_id: &str);
    fn probe_all(&self);
    fn probe_dead_adaptive(&self);
    fn runtime_details(&self) -> serde_json::Value {
        serde_json::json!({})
    }
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
