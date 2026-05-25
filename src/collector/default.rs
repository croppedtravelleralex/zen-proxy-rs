use crate::collector::aggregator::RollingAggregator;
use crate::collector::audit::{AuditGroup, AuditStore};
use crate::collector::ring_buffer::RingBuffer;
use crate::collector::wal::WAL;
use crate::collector::*;
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
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        if tele.status >= 200 && tele.status <= 299 {
            self.success_count.fetch_add(1, Ordering::Relaxed);
        }
        if tele.rate_limited {
            self.count_429.fetch_add(1, Ordering::Relaxed);
        } else if tele.status >= 500 {
            self.count_5xx.fetch_add(1, Ordering::Relaxed);
        } else if tele.status >= 400 {
            self.count_4xx.fetch_add(1, Ordering::Relaxed);
        }
        self.bytes_sent
            .fetch_add(tele.bytes_sent, Ordering::Relaxed);
        self.bytes_received
            .fetch_add(tele.bytes_received, Ordering::Relaxed);
        self.bandwidth_bytes
            .fetch_add(tele.bytes_sent + tele.bytes_received, Ordering::Relaxed);

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

        self.ring_buffer.push(tele.clone());
        self.aggregator.record(tele);

        if let Some(ref wal) = self.wal {
            let _ = wal.append(tele);
        }
        if let Some(ref audit) = self.audit {
            let _ = audit.append(tele);
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
