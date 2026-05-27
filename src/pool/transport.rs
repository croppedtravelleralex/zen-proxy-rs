use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use crate::pool::{NodeId, NodeRef};

#[derive(Debug, Clone)]
pub struct TransportRegistrySnapshot {
    pub node_client_count: usize,
    pub direct_client_initialized: bool,
    pub connect_timeout_secs: u64,
    pub request_timeout_secs: u64,
}

pub struct TransportRegistry {
    clients: RwLock<HashMap<NodeId, reqwest::Client>>,
    direct_client: Mutex<Option<reqwest::Client>>,
    connect_timeout: Duration,
    request_timeout: Duration,
}

impl TransportRegistry {
    pub fn new(connect_timeout: Duration, request_timeout: Duration) -> Self {
        Self {
            clients: RwLock::new(HashMap::new()),
            direct_client: Mutex::new(None),
            connect_timeout,
            request_timeout,
        }
    }

    pub fn client_for_node(&self, node: &NodeRef) -> reqwest::Client {
        let mut map = self.clients.write().unwrap();
        map.entry(node.id.clone())
            .or_insert_with(|| {
                Self::make_socks_client(&node.url, self.connect_timeout, self.request_timeout)
            })
            .clone()
    }

    pub fn direct_client(&self) -> reqwest::Client {
        let mut client = self.direct_client.lock().unwrap();
        if client.is_none() {
            *client = Some(
                reqwest::Client::builder()
                    .no_proxy()
                    .connect_timeout(self.connect_timeout)
                    .timeout(self.request_timeout)
                    .build()
                    .unwrap(),
            );
        }
        client.as_ref().cloned().unwrap()
    }

    pub fn remove_client(&self, node_id: &str) -> bool {
        self.clients.write().unwrap().remove(node_id).is_some()
    }

    pub fn snapshot(&self) -> TransportRegistrySnapshot {
        TransportRegistrySnapshot {
            node_client_count: self.clients.read().unwrap().len(),
            direct_client_initialized: self.direct_client.lock().unwrap().is_some(),
            connect_timeout_secs: self.connect_timeout.as_secs(),
            request_timeout_secs: self.request_timeout.as_secs(),
        }
    }

    fn make_socks_client(
        socks5_url: &str,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> reqwest::Client {
        reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(socks5_url).unwrap())
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .build()
            .unwrap()
    }
}

impl Default for TransportRegistry {
    fn default() -> Self {
        Self::new(Duration::from_secs(5), Duration::from_secs(120))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_exposes_configured_timeouts_for_clients() {
        let registry = TransportRegistry::new(Duration::from_secs(7), Duration::from_secs(333));
        let snapshot = registry.snapshot();

        assert_eq!(snapshot.connect_timeout_secs, 7);
        assert_eq!(snapshot.request_timeout_secs, 333);
    }

    #[test]
    fn remove_client_drops_cached_node_client() {
        let registry = TransportRegistry::new(Duration::from_secs(1), Duration::from_secs(2));
        let node = NodeRef::new("socks5h://user:pass@127.0.0.1:1080".to_string());

        let _client = registry.client_for_node(&node);
        assert_eq!(registry.snapshot().node_client_count, 1);
        assert!(registry.remove_client(&node.id));
        assert_eq!(registry.snapshot().node_client_count, 0);
        assert!(!registry.remove_client(&node.id));
    }
}
