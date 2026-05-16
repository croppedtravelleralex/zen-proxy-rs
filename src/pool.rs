use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

use reqwest::{Client, Proxy};

/// Manages a pool of reqwest HTTP clients, each optionally tunneled through a SOCKS5 proxy.
///
/// Ported from  SessionPool (lines 505-568).
pub struct SessionPool {
    clients: RwLock<HashMap<String, Client>>,
    direct: Client,
    pool_max_size: u32,
    timeout_secs: u64,
    connect_timeout_secs: u64,
}

impl SessionPool {
    /// Creates a new .
    ///
    /// The direct (no-proxy) client is built immediately. Per-node SOCKS5 clients
    /// are created on demand via [].
    pub fn new(pool_max_size: u32, timeout_secs: u64, connect_timeout_secs: u64) -> Self {
        let timeout = Duration::from_secs(timeout_secs);
        let connect_timeout = Duration::from_secs(connect_timeout_secs);

        Self {
            clients: RwLock::new(HashMap::new()),
            direct: build_direct_client(pool_max_size, timeout, connect_timeout),
            pool_max_size,
            timeout_secs,
            connect_timeout_secs,
        }
    }

    /// Returns a reqwest [] for the given node URL.
    ///
    /// *  -- returns a cached or freshly-built SOCKS5 client for that proxy.
    /// *              -- returns the built-in direct client.
    pub fn get_client(&self, node_url: Option<&str>) -> Client {
        match node_url {
            Some(url) => {
                // Fast path -- read without locking for write.
                if let Ok(map) = self.clients.read() {
                    if let Some(client) = map.get(url) {
                        return client.clone();
                    }
                }

                // Slow path -- build and cache.
                let timeout = Duration::from_secs(self.timeout_secs);
                let connect_timeout = Duration::from_secs(self.connect_timeout_secs);

                let client = build_socks5_client(url, self.pool_max_size, timeout, connect_timeout);

                if let Ok(mut map) = self.clients.write() {
                    map.insert(url.to_string(), client.clone());
                }

                client
            }
            None => self.direct.clone(),
        }
    }

    /// Drops all cached per-node clients.
    ///
    /// The direct client is intentionally kept alive.
    pub fn close(&self) {
        if let Ok(mut map) = self.clients.write() {
            map.clear();
        }
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Builds an HTTP client that routes through a SOCKS5 proxy.
fn build_socks5_client(
    socks5_url: &str,
    pool_max_size: u32,
    timeout: Duration,
    connect_timeout: Duration,
) -> Client {
    let url_owned = socks5_url.to_owned();
    let proxy = Proxy::custom(move |_| {
        url_owned.parse::<reqwest::Url>().ok()
    });

    Client::builder()
        .proxy(proxy)
        .danger_accept_invalid_certs(true)
        .pool_max_idle_per_host(pool_max_size as usize)
        .user_agent("Zen-Proxy-RS/1.0")
        .timeout(timeout)
        .connect_timeout(connect_timeout)
        .build()
        .expect("Failed to build SOCKS5 reqwest client")
}

/// Builds a plain HTTP client with no proxy configured.
fn build_direct_client(
    pool_max_size: u32,
    timeout: Duration,
    connect_timeout: Duration,
) -> Client {
    Client::builder()
        .danger_accept_invalid_certs(true)
        .pool_max_idle_per_host(pool_max_size as usize)
        .user_agent("Zen-Proxy-RS/1.0")
        .timeout(timeout)
        .connect_timeout(connect_timeout)
        .build()
        .expect("Failed to build direct reqwest client")
}
