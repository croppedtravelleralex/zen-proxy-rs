use crate::collector::*;
use std::fs::{rename, File};
use std::io::Write;

pub struct JsonBackend {
    path: String,
}

impl JsonBackend {
    pub fn new(path: &str) -> Self {
        JsonBackend {
            path: path.to_string(),
        }
    }
}

impl StorageBackend for JsonBackend {
    fn write(&self, snapshot: &DataSnapshot) {
        let json_str = serde_json::to_string_pretty(snapshot).unwrap_or_default();
        let tmp_path = format!("{}.tmp", self.path);
        if let Ok(mut f) = File::create(&tmp_path) {
            let _ = f.write_all(json_str.as_bytes());
            let _ = f.sync_all();
            let _ = rename(&tmp_path, &self.path);
        }
    }

    fn name(&self) -> &'static str {
        "json"
    }
}

pub struct PrometheusBackend;

impl PrometheusBackend {
    pub fn encode(&self, snapshot: &DataSnapshot) -> String {
        let mut out = String::new();
        let r = &snapshot.requests;
        let p = &snapshot.pools;
        let s = &snapshot.system;

        out.push_str("# HELP zen_proxy_requests_total Total request count\n");
        out.push_str("# TYPE zen_proxy_requests_total counter\n");
        out.push_str(&format!(
            "zen_proxy_requests_total{{status=\"200\"}} {}\n",
            r.success
        ));
        out.push_str(&format!(
            "zen_proxy_requests_total{{status=\"429\"}} {}\n",
            r.count_429
        ));
        out.push_str(&format!(
            "zen_proxy_requests_total{{status=\"4xx\"}} {}\n",
            r.count_4xx
        ));
        out.push_str(&format!(
            "zen_proxy_requests_total{{status=\"5xx\"}} {}\n",
            r.count_5xx
        ));
        out.push_str(&format!(
            "zen_proxy_requests_total{{status=\"timeout\"}} {}\n",
            r.count_timeout
        ));

        out.push_str("# HELP zen_proxy_pool_size Pool size by state\n");
        out.push_str("# TYPE zen_proxy_pool_size gauge\n");
        out.push_str(&format!(
            "zen_proxy_pool_size{{pool=\"dispatch\"}} {}\n",
            p.dispatch_size
        ));
        out.push_str(&format!(
            "zen_proxy_pool_size{{pool=\"active\"}} {}\n",
            p.active_size
        ));
        out.push_str(&format!(
            "zen_proxy_pool_size{{pool=\"ratelimited\"}} {}\n",
            p.ratelimited_size
        ));
        out.push_str(&format!(
            "zen_proxy_pool_size{{pool=\"dead\"}} {}\n",
            p.dead_size
        ));

        out.push_str("# HELP zen_proxy_active_concurrency Active request concurrency\n");
        out.push_str("# TYPE zen_proxy_active_concurrency gauge\n");
        out.push_str(&format!(
            "zen_proxy_active_concurrency {}\n",
            p.active_concurrency
        ));

        out.push_str("# HELP zen_proxy_bandwidth_bps Current bandwidth in bytes/sec\n");
        out.push_str("# TYPE zen_proxy_bandwidth_bps gauge\n");
        out.push_str(&format!("zen_proxy_bandwidth_bps {}\n", s.current_bps));

        out.push_str("# HELP zen_proxy_rpm Requests per minute\n");
        out.push_str("# TYPE zen_proxy_rpm gauge\n");
        out.push_str(&format!("zen_proxy_rpm {}\n", r.rpm));

        out.push_str("# HELP zen_proxy_bytes_sent Total bytes sent\n");
        out.push_str("# TYPE zen_proxy_bytes_sent counter\n");
        out.push_str(&format!("zen_proxy_bytes_sent {}\n", r.bytes_sent));

        out.push_str("# HELP zen_proxy_bytes_received Total bytes received\n");
        out.push_str("# TYPE zen_proxy_bytes_received counter\n");
        out.push_str(&format!("zen_proxy_bytes_received {}\n", r.bytes_received));

        out.push_str("# HELP zen_proxy_avg_latency_ms Average latency in ms\n");
        out.push_str("# TYPE zen_proxy_avg_latency_ms gauge\n");
        out.push_str(&format!("zen_proxy_avg_latency_ms {}\n", r.avg_latency_ms));

        out.push_str("# HELP zen_proxy_pool_transitions Pool transition count\n");
        out.push_str("# TYPE zen_proxy_pool_transitions counter\n");
        out.push_str(&format!(
            "zen_proxy_pool_transitions {}\n",
            p.pool_transitions
        ));

        out.push_str("# HELP zen_proxy_uptime_seconds Uptime in seconds\n");
        out.push_str("# TYPE zen_proxy_uptime_seconds gauge\n");
        out.push_str(&format!("zen_proxy_uptime_seconds {}\n", s.uptime_secs));

        out
    }
}

impl StorageBackend for PrometheusBackend {
    fn write(&self, snapshot: &DataSnapshot) {
        let _ = self.encode(snapshot);
    }

    fn name(&self) -> &'static str {
        "prometheus"
    }
}

pub struct MultiBackend {
    backends: Vec<Box<dyn StorageBackend>>,
}

impl MultiBackend {
    pub fn new(backends: Vec<Box<dyn StorageBackend>>) -> Self {
        MultiBackend { backends }
    }
}

impl StorageBackend for MultiBackend {
    fn write(&self, snapshot: &DataSnapshot) {
        for backend in &self.backends {
            backend.write(snapshot);
        }
    }

    fn name(&self) -> &'static str {
        "multi"
    }
}
