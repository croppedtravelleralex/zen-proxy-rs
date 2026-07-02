use axum::body::Bytes;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::pool::{DispatchError, DispatchResult, NodeRef, ProbeResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtocolKind {
    OpenAIChatCompletions,
    AnthropicMessages,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestContext {
    pub request_id: String,
    pub client_id_hash: Option<String>,
    pub public_model: String,
    pub upstream_model: String,
    pub protocol: ProtocolKind,
    pub stream: bool,
    pub body_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransportErrorKind {
    Timeout,
    ConnectionRefused,
    DnsFailure,
    ProxyHandshake,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpstreamOutcome {
    Success {
        status: u16,
        usage: Option<TokenUsage>,
    },
    RateLimited {
        status: u16,
        retry_after_secs: Option<u64>,
    },
    UpstreamError {
        status: u16,
    },
    TransportError {
        kind: TransportErrorKind,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestRecord {
    pub request_id: String,
    pub ts_ms: i64,
    pub public_model: String,
    pub upstream_model: String,
    pub protocol: ProtocolKind,
    pub stream: bool,
    pub selected_node_id: String,
    pub selected_node_url_redacted: String,
    pub observed_exit_ip: Option<String>,
    pub status: u16,
    pub outcome: String,
    pub retry_count: u32,
    pub latency_total_ms: u64,
    pub upstream_ms: u64,
    pub ttft_ms: Option<u64>,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
}

pub struct ProviderResponse {
    pub response: axum::response::Response,
    pub outcome: UpstreamOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    pub outcome: UpstreamOutcome,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError {
    pub kind: TransportErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeRequest {
    pub public_model: String,
    pub upstream_model: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeadNodeState {
    pub node_id: String,
    pub dead_count: u32,
    pub last_probe_ts_ms: Option<i64>,
    pub recent_recovery_rate: f64,
}

pub trait TransportHandle: Send + Sync {
    fn client(&self) -> reqwest::Client;
    fn node(&self) -> &NodeRef;
}

pub trait ProviderAdapter: Send + Sync {
    fn handle<'a>(
        &'a self,
        ctx: &'a RequestContext,
        transport: &'a dyn TransportHandle,
        body: Bytes,
    ) -> BoxFuture<'a, Result<ProviderResponse, ProviderError>>;
}

pub trait FreeModelKernel: Send + Sync {
    fn openai_chat<'a>(
        &'a self,
        client: reqwest::Client,
        ctx: &'a RequestContext,
        body: serde_json::Value,
    ) -> BoxFuture<'a, Result<ProviderResponse, ProviderError>>;

    fn anthropic_messages<'a>(
        &'a self,
        client: reqwest::Client,
        ctx: &'a RequestContext,
        body: serde_json::Value,
    ) -> BoxFuture<'a, Result<ProviderResponse, ProviderError>>;
}

pub trait TransportProvider: Send + Sync {
    fn client_for_node(&self, node: &NodeRef) -> Result<reqwest::Client, TransportError>;
    fn probe_node<'a>(
        &'a self,
        node: &'a NodeRef,
        probe: &'a ProbeRequest,
    ) -> BoxFuture<'a, ProbeResult>;
}

pub trait V4PoolManager: Send + Sync {
    fn dispatch(&self, ctx: &RequestContext) -> Result<DispatchResult, DispatchError>;
    fn dispatch_sticky(
        &self,
        ctx: &RequestContext,
        node_id: &str,
    ) -> Result<DispatchResult, DispatchError>;
    fn report(&self, node_id: &str, outcome: &UpstreamOutcome);
}

pub trait DeadProbePolicy: Send + Sync {
    fn next_delay_secs(&self, node: &DeadNodeState) -> u64;
    fn next_batch_size(&self, dead_count: usize, recent_recovery_rate: f64) -> usize;
    fn recovered(&self, result: &ProbeResult) -> bool;
}

pub trait RequestLedger: Send + Sync {
    fn record_request(&self, record: RequestRecord);
    fn record_event(&self, event: EventRecord);
    fn query_requests(&self, filter: RequestFilter) -> RequestQueryResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
    pub ts_ms: i64,
    pub request_id: Option<String>,
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestFilter {
    pub request_id: Option<String>,
    pub public_model: Option<String>,
    pub status: Option<u16>,
    pub limit: usize,
    pub cursor: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestQueryResult {
    pub items: Vec<RequestRecord>,
    pub next_cursor: Option<u64>,
}
