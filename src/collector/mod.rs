pub mod aggregator;
pub mod default;
pub mod export;
pub mod ring_buffer;
pub mod telemetry;
pub mod wal;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestTelemetry {
    pub rid: String,
    pub ts: i64,
    pub model: String,
    pub client_id: String,
    pub path: String,
    pub method: String,
    pub is_streaming: bool,
    pub node_url: String,
    pub pool: String,
    pub exit_ip: String,
    pub status: u16,
    pub rate_limited: bool,
    pub retry_count: u32,
    pub latency_total_ms: u64,
    pub upstream_ms: u64,
    pub ttft_ms: u64,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub bytes_sent: u64,
    pub bytes_received: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolEvent {
    pub ts: i64,
    pub node_id: String,
    pub from_pool: String,
    pub to_pool: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleEvent {
    pub ts: i64,
    pub node_id: String,
    pub pool: String,
    pub score_before: f64,
    pub success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeEvent {
    pub ts: i64,
    pub node_id: String,
    pub pool: String,
    pub ok: bool,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemEvent {
    pub ts: i64,
    pub kind: String,
    pub value: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataSnapshot {
    pub ts: i64,
    pub requests: RequestCounters,
    pub pools: PoolDimensionStats,
    pub system: SystemStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestCounters {
    pub total: u64,
    pub success: u64,
    pub count_429: u64,
    pub count_4xx: u64,
    pub count_5xx: u64,
    pub count_timeout: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub rpm: u64,
    pub avg_latency_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolDimensionStats {
    pub dispatch_size: usize,
    pub active_size: usize,
    pub ratelimited_size: usize,
    pub dead_size: usize,
    pub pool_transitions: u64,
    pub active_concurrency: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemStats {
    pub current_bps: f64,
    pub memory_bytes: u64,
    pub uptime_secs: u64,
}

pub trait StorageBackend: Send + Sync {
    fn write(&self, snapshot: &DataSnapshot);
    fn name(&self) -> &'static str;
}

pub trait DataCollector: Send + Sync {
    fn record_request(&self, tele: &RequestTelemetry);
    fn record_pool(&self, event: &PoolEvent);
    fn record_schedule(&self, event: &ScheduleEvent);
    fn record_probe(&self, event: &ProbeEvent);
    fn record_system(&self, event: &SystemEvent);
    fn snapshot(&self) -> DataSnapshot;
    fn set_backend(&self, backend: Box<dyn StorageBackend>);
}
