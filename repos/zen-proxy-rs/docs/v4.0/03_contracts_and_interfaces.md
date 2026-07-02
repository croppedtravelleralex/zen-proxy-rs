# V4.0 Contracts and Interfaces

## Core Data Types

### RequestContext

```rust
pub struct RequestContext {
    pub request_id: String,
    pub client_id_hash: Option<String>,
    pub public_model: String,
    pub upstream_model: String,
    pub protocol: ProtocolKind,
    pub stream: bool,
    pub body_size: u64,
}
```

### ProtocolKind

```rust
pub enum ProtocolKind {
    OpenAIChatCompletions,
    AnthropicMessages,
}
```

### UpstreamOutcome

```rust
pub enum UpstreamOutcome {
    Success { status: u16, usage: Option<TokenUsage> },
    RateLimited { status: u16, retry_after_secs: Option<u64> },
    UpstreamError { status: u16 },
    TransportError { kind: TransportErrorKind },
}
```

### RequestRecord

```rust
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
```

## Required Traits

### ModelRegistry

```rust
pub trait ModelRegistry: Send + Sync {
    fn public_models(&self) -> Vec<ModelInfo>;
    fn resolve(&self, public_model: &str) -> Result<ModelResolution, ModelError>;
}
```

V4.0 default:

```text
deepseek-v4-flash      -> deepseek-v4-flash-free
deepseek-v4-flash-lite -> big-pickle
```

### ProviderAdapter

```rust
pub trait ProviderAdapter: Send + Sync {
    async fn handle(
        &self,
        ctx: &RequestContext,
        transport: &dyn TransportHandle,
        body: bytes::Bytes,
    ) -> Result<ProviderResponse, ProviderError>;
}
```

The first V4.0 implementation is `FreeModelProviderAdapter`.

### FreeModelKernel

```rust
pub trait FreeModelKernel: Send + Sync {
    async fn openai_chat(
        &self,
        client: reqwest::Client,
        ctx: &RequestContext,
        body: serde_json::Value,
    ) -> Result<ProviderResponse, ProviderError>;

    async fn anthropic_messages(
        &self,
        client: reqwest::Client,
        ctx: &RequestContext,
        body: serde_json::Value,
    ) -> Result<ProviderResponse, ProviderError>;
}
```

The key constraint is that `client` comes from `zen-proxy-rs` transport
selection.

### TransportProvider

```rust
pub trait TransportProvider: Send + Sync {
    fn client_for_node(&self, node: &NodeRef) -> Result<reqwest::Client, TransportError>;
    async fn probe_node(&self, node: &NodeRef, probe: &ProbeRequest) -> ProbeResult;
}
```

### PoolManager

```rust
pub trait PoolManager: Send + Sync {
    fn dispatch(&self, ctx: &RequestContext) -> Result<DispatchResult, DispatchError>;
    fn dispatch_sticky(&self, ctx: &RequestContext, node_id: &str) -> Result<DispatchResult, DispatchError>;
    fn report(&self, node_id: &str, outcome: &UpstreamOutcome);
}
```

### DeadProbePolicy

```rust
pub trait DeadProbePolicy: Send + Sync {
    fn next_delay_secs(&self, node: &DeadNodeState) -> u64;
    fn next_batch_size(&self, dead_count: usize, recent_recovery_rate: f64) -> usize;
    fn recovered(&self, result: &ProbeResult) -> bool;
}
```

### RequestLedger

```rust
pub trait RequestLedger: Send + Sync {
    fn record_request(&self, record: RequestRecord);
    fn record_event(&self, event: EventRecord);
    fn query_requests(&self, filter: RequestFilter) -> RequestQueryResult;
}
```

## Compatibility Rule

Existing public routes may be preserved, but their internal implementation must
go through these contracts before V4.0 is considered complete.

