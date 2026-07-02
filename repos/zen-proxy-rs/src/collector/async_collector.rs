use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::collector::{
    DataCollector, DataSnapshot, PoolEvent, ProbeEvent, RequestFilter, RequestQueryResult,
    RequestTelemetry, ScheduleEvent, StorageBackend, SystemEvent,
};

enum CollectorEvent {
    Request(Box<RequestTelemetry>),
    Pool(PoolEvent),
    Schedule(ScheduleEvent),
    Probe(ProbeEvent),
    System(SystemEvent),
}

pub struct AsyncCollector {
    inner: Arc<dyn DataCollector>,
    tx: mpsc::Sender<CollectorEvent>,
    dropped_events: AtomicU64,
}

impl AsyncCollector {
    pub fn spawn(inner: Arc<dyn DataCollector>, queue_capacity: usize) -> Arc<Self> {
        let (tx, mut rx) = mpsc::channel(queue_capacity.max(1));
        let worker_inner = inner.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    CollectorEvent::Request(item) => worker_inner.record_request(item.as_ref()),
                    CollectorEvent::Pool(item) => worker_inner.record_pool(&item),
                    CollectorEvent::Schedule(item) => worker_inner.record_schedule(&item),
                    CollectorEvent::Probe(item) => worker_inner.record_probe(&item),
                    CollectorEvent::System(item) => worker_inner.record_system(&item),
                }
            }
        });

        Arc::new(Self {
            inner,
            tx,
            dropped_events: AtomicU64::new(0),
        })
    }

    pub fn dropped_events(&self) -> u64 {
        self.dropped_events.load(Ordering::Relaxed)
    }

    fn enqueue_lossy(&self, event: CollectorEvent) {
        if self.tx.try_send(event).is_err() {
            self.dropped_events.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl DataCollector for AsyncCollector {
    fn record_request(&self, tele: &RequestTelemetry) {
        match self
            .tx
            .try_send(CollectorEvent::Request(Box::new(tele.clone())))
        {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(CollectorEvent::Request(item)))
            | Err(mpsc::error::TrySendError::Closed(CollectorEvent::Request(item))) => {
                self.dropped_events.fetch_add(1, Ordering::Relaxed);
                self.inner.record_request(item.as_ref());
            }
            Err(_) => {
                self.dropped_events.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn record_pool(&self, event: &PoolEvent) {
        self.enqueue_lossy(CollectorEvent::Pool(event.clone()));
    }

    fn record_schedule(&self, event: &ScheduleEvent) {
        self.enqueue_lossy(CollectorEvent::Schedule(event.clone()));
    }

    fn record_probe(&self, event: &ProbeEvent) {
        self.enqueue_lossy(CollectorEvent::Probe(event.clone()));
    }

    fn record_system(&self, event: &SystemEvent) {
        self.enqueue_lossy(CollectorEvent::System(event.clone()));
    }

    fn snapshot(&self) -> DataSnapshot {
        self.inner.snapshot()
    }

    fn set_backend(&self, backend: Box<dyn StorageBackend>) {
        self.inner.set_backend(backend);
    }

    fn query_requests(&self, filter: &RequestFilter) -> RequestQueryResult {
        self.inner.query_requests(filter)
    }

    fn aggregator_snapshot(&self) -> serde_json::Value {
        self.inner.aggregator_snapshot()
    }

    fn persist(&self) {
        self.inner.persist();
    }

    fn recent_events(&self, limit: usize) -> Vec<PoolEvent> {
        self.inner.recent_events(limit)
    }

    fn query_audit_requests(&self, filter: &RequestFilter) -> RequestQueryResult {
        self.inner.query_audit_requests(filter)
    }

    fn audit_summary(&self, filter: &RequestFilter) -> serde_json::Value {
        self.inner.audit_summary(filter)
    }

    fn audit_models(&self, filter: &RequestFilter) -> serde_json::Value {
        self.inner.audit_models(filter)
    }

    fn audit_nodes(&self, filter: &RequestFilter) -> serde_json::Value {
        self.inner.audit_nodes(filter)
    }

    fn audit_anomalies(&self, filter: &RequestFilter) -> serde_json::Value {
        self.inner.audit_anomalies(filter)
    }

    fn audit_export(&self, filter: &RequestFilter) -> String {
        self.inner.audit_export(filter)
    }

    fn audit_timeseries(&self, filter: &RequestFilter, bucket_ms: i64) -> serde_json::Value {
        self.inner.audit_timeseries(filter, bucket_ms)
    }

    fn audit_top_requests(&self, filter: &RequestFilter, by: &str) -> serde_json::Value {
        self.inner.audit_top_requests(filter, by)
    }

    fn audit_top_nodes(&self, filter: &RequestFilter, by: &str) -> serde_json::Value {
        self.inner.audit_top_nodes(filter, by)
    }

    fn audit_failures(&self, filter: &RequestFilter) -> serde_json::Value {
        self.inner.audit_failures(filter)
    }

    fn audit_node_detail(&self, filter: &RequestFilter, node_id: &str) -> serde_json::Value {
        self.inner.audit_node_detail(filter, node_id)
    }

    fn audit_by_external_id(&self, external_id: &str, limit: usize) -> serde_json::Value {
        self.inner.audit_by_external_id(external_id, limit)
    }

    fn audit_reconcile(&self, filter: &RequestFilter) -> serde_json::Value {
        self.inner.audit_reconcile(filter)
    }

    fn audit_budget_history(&self, filter: &RequestFilter, bucket_ms: i64) -> serde_json::Value {
        self.inner.audit_budget_history(filter, bucket_ms)
    }
}
