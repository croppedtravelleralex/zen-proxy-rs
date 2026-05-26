pub mod aggregator;
pub mod async_collector;
pub mod audit;
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
    #[serde(default)]
    pub external_request_id: String,
    #[serde(default)]
    pub gateway: String,
    #[serde(default)]
    pub gateway_channel_id: String,
    pub model: String,
    pub public_model: String,
    pub upstream_model: String,
    pub protocol: String,
    pub client_id: String,
    pub path: String,
    pub method: String,
    pub is_streaming: bool,
    pub node_url: String,
    pub selected_node_id: String,
    pub selected_node_url_redacted: String,
    pub observed_exit_ip: String,
    pub outcome: String,
    pub pool: String,
    pub exit_ip: String,
    pub status: u16,
    pub rate_limited: bool,
    pub retry_count: u32,
    pub latency_total_ms: u64,
    pub upstream_ms: u64,
    pub ttft_ms: u64,
    #[serde(default)]
    pub timings: RequestTimings,
    #[serde(default)]
    pub affinity_key: String,
    #[serde(default)]
    pub affinity_hit: bool,
    #[serde(default)]
    pub affinity_node_id: String,
    #[serde(default)]
    pub body_size_bucket: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    #[serde(default)]
    pub cached_tokens: u32,
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
    #[serde(default)]
    pub cache_read_input_tokens: u32,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    #[serde(default)]
    pub failure_kind: String,
    #[serde(default)]
    pub failure_message: String,
    #[serde(default)]
    pub retry_chain: Vec<RequestAttemptTelemetry>,
    pub context: Option<ContextTelemetry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequestAttemptTelemetry {
    pub attempt: u32,
    pub node_id: String,
    pub node_url_redacted: String,
    pub status: u16,
    pub latency_ms: u64,
    pub outcome: String,
    pub error_type: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequestTimings {
    pub dispatch_wait_ms: u64,
    pub upstream_response_ms: u64,
    pub first_chunk_ms: u64,
    pub first_content_token_ms: u64,
    pub first_tool_call_ms: u64,
    pub stream_complete_ms: u64,
    pub total_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextTelemetry {
    pub original_body_bytes: u64,
    pub effective_body_bytes: u64,
    pub estimated_prompt_tokens: u64,
    pub message_count: u32,
    pub tools_count: u32,
    pub largest_message_bytes: u64,
    pub tool_result_bytes: u64,
    pub mode: String,
    pub action: String,
    pub trimmed: bool,
    pub trimmed_bytes: u64,
    pub artifact_cache_mode: String,
    pub artifact_cache_hits: u32,
    pub artifact_cache_writes: u32,
    pub trace: Vec<String>,
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

pub trait DataCollector: Send + Sync {
    fn record_request(&self, tele: &RequestTelemetry);
    fn record_pool(&self, event: &PoolEvent);
    fn record_schedule(&self, event: &ScheduleEvent);
    fn record_probe(&self, event: &ProbeEvent);
    fn record_system(&self, event: &SystemEvent);
    fn snapshot(&self) -> DataSnapshot;
    fn set_backend(&self, backend: Box<dyn StorageBackend>);
    fn query_requests(&self, filter: &RequestFilter) -> RequestQueryResult;
    fn aggregator_snapshot(&self) -> serde_json::Value;
    fn persist(&self);
    fn recent_events(&self, limit: usize) -> Vec<PoolEvent>;
    fn query_audit_requests(&self, filter: &RequestFilter) -> RequestQueryResult;
    fn audit_summary(&self, filter: &RequestFilter) -> serde_json::Value;
    fn audit_models(&self, filter: &RequestFilter) -> serde_json::Value;
    fn audit_nodes(&self, filter: &RequestFilter) -> serde_json::Value;
    fn audit_anomalies(&self, filter: &RequestFilter) -> serde_json::Value;
    fn audit_export(&self, filter: &RequestFilter) -> String;
    fn audit_timeseries(&self, filter: &RequestFilter, bucket_ms: i64) -> serde_json::Value;
    fn audit_top_requests(&self, filter: &RequestFilter, by: &str) -> serde_json::Value;
    fn audit_top_nodes(&self, filter: &RequestFilter, by: &str) -> serde_json::Value;
    fn audit_failures(&self, filter: &RequestFilter) -> serde_json::Value;
    fn audit_node_detail(&self, filter: &RequestFilter, node_id: &str) -> serde_json::Value;
    fn audit_by_external_id(&self, external_id: &str, limit: usize) -> serde_json::Value;
    fn audit_reconcile(&self, filter: &RequestFilter) -> serde_json::Value;
    fn audit_budget_history(&self, filter: &RequestFilter, bucket_ms: i64) -> serde_json::Value;
}

pub struct RequestFilter {
    pub rid: Option<String>,
    pub model: Option<String>,
    pub node_url: Option<String>,
    pub status: Option<u16>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub limit: usize,
    pub cursor: Option<u64>,
}

impl Default for RequestFilter {
    fn default() -> Self {
        Self {
            rid: None,
            model: None,
            node_url: None,
            status: None,
            since: None,
            until: None,
            limit: 100,
            cursor: None,
        }
    }
}

pub struct RequestQueryResult {
    pub items: Vec<RequestTelemetry>,
    pub next_cursor: Option<u64>,
}

pub trait StorageBackend: Send + Sync {
    fn write(&self, snapshot: &DataSnapshot);
    fn name(&self) -> &'static str;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_telemetry_defaults_missing_optional_v41_fields_for_old_records() {
        let value = json!({
            "rid": "r1",
            "ts": 1,
            "model": "deepseek-v4-flash",
            "public_model": "deepseek-v4-flash",
            "upstream_model": "deepseek-v4-flash-free",
            "protocol": "anthropic_messages",
            "client_id": "sk-dev",
            "path": "messages",
            "method": "POST",
            "is_streaming": true,
            "node_url": "node",
            "selected_node_id": "n1",
            "selected_node_url_redacted": "node",
            "observed_exit_ip": "",
            "outcome": "success",
            "pool": "dispatch",
            "exit_ip": "",
            "status": 200,
            "rate_limited": false,
            "retry_count": 0,
            "latency_total_ms": 10,
            "upstream_ms": 8,
            "ttft_ms": 7,
            "prompt_tokens": 1,
            "completion_tokens": 1,
            "total_tokens": 2,
            "bytes_sent": 100,
            "bytes_received": 50,
            "context": null
        });

        let telemetry: RequestTelemetry = serde_json::from_value(value).unwrap();

        assert_eq!(telemetry.timings.first_chunk_ms, 0);
        assert!(telemetry.external_request_id.is_empty());
        assert!(telemetry.gateway.is_empty());
        assert!(telemetry.failure_kind.is_empty());
        assert!(telemetry.retry_chain.is_empty());
        assert_eq!(telemetry.latency_total_ms, 10);
    }
}
