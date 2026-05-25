use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use crate::pool::{NodeId, NodeRef};

pub struct TransportRegistry {
    clients: RwLock<HashMap<NodeId, reqwest::Client>>,
    direct_client: Mutex<Option<reqwest::Client>>,
}

impl TransportRegistry {
    pub fn new() -> Self {
        Self {
            clients: RwLock::new(HashMap::new()),
            direct_client: Mutex::new(None),
        }
    }

    pub fn client_for_node(&self, node: &NodeRef) -> reqwest::Client {
        let mut map = self.clients.write().unwrap();
        map.entry(node.id.clone())
            .or_insert_with(|| Self::make_socks_client(&node.url))
            .clone()
    }

    pub fn direct_client(&self) -> reqwest::Client {
        let mut client = self.direct_client.lock().unwrap();
        if client.is_none() {
            *client = Some(
                reqwest::Client::builder()
                    .no_proxy()
                    .connect_timeout(Duration::from_secs(30))
                    .timeout(Duration::from_secs(120))
                    .build()
                    .unwrap(),
            );
        }
        client.as_ref().cloned().unwrap()
    }

    fn make_socks_client(socks5_url: &str) -> reqwest::Client {
        reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(socks5_url).unwrap())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap()
    }
}

impl Default for TransportRegistry {
    fn default() -> Self {
        Self::new()
    }
}
