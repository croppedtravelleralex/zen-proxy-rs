use std::sync::atomic::Ordering;
use crate::node_db::NodeDB;
use crate::metrics::Metrics;
use crate::bandwidth::BandwidthCollector;
use crate::health::UpstreamHealth;
use crate::selector::ProxySelector;

pub struct AdminState {
    pub api_key: String,
}

impl AdminState {
    pub fn new() -> Self {
        Self {
            api_key: std::env::var("ADMIN_API_KEY").unwrap_or_else(|_| "zen-admin-key".into()),
        }
    }
}

pub fn build_admin_stats(
    node_db: &NodeDB,
    metrics: &Metrics,
    bandwidth: &BandwidthCollector,
    upstream: &UpstreamHealth,
    proxy_selector: Option<&ProxySelector>,
) -> serde_json::Value {
    let us = upstream.stats();
    serde_json::json!({
        "requests": {
            "total": metrics.total_requests.load(Ordering::Relaxed),
            "success": metrics.success_count.load(Ordering::Relaxed),
            "error": metrics.error_count.load(Ordering::Relaxed),
            "count_429": metrics.count_429.load(Ordering::Relaxed),
            "rpm": metrics.rpm(),
        },
        "upstream": {
            "backoff": us.backoff,
            "rate_429": us.rate_429,
            "total_requests": us.total_requests,
            "success_rate": us.success_rate,
        },
        "bandwidth": {
            "bps": bandwidth.bps(),
        },
        "node_db": {
            "node_count": node_db.node_count(),
        },
        "proxy_selector": {
            "total_nodes": proxy_selector.map(|ps| ps.total_nodes()).unwrap_or(0),
            "available_nodes": proxy_selector.map(|ps| ps.available_nodes()).unwrap_or(0),
        },
    })
}
