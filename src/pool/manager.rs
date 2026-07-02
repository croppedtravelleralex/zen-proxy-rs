use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::collector::{DataCollector, ProbeEvent};
use crate::pool::node_registry::NodeRegistry;
use crate::pool::probe_period::ProbePeriod;
use crate::pool::transport::TransportRegistry;
use crate::pool::*;
use crate::v4::contracts::{DeadNodeState, DeadProbePolicy};
use crate::v4::dead_probe::AdaptiveDeadProbePolicy;

const DIRECT_NODE_ID: &str = "direct";
const DIRECT_NODE_URL: &str = "direct";

pub struct PoolManagerImpl<D, A, R, K>
where
    D: Pool,
    A: Pool,
    R: RateLimitedPool,
    K: DeadPool,
{
    dispatch: Arc<D>,
    active: Arc<A>,
    ratelimited: Arc<R>,
    dead: Arc<K>,
    collector: Arc<dyn DataCollector>,
    fuse: AtomicBool,
    nodes: NodeRegistry,
    transport: TransportRegistry,
    upstream_base: String,
    upstream_api_key: String,
    probe_timeout_secs: u64,
    allow_direct_fallback: bool,
}

impl<D, A, R, K> PoolManagerImpl<D, A, R, K>
where
    D: Pool,
    A: Pool,
    R: RateLimitedPool,
    K: DeadPool,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        dispatch: Arc<D>,
        active: Arc<A>,
        ratelimited: Arc<R>,
        dead: Arc<K>,
        collector: Arc<dyn DataCollector>,
        upstream_base: String,
        upstream_api_key: String,
        probe_timeout_secs: u64,
        connect_timeout: Duration,
        request_timeout: Duration,
        allow_direct_fallback: bool,
    ) -> Self {
        Self {
            dispatch,
            active,
            ratelimited,
            dead,
            collector,
            fuse: AtomicBool::new(false),
            nodes: NodeRegistry::new(),
            transport: TransportRegistry::new(connect_timeout, request_timeout),
            upstream_base,
            upstream_api_key,
            probe_timeout_secs,
            allow_direct_fallback,
        }
    }

    pub fn register_known_node(&self, node: NodeRef) {
        self.nodes.insert(node);
    }
}

impl<D, A, R, K> PoolManager for PoolManagerImpl<D, A, R, K>
where
    D: Pool + 'static,
    A: Pool + 'static,
    R: RateLimitedPool + 'static,
    K: DeadPool + 'static,
{
    fn dispatch(&self, req: &RequestMeta) -> Result<DispatchResult, DispatchError> {
        if self.fuse.load(Ordering::Acquire) {
            return Err(DispatchError::NoResource);
        }
        self.dispatch.preflight(req)?;
        let (node, affinity_hit, affinity_node_id) = self
            .dispatch
            .try_acquire_affinity(req)
            .map(|(node, affinity_node_id)| (node, true, affinity_node_id))
            .or_else(|_| {
                self.dispatch
                    .acquire_for(req)
                    .map(|node| (node, false, String::new()))
                    .ok_or(DispatchError::NoResource)
            })
            .or_else(|_| {
                if self.allow_direct_fallback && req.allow_direct_fallback {
                    Ok((
                        NodeRef {
                            id: DIRECT_NODE_ID.to_string(),
                            url: DIRECT_NODE_URL.to_string(),
                        },
                        false,
                        String::new(),
                    ))
                } else {
                    Err(DispatchError::NoResource)
                }
            })?;

        if node.id != DIRECT_NODE_ID {
            self.active.add(node.clone());

            self.nodes.insert(node.clone());
        }

        let url = node.url.clone();
        let client = if node.id == "direct" {
            self.transport.direct_client()
        } else {
            self.transport.client_for_node(&node)
        };

        Ok(DispatchResult {
            node,
            client,
            url,
            affinity_hit,
            affinity_node_id,
        })
    }

    fn dispatch_direct(&self) -> Result<DispatchResult, DispatchError> {
        if self.fuse.load(Ordering::Acquire) || !self.allow_direct_fallback {
            return Err(DispatchError::NoResource);
        }

        let node = NodeRef {
            id: DIRECT_NODE_ID.to_string(),
            url: DIRECT_NODE_URL.to_string(),
        };

        Ok(DispatchResult {
            node,
            client: self.transport.direct_client(),
            url: DIRECT_NODE_URL.to_string(),
            affinity_hit: false,
            affinity_node_id: String::new(),
        })
    }

    fn dispatch_sticky(
        &self,
        meta: &RequestMeta,
        node_id: &str,
    ) -> Result<DispatchResult, DispatchError> {
        if self.fuse.load(Ordering::Acquire) {
            return Err(DispatchError::NoResource);
        }
        self.dispatch.preflight(meta)?;
        if node_id == DIRECT_NODE_ID {
            return self.dispatch_direct();
        }

        // 先尝试粘滞获取指定节点
        let nid: NodeId = node_id.to_string();
        if let Ok(node) = self.dispatch.try_acquire_sticky(meta, &nid) {
            self.active.add(node.clone());
            self.nodes.insert(node.clone());
            let url = node.url.clone();
            let client = self.transport.client_for_node(&node);
            return Ok(DispatchResult {
                node,
                client,
                url,
                affinity_hit: false,
                affinity_node_id: String::new(),
            });
        }
        // 回退到普通 dispatch
        self.dispatch(meta)
    }

    fn report(&self, node_id: NodeId, result: ResultKind, latency_ms: u64) {
        if node_id == DIRECT_NODE_ID {
            return;
        }

        match result {
            ResultKind::Success(_) => {
                self.active.release(&node_id, &result);
                self.dispatch
                    .release_with_latency(&node_id, &result, latency_ms);
            }
            ResultKind::RateLimited => {
                self.ratelimited.quarantine(node_id.clone());
                self.active.release(&node_id, &result);
                self.dispatch
                    .release_with_latency(&node_id, &result, latency_ms);
                self.dispatch.remove(&node_id);

                if let Some(nr) = self.nodes.get(&node_id) {
                    let ratelimited = self.ratelimited.clone();
                    let dispatch = self.dispatch.clone();
                    let collector = self.collector.clone();
                    let client = self.transport.client_for_node(&nr);
                    let upstream = self.upstream_base.clone();
                    let timeout = self.probe_timeout_secs;
                    let api_key = self.upstream_api_key.clone();
                    let nid = node_id.clone();

                    tokio::spawn(async move {
                        let ok =
                            ProbePeriod::probe_node(&client, &nr, &upstream, timeout, &api_key)
                                .await;

                        if ok {
                            ratelimited.recover(&nid);
                            dispatch.add(NodeRef {
                                id: nid.clone(),
                                url: nr.url.clone(),
                            });
                            dispatch.release(&nid, &ResultKind::Success(200));
                        }

                        collector.record_probe(&ProbeEvent {
                            ts: chrono::Utc::now().timestamp(),
                            node_id: nid,
                            pool: "ratelimited_probe".to_string(),
                            ok,
                            latency_ms: 0,
                        });
                    });
                }
            }
            ResultKind::EmptyOutput => {
                self.active.release(&node_id, &result);
                self.dispatch
                    .release_with_latency(&node_id, &result, latency_ms);
            }
            ResultKind::ClientGone => {
                self.active.release(&node_id, &result);
                self.dispatch
                    .release_with_latency(&node_id, &result, latency_ms);
            }
            ResultKind::SoftFailure { .. } => {
                self.active.release(&node_id, &result);
                self.dispatch
                    .release_with_latency(&node_id, &result, latency_ms);
            }
            ResultKind::Error { .. } => {
                self.active.release(&node_id, &result);
                self.dispatch
                    .release_with_latency(&node_id, &result, latency_ms);
                self.dispatch.remove(&node_id);
                if let Some(node) = self.nodes.get(&node_id) {
                    self.dead.add(node);
                }
                self.dead.bury(node_id);
            }
        }
    }

    fn record_latency_hint(&self, node_id: NodeId, latency_ms: u64) {
        if node_id == DIRECT_NODE_ID {
            return;
        }
        self.dispatch.record_latency_hint(&node_id, latency_ms);
    }

    fn record_bucket_latency_hint(&self, node_id: NodeId, bucket: &str, latency_ms: u64) {
        if node_id == DIRECT_NODE_ID {
            return;
        }
        self.dispatch
            .record_bucket_latency_hint(&node_id, bucket, latency_ms);
    }

    fn record_affinity_success(&self, affinity_key: &str, node_id: NodeId) {
        if node_id == DIRECT_NODE_ID {
            return;
        }
        self.dispatch
            .record_affinity_success(affinity_key, &node_id);
    }

    fn pool_stats(&self) -> PoolStats {
        let (cooldown_size, budget_limited_size, leased_count) = self.dispatch.budget_counts();
        PoolStats {
            dispatch_size: self.dispatch.available(),
            active_size: self.active.available(),
            ratelimited_size: self.ratelimited.available(),
            dead_size: self.dead.available(),
            pool_transitions: 0,
            active_concurrency: self.active.available(),
            fuse: self.fuse.load(Ordering::Acquire),
            cooldown_size,
            budget_limited_size,
            leased_count,
        }
    }

    fn budget_details(&self) -> Vec<serde_json::Value> {
        self.dispatch.budget_details()
    }

    fn node_budget_detail(&self, node_id: &str) -> Option<serde_json::Value> {
        self.dispatch.node_budget_detail(&node_id.to_string())
    }

    fn fuse_all(&self) {
        self.fuse.store(true, Ordering::Release);
        let ids = self.nodes.ids();
        for id in &ids {
            self.dispatch.remove(id);
            self.dead.bury(id.clone());
        }
    }

    fn unfuse_all(&self) {
        self.fuse.store(false, Ordering::Release);
        let dead_ids = self.dead.select_all_for_probe();
        for id in &dead_ids {
            if let Some(nr) = self.nodes.get(id) {
                self.dispatch.add(nr.clone());
                self.dead.recover(id);
            }
        }
    }

    fn add_node(&self, url: &str) {
        let nr = NodeRef::new(url.to_string());
        self.dispatch.add(nr.clone());
        self.nodes.insert(nr);
    }

    fn remove_node(&self, node_id: &str) {
        let nid = node_id.to_string();
        self.dispatch.remove(&nid);
        self.active.remove(&nid);
        self.ratelimited.remove(&nid);
        self.dead.remove(&nid);
        self.nodes.remove(&nid);
        self.transport.remove_client(&nid);
    }

    fn probe_node(&self, node_id: &str) -> Option<ProbeResult> {
        let nid = node_id.to_string();
        let nr = self.nodes.get(&nid)?;
        let client = self.transport.client_for_node(&nr);
        let upstream = self.upstream_base.clone();
        let timeout = self.probe_timeout_secs;
        let api_key = self.upstream_api_key.clone();
        let start = std::time::Instant::now();
        let ok = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                ProbePeriod::probe_node(&client, &nr, &upstream, timeout, &api_key).await
            })
        });
        Some(ProbeResult {
            success: ok,
            latency_ms: start.elapsed().as_millis() as u64,
        })
    }

    fn recover_node(&self, node_id: &str) {
        let nid = node_id.to_string();
        if let Some(nr) = self.nodes.get(&nid) {
            self.dead.recover(&nid);
            self.ratelimited.recover(&nid);
            if self.fuse.load(Ordering::Acquire) {
                self.dispatch.remove(&nid);
            } else {
                self.dispatch.add(nr.clone());
                self.dispatch.release(&nid, &ResultKind::Success(200));
            }
        }
    }

    fn probe_all(&self) {
        let ids = self.nodes.ids();
        for id in ids {
            let _ = self.probe_node(&id);
        }
    }

    fn probe_dead_adaptive(&self) {
        let policy = AdaptiveDeadProbePolicy::default();
        let ids = self.dead.select_all_for_probe();
        let dead_count = ids.len();
        let batch_size = policy.next_batch_size(dead_count, 0.0);
        if batch_size == 0 {
            return;
        }

        let due_ids = ids
            .into_iter()
            .filter(|id| {
                let dead_count = self.dead.dead_count(id);
                let state = DeadNodeState {
                    node_id: id.clone(),
                    dead_count,
                    last_probe_ts_ms: None,
                    recent_recovery_rate: 0.0,
                };
                let delay = policy.next_delay_secs(&state);
                let dead_age = self.dead.dead_age_secs(id).unwrap_or(0);
                let probe_age = self.dead.last_probe_age_secs(id);
                dead_age >= delay && probe_age.is_none_or(|age| age >= delay)
            })
            .take(batch_size)
            .collect::<Vec<_>>();

        for id in due_ids {
            let Some(nr) = self.nodes.get(&id) else {
                continue;
            };
            let dead = self.dead.clone();
            let dispatch = self.dispatch.clone();
            let ratelimited = self.ratelimited.clone();
            let collector = self.collector.clone();
            let client = self.transport.client_for_node(&nr);
            let upstream = self.upstream_base.clone();
            let timeout = self.probe_timeout_secs;
            let api_key = self.upstream_api_key.clone();
            let fuse_is_open = self.fuse.load(Ordering::Acquire);

            tokio::spawn(async move {
                let start = std::time::Instant::now();
                let ok = ProbePeriod::probe_node(&client, &nr, &upstream, timeout, &api_key).await;
                let latency_ms = start.elapsed().as_millis() as u64;
                let consecutive_successes = dead.record_probe_result(&id, ok);
                if AdaptiveDeadProbePolicy::recovery_proven(consecutive_successes, false) {
                    dead.recover(&id);
                    ratelimited.recover(&id);
                    if !fuse_is_open {
                        dispatch.add(nr.clone());
                        dispatch.release(&id, &ResultKind::Success(200));
                    }
                }
                collector.record_probe(&ProbeEvent {
                    ts: chrono::Utc::now().timestamp(),
                    node_id: id,
                    pool: "dead_probe_adaptive".to_string(),
                    ok,
                    latency_ms,
                });
            });
        }
    }

    fn runtime_details(&self) -> serde_json::Value {
        let transport = self.transport.snapshot();
        serde_json::json!({
            "node_registry": {
                "nodes": self.nodes.len(),
            },
            "transport": {
                "node_client_count": transport.node_client_count,
                "direct_client_initialized": transport.direct_client_initialized,
                "connect_timeout_secs": transport.connect_timeout_secs,
                "request_timeout_secs": transport.request_timeout_secs,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector::default::DefaultCollector;
    use crate::pool::active::ActivePool;
    use crate::pool::dead::DeadPoolImpl;
    use crate::pool::dispatch::DispatchPool;
    use crate::pool::ratelimited::RateLimitedPoolImpl;

    #[test]
    fn empty_output_does_not_move_node_to_dead_pool() {
        let dispatch = Arc::new(DispatchPool::new());
        let active = Arc::new(ActivePool::new());
        let ratelimited = Arc::new(RateLimitedPoolImpl::new());
        let dead = Arc::new(DeadPoolImpl::new());
        let collector = Arc::new(DefaultCollector::new());
        let manager = PoolManagerImpl::new(
            dispatch.clone(),
            active,
            ratelimited,
            dead.clone(),
            collector,
            "https://example.invalid".to_string(),
            "test".to_string(),
            1,
            Duration::from_secs(1),
            Duration::from_secs(120),
            false,
        );
        let node = NodeRef::new("socks5h://user:pass@127.0.0.1:1080".to_string());
        dispatch.add(node.clone());

        let meta = RequestMeta {
            model: "deepseek-v4-flash".to_string(),
            stream: false,
            body_size: 128,
            affinity_key: String::new(),
            allow_direct_fallback: true,
        };
        let dispatched = manager.dispatch(&meta).unwrap();
        manager.report(dispatched.node.id.clone(), ResultKind::EmptyOutput, 1500);

        assert_eq!(dead.available(), 0);
        assert_eq!(dispatch.available(), 1);
        assert!(manager.dispatch(&meta).is_ok());
    }

    #[test]
    fn upstream_soft_failure_does_not_move_node_to_dead_pool() {
        let dispatch = Arc::new(DispatchPool::new());
        let active = Arc::new(ActivePool::new());
        let ratelimited = Arc::new(RateLimitedPoolImpl::new());
        let dead = Arc::new(DeadPoolImpl::new());
        let collector = Arc::new(DefaultCollector::new());
        let manager = PoolManagerImpl::new(
            dispatch.clone(),
            active.clone(),
            ratelimited,
            dead.clone(),
            collector,
            "https://example.invalid".to_string(),
            "test".to_string(),
            1,
            Duration::from_secs(1),
            Duration::from_secs(120),
            false,
        );
        let node = NodeRef::new("socks5h://user:pass@127.0.0.1:1080".to_string());
        dispatch.add(node.clone());

        let meta = RequestMeta {
            model: "deepseek-v4-flash".to_string(),
            stream: true,
            body_size: 350_000,
            affinity_key: String::new(),
            allow_direct_fallback: true,
        };
        let dispatched = manager.dispatch(&meta).unwrap();
        manager.report(
            dispatched.node.id.clone(),
            ResultKind::SoftFailure {
                kind: ErrorKind::Upstream5xx,
            },
            30_000,
        );

        assert_eq!(active.available(), 0);
        assert_eq!(dead.available(), 0);
        assert_eq!(dispatch.available(), 1);
        assert!(manager.dispatch(&meta).is_ok());
    }

    #[test]
    fn hard_proxy_error_moves_node_to_dead_pool() {
        let dispatch = Arc::new(DispatchPool::new());
        let active = Arc::new(ActivePool::new());
        let ratelimited = Arc::new(RateLimitedPoolImpl::new());
        let dead = Arc::new(DeadPoolImpl::new());
        let collector = Arc::new(DefaultCollector::new());
        let manager = PoolManagerImpl::new(
            dispatch.clone(),
            active.clone(),
            ratelimited,
            dead.clone(),
            collector,
            "https://example.invalid".to_string(),
            "test".to_string(),
            1,
            Duration::from_secs(1),
            Duration::from_secs(120),
            false,
        );
        let node = NodeRef::new("socks5h://user:pass@127.0.0.1:1080".to_string());
        dispatch.add(node.clone());

        let meta = RequestMeta {
            model: "deepseek-v4-flash".to_string(),
            stream: true,
            body_size: 128,
            affinity_key: String::new(),
            allow_direct_fallback: true,
        };
        let dispatched = manager.dispatch(&meta).unwrap();
        manager.report(
            dispatched.node.id.clone(),
            ResultKind::Error {
                kind: ErrorKind::SocksHandshake,
            },
            100,
        );

        assert_eq!(active.available(), 0);
        assert_eq!(dead.available(), 1);
        assert_eq!(dispatch.available(), 0);
    }

    #[test]
    fn direct_dispatch_is_available_only_when_fallback_enabled() {
        let disabled = PoolManagerImpl::new(
            Arc::new(DispatchPool::new()),
            Arc::new(ActivePool::new()),
            Arc::new(RateLimitedPoolImpl::new()),
            Arc::new(DeadPoolImpl::new()),
            Arc::new(DefaultCollector::new()),
            "https://example.invalid".to_string(),
            "test".to_string(),
            1,
            Duration::from_secs(1),
            Duration::from_secs(120),
            false,
        );
        assert!(matches!(
            disabled.dispatch_direct(),
            Err(DispatchError::NoResource)
        ));

        let enabled = PoolManagerImpl::new(
            Arc::new(DispatchPool::new()),
            Arc::new(ActivePool::new()),
            Arc::new(RateLimitedPoolImpl::new()),
            Arc::new(DeadPoolImpl::new()),
            Arc::new(DefaultCollector::new()),
            "https://example.invalid".to_string(),
            "test".to_string(),
            1,
            Duration::from_secs(1),
            Duration::from_secs(120),
            true,
        );
        let direct = enabled.dispatch_direct().unwrap();
        assert_eq!(direct.node.id, "direct");
        assert_eq!(direct.url, "direct");
    }

    #[test]
    fn request_can_disable_direct_fallback_even_when_manager_allows_it() {
        let manager = PoolManagerImpl::new(
            Arc::new(DispatchPool::new()),
            Arc::new(ActivePool::new()),
            Arc::new(RateLimitedPoolImpl::new()),
            Arc::new(DeadPoolImpl::new()),
            Arc::new(DefaultCollector::new()),
            "https://example.invalid".to_string(),
            "test".to_string(),
            1,
            Duration::from_secs(1),
            Duration::from_secs(120),
            true,
        );
        let meta = RequestMeta {
            model: "mimo-v2.5".to_string(),
            stream: false,
            body_size: 128,
            affinity_key: String::new(),
            allow_direct_fallback: false,
        };

        assert!(matches!(
            manager.dispatch(&meta),
            Err(DispatchError::NoResource)
        ));
    }

    #[test]
    fn remove_node_reclaims_cached_transport_client() {
        let dispatch = Arc::new(DispatchPool::new());
        let active = Arc::new(ActivePool::new());
        let ratelimited = Arc::new(RateLimitedPoolImpl::new());
        let dead = Arc::new(DeadPoolImpl::new());
        let collector = Arc::new(DefaultCollector::new());
        let manager = PoolManagerImpl::new(
            dispatch.clone(),
            active,
            ratelimited,
            dead,
            collector,
            "https://example.invalid".to_string(),
            "test".to_string(),
            1,
            Duration::from_secs(2),
            Duration::from_secs(240),
            false,
        );
        let node = NodeRef::new("socks5h://user:pass@127.0.0.1:1080".to_string());
        dispatch.add(node.clone());
        manager.register_known_node(node.clone());

        let meta = RequestMeta {
            model: "deepseek-v4-flash".to_string(),
            stream: false,
            body_size: 128,
            affinity_key: String::new(),
            allow_direct_fallback: true,
        };
        let dispatched = manager.dispatch(&meta).unwrap();
        assert_eq!(
            manager.runtime_details()["transport"]["node_client_count"],
            serde_json::json!(1)
        );

        manager.remove_node(&dispatched.node.id);

        assert_eq!(
            manager.runtime_details()["transport"]["node_client_count"],
            serde_json::json!(0)
        );
        assert_eq!(
            manager.runtime_details()["transport"]["request_timeout_secs"],
            serde_json::json!(240)
        );
    }
}
