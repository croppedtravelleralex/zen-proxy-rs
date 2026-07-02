use crate::collector::aggregator::RollingAggregator;
use crate::collector::audit::{AuditGroup, AuditStore};
use crate::collector::ring_buffer::RingBuffer;
use crate::collector::wal::WAL;
use crate::collector::*;
use crate::ledger::sanitize_request_telemetry;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::Instant;

pub struct DefaultCollector {
    total_requests: AtomicU64,
    success_count: AtomicU64,
    count_429: AtomicU64,
    count_4xx: AtomicU64,
    count_5xx: AtomicU64,
    count_timeout: AtomicU64,
    bytes_sent: AtomicU64,
    bytes_received: AtomicU64,
    request_dims: Mutex<RequestDimensionCounters>,
    rpm_window: Mutex<VecDeque<Instant>>,
    ring_buffer: RingBuffer,
    aggregator: RollingAggregator,
    wal: Option<WAL>,
    audit: Option<AuditStore>,
    backend: RwLock<Option<Box<dyn StorageBackend>>>,
    pool_dims: RwLock<PoolDimensionStats>,
    pool_events: RwLock<VecDeque<PoolEvent>>,
    bandwidth_bytes: AtomicU64,
    bandwidth_ts: Mutex<Instant>,
    current_bps: Mutex<f64>,
}

impl DefaultCollector {
    pub fn new() -> Self {
        DefaultCollector {
            total_requests: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            count_429: AtomicU64::new(0),
            count_4xx: AtomicU64::new(0),
            count_5xx: AtomicU64::new(0),
            count_timeout: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            request_dims: Mutex::new(RequestDimensionCounters::default()),
            rpm_window: Mutex::new(VecDeque::with_capacity(4096)),
            ring_buffer: RingBuffer::new(10000),
            aggregator: RollingAggregator::new(300_000, 12),
            wal: std::env::var("TELEMETRY_WAL_PATH")
                .ok()
                .as_deref()
                .map(WAL::new),
            audit: load_audit_store(),
            backend: RwLock::new(None),
            pool_dims: RwLock::new(PoolDimensionStats {
                dispatch_size: 0,
                active_size: 0,
                ratelimited_size: 0,
                dead_size: 0,
                pool_transitions: 0,
                active_concurrency: 0,
            }),
            pool_events: RwLock::new(VecDeque::with_capacity(5000)),
            bandwidth_bytes: AtomicU64::new(0),
            bandwidth_ts: Mutex::new(Instant::now()),
            current_bps: Mutex::new(0.0),
        }
    }

    pub fn sample_bps(&self) -> f64 {
        let bytes = self.bandwidth_bytes.swap(0, Ordering::Relaxed);
        let mut last_ts = self.bandwidth_ts.lock().unwrap();
        let elapsed = last_ts.elapsed().as_secs_f64();
        *last_ts = Instant::now();
        if elapsed > 0.0 {
            let bps = bytes as f64 / elapsed;
            *self.current_bps.lock().unwrap() = bps;
            bps
        } else {
            0.0
        }
    }
}

impl DataCollector for DefaultCollector {
    fn record_request(&self, tele: &RequestTelemetry) {
        let safe_tele = sanitize_request_telemetry(tele);

        self.total_requests.fetch_add(1, Ordering::Relaxed);
        if safe_tele.status >= 200 && safe_tele.status <= 299 {
            self.success_count.fetch_add(1, Ordering::Relaxed);
        }
        if safe_tele.rate_limited {
            self.count_429.fetch_add(1, Ordering::Relaxed);
        } else if safe_tele.status >= 500 {
            self.count_5xx.fetch_add(1, Ordering::Relaxed);
        } else if safe_tele.status >= 400 {
            self.count_4xx.fetch_add(1, Ordering::Relaxed);
        }
        if safe_tele.failure_kind == "timeout" || safe_tele.outcome == "timeout" {
            self.count_timeout.fetch_add(1, Ordering::Relaxed);
        }
        self.bytes_sent
            .fetch_add(safe_tele.bytes_sent, Ordering::Relaxed);
        self.bytes_received
            .fetch_add(safe_tele.bytes_received, Ordering::Relaxed);
        self.bandwidth_bytes.fetch_add(
            safe_tele.bytes_sent + safe_tele.bytes_received,
            Ordering::Relaxed,
        );
        self.request_dims.lock().unwrap().record(&safe_tele);

        {
            let mut rpm = self.rpm_window.lock().unwrap();
            rpm.push_back(Instant::now());
            while rpm
                .front()
                .is_some_and(|t| t.elapsed().as_secs_f64() > 60.0)
            {
                rpm.pop_front();
            }
        }

        self.ring_buffer.push(safe_tele.clone());
        self.aggregator.record(&safe_tele);

        if let Some(ref wal) = self.wal {
            let _ = wal.append(&safe_tele);
        }
        if let Some(ref audit) = self.audit {
            let _ = audit.append(&safe_tele);
        }
    }

    fn record_pool(&self, event: &PoolEvent) {
        let mut dims = self.pool_dims.write().unwrap();
        dims.pool_transitions += 1;
        match event.to_pool.as_str() {
            "dispatch" => dims.dispatch_size += 1,
            "active" => dims.active_size += 1,
            "ratelimited" => dims.ratelimited_size += 1,
            "dead" => dims.dead_size += 1,
            _ => {}
        }
        match event.from_pool.as_str() {
            "dispatch" => dims.dispatch_size = dims.dispatch_size.saturating_sub(1),
            "active" => dims.active_size = dims.active_size.saturating_sub(1),
            "ratelimited" => dims.ratelimited_size = dims.ratelimited_size.saturating_sub(1),
            "dead" => dims.dead_size = dims.dead_size.saturating_sub(1),
            _ => {}
        }
        dims.active_concurrency = dims.active_size;
    }

    fn record_schedule(&self, event: &ScheduleEvent) {
        if let Ok(mut events) = self.pool_events.write() {
            events.push_back(PoolEvent {
                ts: event.ts,
                node_id: event.node_id.clone(),
                from_pool: event.pool.clone(),
                to_pool: if event.success {
                    "active".into()
                } else {
                    "dispatch".into()
                },
                reason: format!("schedule: score={}", event.score_before),
            });
        }
    }

    fn record_probe(&self, event: &ProbeEvent) {
        if let Ok(mut events) = self.pool_events.write() {
            events.push_back(PoolEvent {
                ts: event.ts,
                node_id: event.node_id.clone(),
                from_pool: event.pool.clone(),
                to_pool: if event.ok {
                    "dispatch".into()
                } else {
                    "dead".into()
                },
                reason: format!(
                    "probe_{}: {}",
                    if event.ok { "ok" } else { "fail" },
                    event.pool
                ),
            });
        }
    }

    fn record_system(&self, event: &SystemEvent) {
        if event.kind == "bps" {
            let mut bps = self.current_bps.lock().unwrap();
            *bps = event.value;
        }
    }

    fn snapshot(&self) -> DataSnapshot {
        let total = self.total_requests.load(Ordering::Relaxed);
        let success = self.success_count.load(Ordering::Relaxed);

        let avg_latency = if total > 0 {
            let (recent, _) = self.ring_buffer.query(None, 100, None);
            let sum: u64 = recent.iter().map(|t| t.latency_total_ms).sum();
            if !recent.is_empty() {
                sum as f64 / recent.len() as f64
            } else {
                0.0
            }
        } else {
            0.0
        };

        let rpm = {
            let rpm = self.rpm_window.lock().unwrap();
            rpm.len() as u64
        };

        let dims = self.pool_dims.read().unwrap().clone();
        let bps = *self.current_bps.lock().unwrap();
        let request_dims = self.request_dims.lock().unwrap().clone();

        DataSnapshot {
            ts: crate::collector::telemetry::unix_ms(),
            requests: RequestCounters {
                total,
                success,
                count_429: self.count_429.load(Ordering::Relaxed),
                count_4xx: self.count_4xx.load(Ordering::Relaxed),
                count_5xx: self.count_5xx.load(Ordering::Relaxed),
                count_timeout: self.count_timeout.load(Ordering::Relaxed),
                bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
                bytes_received: self.bytes_received.load(Ordering::Relaxed),
                rpm,
                avg_latency_ms: avg_latency,
                by_outcome: request_dims.by_outcome,
                by_failure_kind: request_dims.by_failure_kind,
                by_body_bucket: request_dims.by_body_bucket,
                by_stream: request_dims.by_stream,
                by_model: request_dims.by_model,
            },
            pools: dims,
            system: SystemStats {
                current_bps: bps,
                memory_bytes: 0,
                uptime_secs: 0,
            },
        }
    }

    fn set_backend(&self, backend: Box<dyn StorageBackend>) {
        *self.backend.write().unwrap() = Some(backend);
    }

    fn query_requests(&self, filter: &RequestFilter) -> RequestQueryResult {
        let (items, cursor) = self
            .ring_buffer
            .query(filter.since, filter.limit, filter.cursor);
        // Apply post-filter for model/status if set
        let items = if filter.model.is_some() || filter.status.is_some() {
            items
                .into_iter()
                .filter(|r| {
                    let match_model = filter.model.as_ref().is_none_or(|m| r.model == *m);
                    let match_status = filter.status.is_none_or(|s| r.status == s);
                    match_model && match_status
                })
                .collect()
        } else {
            items
        };
        RequestQueryResult {
            items,
            next_cursor: cursor,
        }
    }

    fn aggregator_snapshot(&self) -> serde_json::Value {
        self.aggregator.snapshot()
    }

    fn persist(&self) {
        let snap = self.snapshot();
        if let Some(ref backend) = *self.backend.read().unwrap() {
            backend.write(&snap);
        }
    }

    fn recent_events(&self, limit: usize) -> Vec<PoolEvent> {
        self.pool_events
            .read()
            .unwrap()
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect()
    }

    fn query_audit_requests(&self, filter: &RequestFilter) -> RequestQueryResult {
        match &self.audit {
            Some(audit) => audit.query_requests(filter),
            None => RequestQueryResult {
                items: Vec::new(),
                next_cursor: None,
            },
        }
    }

    fn audit_summary(&self, filter: &RequestFilter) -> serde_json::Value {
        match &self.audit {
            Some(audit) => audit.summary(filter),
            None => serde_json::json!({"requests": 0, "disabled": true}),
        }
    }

    fn audit_models(&self, filter: &RequestFilter) -> serde_json::Value {
        match &self.audit {
            Some(audit) => audit.grouped(filter, AuditGroup::Model),
            None => serde_json::json!([]),
        }
    }

    fn audit_nodes(&self, filter: &RequestFilter) -> serde_json::Value {
        match &self.audit {
            Some(audit) => audit.grouped(filter, AuditGroup::Node),
            None => serde_json::json!([]),
        }
    }

    fn audit_anomalies(&self, filter: &RequestFilter) -> serde_json::Value {
        match &self.audit {
            Some(audit) => audit.anomalies(filter),
            None => serde_json::json!([]),
        }
    }

    fn audit_export(&self, filter: &RequestFilter) -> String {
        match &self.audit {
            Some(audit) => audit.export(filter),
            None => String::new(),
        }
    }

    fn audit_timeseries(&self, filter: &RequestFilter, bucket_ms: i64) -> serde_json::Value {
        match &self.audit {
            Some(audit) => audit.timeseries(filter, bucket_ms),
            None => serde_json::json!([]),
        }
    }

    fn audit_top_requests(&self, filter: &RequestFilter, by: &str) -> serde_json::Value {
        match &self.audit {
            Some(audit) => audit.top_requests(filter, by),
            None => serde_json::json!([]),
        }
    }

    fn audit_top_nodes(&self, filter: &RequestFilter, by: &str) -> serde_json::Value {
        match &self.audit {
            Some(audit) => audit.top_nodes(filter, by),
            None => serde_json::json!([]),
        }
    }

    fn audit_failures(&self, filter: &RequestFilter) -> serde_json::Value {
        match &self.audit {
            Some(audit) => audit.failures(filter),
            None => serde_json::json!([]),
        }
    }

    fn audit_node_detail(&self, filter: &RequestFilter, node_id: &str) -> serde_json::Value {
        match &self.audit {
            Some(audit) => audit.node_detail(filter, node_id),
            None => serde_json::json!({"node_id": node_id, "stats": {"requests": 0}, "recent": []}),
        }
    }

    fn audit_by_external_id(&self, external_id: &str, limit: usize) -> serde_json::Value {
        match &self.audit {
            Some(audit) => audit.by_external_id(external_id, limit),
            None => serde_json::json!([]),
        }
    }

    fn audit_reconcile(&self, filter: &RequestFilter) -> serde_json::Value {
        match &self.audit {
            Some(audit) => audit.reconcile(filter),
            None => serde_json::json!({"requests": 0, "disabled": true}),
        }
    }

    fn audit_budget_history(&self, filter: &RequestFilter, bucket_ms: i64) -> serde_json::Value {
        match &self.audit {
            Some(audit) => audit.budget_history(filter, bucket_ms),
            None => serde_json::json!([]),
        }
    }
}

#[derive(Clone, Default)]
struct RequestDimensionCounters {
    by_outcome: HashMap<String, u64>,
    by_failure_kind: HashMap<String, u64>,
    by_body_bucket: HashMap<String, u64>,
    by_stream: HashMap<String, u64>,
    by_model: HashMap<String, u64>,
}

impl RequestDimensionCounters {
    fn record(&mut self, tele: &RequestTelemetry) {
        increment(&mut self.by_outcome, non_empty_or(&tele.outcome, "unknown"));
        increment(
            &mut self.by_failure_kind,
            non_empty_or(&tele.failure_kind, "none"),
        );
        increment(
            &mut self.by_body_bucket,
            non_empty_or(&tele.body_size_bucket, "unknown"),
        );
        increment(
            &mut self.by_stream,
            if tele.is_streaming {
                "stream"
            } else {
                "non_stream"
            },
        );
        increment(&mut self.by_model, non_empty_or(&tele.model, "unknown"));
    }
}

fn increment(map: &mut HashMap<String, u64>, key: &str) {
    *map.entry(key.to_string()).or_insert(0) += 1;
}

fn non_empty_or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() {
        fallback
    } else {
        value
    }
}

fn load_audit_store() -> Option<AuditStore> {
    if cfg!(test) && std::env::var("AUDIT_LOG_ENABLED").is_err() {
        return None;
    }
    let enabled = std::env::var("AUDIT_LOG_ENABLED")
        .ok()
        .map(|value| !matches!(value.as_str(), "0" | "false" | "off"))
        .unwrap_or(true);
    if !enabled {
        return None;
    }
    let dir = std::env::var("AUDIT_LOG_DIR").unwrap_or_else(|_| "/tmp/zen-proxy-audit".into());
    Some(AuditStore::new(dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_counts_request_observability_dimensions() {
        let collector = DefaultCollector::new();
        collector.record_request(&telemetry(
            "stream_error",
            "stream_error",
            "large",
            true,
            "deepseek-v4-flash",
        ));
        collector.record_request(&telemetry(
            "retry_budget_exhausted",
            "retry_budget_exhausted",
            "huge",
            false,
            "deepseek-v4-pro",
        ));
        collector.record_request(&telemetry(
            "empty_output",
            "empty_output",
            "small",
            true,
            "deepseek-v4-flash",
        ));

        let snapshot = collector.snapshot();

        assert_eq!(snapshot.requests.by_outcome["stream_error"], 1);
        assert_eq!(snapshot.requests.by_outcome["retry_budget_exhausted"], 1);
        assert_eq!(snapshot.requests.by_outcome["empty_output"], 1);
        assert_eq!(snapshot.requests.by_failure_kind["stream_error"], 1);
        assert_eq!(
            snapshot.requests.by_failure_kind["retry_budget_exhausted"],
            1
        );
        assert_eq!(snapshot.requests.by_failure_kind["empty_output"], 1);
        assert_eq!(snapshot.requests.by_body_bucket["large"], 1);
        assert_eq!(snapshot.requests.by_body_bucket["huge"], 1);
        assert_eq!(snapshot.requests.by_stream["stream"], 2);
        assert_eq!(snapshot.requests.by_stream["non_stream"], 1);
        assert_eq!(snapshot.requests.by_model["deepseek-v4-flash"], 2);
    }

    fn telemetry(
        outcome: &str,
        failure_kind: &str,
        body_size_bucket: &str,
        is_streaming: bool,
        model: &str,
    ) -> RequestTelemetry {
        RequestTelemetry {
            rid: format!("{outcome}-{failure_kind}"),
            ts: 1,
            external_request_id: String::new(),
            gateway: String::new(),
            gateway_channel_id: String::new(),
            model: model.to_string(),
            public_model: model.to_string(),
            upstream_model: model.to_string(),
            protocol: "openai_chat_completions".to_string(),
            client_id: "test".to_string(),
            path: "chat/completions".to_string(),
            method: "POST".to_string(),
            is_streaming,
            node_url: "node".to_string(),
            selected_node_id: "n1".to_string(),
            selected_node_url_redacted: "node".to_string(),
            observed_exit_ip: String::new(),
            outcome: outcome.to_string(),
            pool: "dispatch".to_string(),
            exit_ip: String::new(),
            status: 502,
            rate_limited: false,
            retry_count: 0,
            latency_total_ms: 10,
            upstream_ms: 10,
            ttft_ms: 0,
            timings: RequestTimings::default(),
            affinity_key: String::new(),
            affinity_hit: false,
            affinity_node_id: String::new(),
            body_size_bucket: body_size_bucket.to_string(),
            protocol_guard: None,
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            cached_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            bytes_sent: 10,
            bytes_received: 0,
            failure_kind: failure_kind.to_string(),
            failure_message: String::new(),
            retry_chain: Vec::new(),
            context: None,
        }
    }
}
