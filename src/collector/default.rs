use crate::collector::aggregator::RollingAggregator;
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
    backend: RwLock<Option<Box<dyn StorageBackend>>>,
    pool_dims: RwLock<PoolDimensionStats>,
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
            wal: None,
            backend: RwLock::new(None),
            pool_dims: RwLock::new(PoolDimensionStats {
                dispatch_size: 0,
                active_size: 0,
                ratelimited_size: 0,
                dead_size: 0,
                pool_transitions: 0,
                active_concurrency: 0,
            }),
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
                .map_or(false, |t| t.elapsed().as_secs_f64() > 60.0)
            {
                rpm.pop_front();
            }
        }

        self.ring_buffer.push(tele.clone());
        self.aggregator.record(tele);

        if let Some(ref wal) = self.wal {
            let _ = wal.append(tele);
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

    fn record_schedule(&self, _event: &ScheduleEvent) {}

    fn record_probe(&self, _event: &ProbeEvent) {}

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
}
