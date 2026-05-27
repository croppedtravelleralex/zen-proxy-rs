use crate::collector::*;
use std::collections::HashMap;
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
        push_labeled_counts(
            &mut out,
            "zen_proxy_requests_by_outcome_total",
            "Requests by outcome",
            "outcome",
            &r.by_outcome,
        );
        push_labeled_counts(
            &mut out,
            "zen_proxy_requests_by_failure_kind_total",
            "Requests by failure kind",
            "failure_kind",
            &r.by_failure_kind,
        );
        push_labeled_counts(
            &mut out,
            "zen_proxy_requests_by_body_bucket_total",
            "Requests by body size bucket",
            "body_bucket",
            &r.by_body_bucket,
        );
        push_labeled_counts(
            &mut out,
            "zen_proxy_requests_by_stream_total",
            "Requests by stream mode",
            "stream",
            &r.by_stream,
        );
        push_labeled_counts(
            &mut out,
            "zen_proxy_requests_by_model_total",
            "Requests by public model",
            "model",
            &r.by_model,
        );

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

fn push_labeled_counts(
    out: &mut String,
    metric: &str,
    help: &str,
    label_name: &str,
    values: &HashMap<String, u64>,
) {
    out.push_str(&format!("# HELP {metric} {help}\n"));
    out.push_str(&format!("# TYPE {metric} counter\n"));
    for (label_value, count) in values {
        out.push_str(&format!(
            "{metric}{{{label_name}=\"{}\"}} {count}\n",
            escape_label_value(label_value)
        ));
    }
}

fn escape_label_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prometheus_exports_request_dimension_labels() {
        let mut by_outcome = HashMap::new();
        by_outcome.insert("stream_error".to_string(), 1);
        by_outcome.insert("retry_budget_exhausted".to_string(), 2);
        by_outcome.insert("empty_output".to_string(), 3);
        let mut by_failure_kind = HashMap::new();
        by_failure_kind.insert("stream_error".to_string(), 1);
        let mut by_body_bucket = HashMap::new();
        by_body_bucket.insert("huge".to_string(), 4);
        let mut by_stream = HashMap::new();
        by_stream.insert("stream".to_string(), 5);
        let mut by_model = HashMap::new();
        by_model.insert("deepseek-v4-flash".to_string(), 6);

        let encoded = PrometheusBackend.encode(&DataSnapshot {
            ts: 1,
            requests: RequestCounters {
                total: 6,
                success: 0,
                count_429: 0,
                count_4xx: 0,
                count_5xx: 0,
                count_timeout: 0,
                bytes_sent: 0,
                bytes_received: 0,
                rpm: 0,
                avg_latency_ms: 0.0,
                by_outcome,
                by_failure_kind,
                by_body_bucket,
                by_stream,
                by_model,
            },
            pools: PoolDimensionStats {
                dispatch_size: 0,
                active_size: 0,
                ratelimited_size: 0,
                dead_size: 0,
                pool_transitions: 0,
                active_concurrency: 0,
            },
            system: SystemStats {
                current_bps: 0.0,
                memory_bytes: 0,
                uptime_secs: 0,
            },
        });

        assert!(encoded.contains("zen_proxy_requests_by_outcome_total{outcome=\"stream_error\"} 1"));
        assert!(encoded
            .contains("zen_proxy_requests_by_outcome_total{outcome=\"retry_budget_exhausted\"} 2"));
        assert!(encoded.contains("zen_proxy_requests_by_outcome_total{outcome=\"empty_output\"} 3"));
        assert!(encoded
            .contains("zen_proxy_requests_by_failure_kind_total{failure_kind=\"stream_error\"} 1"));
        assert!(encoded.contains("zen_proxy_requests_by_body_bucket_total{body_bucket=\"huge\"} 4"));
        assert!(encoded.contains("zen_proxy_requests_by_stream_total{stream=\"stream\"} 5"));
        assert!(
            encoded.contains("zen_proxy_requests_by_model_total{model=\"deepseek-v4-flash\"} 6")
        );
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
