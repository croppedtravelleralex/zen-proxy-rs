pub mod active;
pub mod dead;
pub mod dispatch;
pub mod global_budget;
pub mod manager;
pub mod node_registry;
pub mod probe_period;
pub mod ratelimited;
pub mod session_pin;
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
    ClientGone,
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
    pub session_pin_hit: bool,
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
    pub upstream_model: String,
    pub session_id: String,
    pub stream: bool,
    pub body_size: u64,
    pub affinity_key: String,
    pub allow_direct_fallback: bool,
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
    /// Acquire a node, skipping any node whose id appears in `exclude`.
    ///
    /// Implementations that do not override this method fall back to the
    /// standard `acquire_for`, which is correct for pools that do not perform
    /// node selection (e.g. active/dead/ratelimited pools).
    fn acquire_for_excluding(&self, meta: &RequestMeta, exclude: &[NodeId]) -> Option<NodeRef> {
        let _ = exclude;
        self.acquire_for(meta)
    }
    /// Acquire an affinity node, skipping candidates whose id appears in `exclude`.
    ///
    /// Implementations that do not override this method fall back to
    /// `try_acquire_affinity`, which is correct for pools without affinity tracking.
    fn try_acquire_affinity_excluding(
        &self,
        meta: &RequestMeta,
        exclude: &[NodeId],
    ) -> Result<(NodeRef, NodeId), DispatchError> {
        let _ = exclude;
        self.try_acquire_affinity(meta)
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
    /// Dispatch a request while excluding specific nodes from selection.
    ///
    /// Callers pass node ids that have already failed for this request (e.g.
    /// returned empty output) so retries land on a different proxy node.
    ///
    /// The default implementation ignores `exclude` and delegates to `dispatch`,
    /// preserving backwards compatibility for existing `PoolManager` implementations.
    fn dispatch_excluding(
        &self,
        req: &RequestMeta,
        exclude: &[NodeId],
    ) -> Result<DispatchResult, DispatchError> {
        let _ = exclude;
        self.dispatch(req)
    }
    fn dispatch_direct(&self) -> Result<DispatchResult, DispatchError>;
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
    fn probe_ratelimited_periodic(&self);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn meta(body_size: u64) -> RequestMeta {
        RequestMeta {
            model: "deepseek-v4-flash".to_string(),
            upstream_model: "deepseek-v4-flash-free".to_string(),
            session_id: "sess".to_string(),
            stream: false,
            body_size,
            affinity_key: String::new(),
            allow_direct_fallback: true,
        }
    }

    /// A pool that implements only the required methods, so every call below
    /// exercises the trait's default implementations.
    struct BarePool {
        nodes: Mutex<Vec<NodeRef>>,
    }

    impl BarePool {
        fn with_node(url: &str) -> Self {
            Self {
                nodes: Mutex::new(vec![NodeRef::new(url.to_string())]),
            }
        }
    }

    impl Pool for BarePool {
        fn acquire(&self) -> Option<NodeRef> {
            self.nodes.lock().unwrap().first().cloned()
        }
        fn release(&self, _node_id: &NodeId, _result: &ResultKind) {}
        fn remove(&self, node_id: &NodeId) {
            self.nodes.lock().unwrap().retain(|n| &n.id != node_id);
        }
        fn add(&self, node: NodeRef) {
            self.nodes.lock().unwrap().push(node);
        }
        fn available(&self) -> usize {
            self.nodes.lock().unwrap().len()
        }
        fn name(&self) -> &'static str {
            "bare"
        }
    }

    /// Pins the documented contract of the default `_excluding` methods: they IGNORE
    /// `exclude` and delegate to the non-excluding variant. Any pool that actually
    /// performs node selection must override them, otherwise a caller asking to avoid
    /// a failed node silently gets it back. `DispatchPool` overrides both.
    #[test]
    fn default_excluding_methods_ignore_the_exclusion_list() {
        let pool = BarePool::with_node("socks5h://user:pass@127.0.0.1:1080");
        let only_node = pool.acquire().expect("seeded node");

        let selected = pool
            .acquire_for_excluding(&meta(1024), std::slice::from_ref(&only_node.id))
            .expect("default impl ignores exclude and still returns the node");
        assert_eq!(selected.id, only_node.id);

        // Default affinity has no tracking, so it reports NoResource either way.
        assert_eq!(
            pool.try_acquire_affinity_excluding(&meta(1024), std::slice::from_ref(&only_node.id))
                .err(),
            Some(DispatchError::NoResource)
        );
    }

    #[test]
    fn pool_trait_defaults_are_inert_but_callable() {
        let pool = BarePool::with_node("socks5h://user:pass@127.0.0.1:1081");
        let node = pool.acquire().expect("seeded node");

        assert!(pool.preflight(&meta(1024)).is_ok());
        assert_eq!(
            pool.acquire_for(&meta(1024)).map(|n| n.id),
            Some(node.id.clone())
        );
        assert_eq!(pool.budget_counts(), (0, 0, 0));
        assert!(pool.budget_details().is_empty());
        assert!(pool.node_budget_detail(&node.id).is_none());
        assert_eq!(
            pool.try_acquire_sticky(&meta(1024), &node.id).err(),
            Some(DispatchError::NoResource)
        );
        assert_eq!(
            pool.try_acquire_affinity(&meta(1024)).err(),
            Some(DispatchError::NoResource)
        );

        // Smoke the no-op recorders: they must not panic or mutate the pool.
        pool.release_with_latency(&node.id, &ResultKind::Success(200), 12);
        pool.record_latency_hint(&node.id, 12);
        pool.record_bucket_latency_hint(&node.id, "tiny", 12);
        pool.record_affinity_success("key", &node.id);
        assert_eq!(pool.available(), 1);
        assert_eq!(pool.name(), "bare");

        pool.remove(&node.id);
        assert_eq!(pool.available(), 0);
    }

    #[test]
    fn request_meta_derives_size_buckets_from_body_size() {
        // estimated_input_tokens is body_size/4, floored at 1.
        assert_eq!(meta(0).estimated_input_tokens(), 1);
        assert_eq!(meta(4_000).estimated_input_tokens(), 1_000);
        assert_eq!(meta(1024).request_kb(), 1);
        assert_eq!(meta(1025).request_kb(), 2);
        assert_eq!(meta(0).request_kb(), 1);
        assert_eq!(meta(1024).body_size_bucket(), "tiny");
        assert_eq!(meta(4_000).token_bucket(), "under_50k");
        // body_size/4 => 200_000 estimated tokens
        assert_eq!(meta(800_000).token_bucket(), "200k_400k");
        assert_eq!(meta(600_000).token_bucket(), "100k_200k");
    }

    #[test]
    fn body_size_buckets_cover_every_boundary() {
        assert_eq!(body_size_bucket(0), "tiny");
        assert_eq!(body_size_bucket(131_071), "tiny");
        assert_eq!(body_size_bucket(131_072), "small");
        assert_eq!(body_size_bucket(262_143), "small");
        assert_eq!(body_size_bucket(262_144), "medium");
        assert_eq!(body_size_bucket(524_287), "medium");
        assert_eq!(body_size_bucket(524_288), "large");
        assert_eq!(body_size_bucket(1_048_575), "large");
        assert_eq!(body_size_bucket(1_048_576), "huge");
    }

    #[test]
    fn token_buckets_cover_every_boundary() {
        assert_eq!(token_bucket(0), "under_50k");
        assert_eq!(token_bucket(49_999), "under_50k");
        assert_eq!(token_bucket(50_000), "50k_100k");
        assert_eq!(token_bucket(99_999), "50k_100k");
        assert_eq!(token_bucket(100_000), "100k_200k");
        assert_eq!(token_bucket(199_999), "100k_200k");
        assert_eq!(token_bucket(200_000), "200k_400k");
        assert_eq!(token_bucket(399_999), "200k_400k");
        assert_eq!(token_bucket(400_000), "400k_plus");
    }

    #[test]
    fn pool_stats_total_sums_every_pool() {
        let stats = PoolStats {
            dispatch_size: 3,
            active_size: 2,
            ratelimited_size: 1,
            dead_size: 4,
            pool_transitions: 0,
            active_concurrency: 2,
            fuse: false,
            cooldown_size: 0,
            budget_limited_size: 0,
            leased_count: 0,
        };
        assert_eq!(stats.total(), 10);
    }

    #[test]
    fn node_ref_id_is_a_stable_hash_of_the_url() {
        let url = "socks5h://user:pass@127.0.0.1:1080".to_string();
        let a = NodeRef::new(url.clone());
        let b = NodeRef::new(url.clone());
        assert_eq!(a.id, b.id);
        assert_eq!(a.id, sha256_first8(&url));
        assert_ne!(a.id, NodeRef::new("socks5h://other:1081".to_string()).id);
        // The id must never leak the embedded credentials.
        assert!(!a.id.contains("pass"));
    }
}
