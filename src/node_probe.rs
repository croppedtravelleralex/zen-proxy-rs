use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};
use crate::selector::{ProxyNode, ProxySelector};

pub struct NodeProber {
    pub selector: Arc<ProxySelector>,
    pub connect_timeout: Duration,
}

impl NodeProber {
    pub fn new(selector: Arc<ProxySelector>, connect_timeout_secs: u64) -> Self {
        Self {
            selector,
            connect_timeout: Duration::from_secs(connect_timeout_secs),
        }
    }

    /// Probe a single node by TCP connect through its SOCKS5 address
    pub async fn probe_node(&self, node: Arc<ProxyNode>) -> bool {
        let url = node.url.clone();
        let addr = url
            .trim_start_matches("socks5://")
            .trim_start_matches("socks5h://");
        match tokio::time::timeout(self.connect_timeout, tokio::net::TcpStream::connect(addr)).await {
            Ok(Ok(_)) => {
                node.set_probed(true);
                info!(url = %addr, "node probe OK");
                true
            }
            Ok(Err(e)) => {
                node.set_probed(false);
                warn!(url = %addr, error = %e, "node probe FAIL");
                false
            }
            Err(_) => {
                node.set_probed(false);
                warn!(url = %addr, "node probe TIMEOUT");
                false
            }
        }
    }

    /// Probe all nodes in the selector
    pub async fn probe_all(&self) -> (usize, usize) {
        let nodes: Vec<Arc<ProxyNode>> = self.selector.nodes().to_vec();
        let total = nodes.len();
        let mut ok = 0usize;
        for node in nodes {
            if self.probe_node(node).await { ok += 1; }
        }
        (ok, total)
    }
}

pub async fn orchestrator_loop(prober: &NodeProber) {
    info!("node probe cycle started");
    let (ok, total) = prober.probe_all().await;
    info!(ok, total, "node probe cycle completed");
}

/// Simple version that creates internal prober (used by main.rs)
pub async fn orchestrator_loop_placeholder() {
    info!("node probe cycle started (placeholder - use NodeProber for real probing)");
    tokio::time::sleep(Duration::from_millis(100)).await;
}
