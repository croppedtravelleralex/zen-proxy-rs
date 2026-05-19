use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Duration;

use crate::collector::{DataCollector, ProbeEvent};
use crate::pool::probe_period::ProbePeriod;
use crate::pool::*;

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
    nodes: RwLock<HashMap<NodeId, NodeRef>>,
    clients: RwLock<HashMap<NodeId, reqwest::Client>>,
    upstream_base: String,
    upstream_api_key: String,
    probe_timeout_secs: u64,
    allow_direct_fallback: bool,
    direct_client: std::sync::Mutex<Option<reqwest::Client>>,
}

impl<D, A, R, K> PoolManagerImpl<D, A, R, K>
where
    D: Pool,
    A: Pool,
    R: RateLimitedPool,
    K: DeadPool,
{
    pub fn new(
        dispatch: Arc<D>,
        active: Arc<A>,
        ratelimited: Arc<R>,
        dead: Arc<K>,
        collector: Arc<dyn DataCollector>,
        upstream_base: String,
        upstream_api_key: String,
        probe_timeout_secs: u64,
        allow_direct_fallback: bool,
    ) -> Self {
        Self {
            dispatch,
            active,
            ratelimited,
            dead,
            collector,
            fuse: AtomicBool::new(false),
            nodes: RwLock::new(HashMap::new()),
            clients: RwLock::new(HashMap::new()),
            upstream_base,
            upstream_api_key,
            probe_timeout_secs,
            allow_direct_fallback,
            direct_client: std::sync::Mutex::new(None),
        }
    }

    fn make_client(socks5_url: &str) -> reqwest::Client {
        reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(socks5_url).unwrap())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap()
    }

    fn get_or_create_client(&self, node_id: &NodeId, url: &str) -> reqwest::Client {
        let mut map = self.clients.write().unwrap();
        map.entry(node_id.clone())
            .or_insert_with(|| Self::make_client(url))
            .clone()
    }
}

impl<D, A, R, K> PoolManager for PoolManagerImpl<D, A, R, K>
where
    D: Pool + 'static,
    A: Pool + 'static,
    R: RateLimitedPool + 'static,
    K: DeadPool + 'static,
{
    fn dispatch(&self, _req: &RequestMeta) -> Result<DispatchResult, DispatchError> {
        if self.fuse.load(Ordering::Acquire) {
            return Err(DispatchError::NoResource);
        }
        let node = self
            .dispatch
            .acquire()
            .ok_or(DispatchError::NoResource)
            .or_else(|_| {
                if self.allow_direct_fallback {
                    let mut dc = self.direct_client.lock().unwrap();
                    if dc.is_none() {
                        *dc = Some(
                            reqwest::Client::builder()
                                .no_proxy()
                                .connect_timeout(Duration::from_secs(30))
                                .timeout(Duration::from_secs(120))
                                .build()
                                .unwrap(),
                        );
                    }
                    let direct_id = "direct".to_string();
                    Ok(NodeRef {
                        id: direct_id,
                        url: "direct".to_string(),
                    })
                } else {
                    Err(DispatchError::NoResource)
                }
            })?;
        self.active.add(node.clone());

        {
            let mut all = self.nodes.write().unwrap();
            if !all.contains_key(&node.id) {
                all.insert(node.id.clone(), node.clone());
            }
        }

        let url = node.url.clone();
        let client = if node.id == "direct" {
            self.direct_client
                .lock()
                .unwrap()
                .as_ref()
                .cloned()
                .unwrap()
        } else {
            self.get_or_create_client(&node.id, &url)
        };

        Ok(DispatchResult { node, client, url })
    }

    fn report(&self, node_id: NodeId, result: ResultKind, _latency_us: u64) {
        match result {
            ResultKind::Success(_) => {
                self.active.release(&node_id, &result);
                self.dispatch.release(&node_id, &result);
            }
            ResultKind::RateLimited => {
                self.ratelimited.quarantine(node_id.clone());
                self.active.release(&node_id, &result);
                self.dispatch.release(&node_id, &result);
                self.dispatch.remove(&node_id);

                if let Some(nr) = self.nodes.read().unwrap().get(&node_id).cloned() {
                    let ratelimited = self.ratelimited.clone();
                    let dispatch = self.dispatch.clone();
                    let collector = self.collector.clone();
                    let client = self.get_or_create_client(&node_id, &nr.url);
                    let upstream = self.upstream_base.clone();
                    let timeout = self.probe_timeout_secs;
                    let api_key = self.upstream_api_key.clone();
                    let nid = node_id.clone();

                    tokio::spawn(async move {
                        let ok = ProbePeriod::probe_node(&client, &nr, &upstream, timeout, &api_key).await;

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
            ResultKind::Error { .. } => {
                self.active.release(&node_id, &result);
                self.dispatch.release(&node_id, &result);
                self.dispatch.remove(&node_id);

                if let Some(nr) = self.nodes.read().unwrap().get(&node_id).cloned() {
                    let dispatch = self.dispatch.clone();
                    let dead = self.dead.clone();
                    let collector = self.collector.clone();
                    let client = self.get_or_create_client(&node_id, &nr.url);
                    let upstream = self.upstream_base.clone();
                    let timeout = self.probe_timeout_secs;
                    let api_key = self.upstream_api_key.clone();
                    let nid = node_id.clone();

                    tokio::spawn(async move {
                        let ok = ProbePeriod::probe_node(&client, &nr, &upstream, timeout, &api_key).await;

                        if ok {
                            dispatch.add(NodeRef {
                                id: nid.clone(),
                                url: nr.url.clone(),
                            });
                            dispatch.release(&nid, &ResultKind::Success(200));
                        } else {
                            dead.bury(nid.clone());
                        }

                        collector.record_probe(&ProbeEvent {
                            ts: chrono::Utc::now().timestamp(),
                            node_id: nid,
                            pool: "error_probe".to_string(),
                            ok,
                            latency_ms: 0,
                        });
                    });
                }
            }
        }
    }

    fn pool_stats(&self) -> PoolStats {
        PoolStats {
            dispatch_size: self.dispatch.available(),
            active_size: self.active.available(),
            ratelimited_size: self.ratelimited.available(),
            dead_size: self.dead.available(),
            pool_transitions: 0,
            active_concurrency: self.active.available(),
            fuse: self.fuse.load(Ordering::Acquire),
        }
    }

    fn fuse_all(&self) {
        self.fuse.store(true, Ordering::Release);
        let ids: Vec<NodeId> = self.nodes.read().unwrap().keys().cloned().collect();
        for id in &ids {
            self.dispatch.remove(id);
            self.dead.bury(id.clone());
        }
    }

    fn unfuse_all(&self) {
        self.fuse.store(false, Ordering::Release);
        let dead_ids = self.dead.select_all_for_probe();
        let nodes = self.nodes.read().unwrap();
        for id in &dead_ids {
            if let Some(nr) = nodes.get(id) {
                self.dispatch.add(nr.clone());
                self.dead.recover(id);
            }
        }
    }
}
