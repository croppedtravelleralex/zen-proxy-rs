# V4.0 Request Flow

## OpenAI Chat Flow

```text
POST /v1/chat/completions
-> parse and validate request
-> profile request body, messages, tools, and estimated tokens
-> budget decision: pass, warn, observe_compact, compact, or reject
-> optionally compact old low-value context before upstream dispatch
-> build RequestContext
-> ModelRegistry resolves public model to upstream model
-> PoolManager dispatch selects node
-> TransportProvider returns per-node reqwest::Client
-> FreeModelProviderAdapter calls FreeModelKernel
-> FreeModelKernel sends Zen request with selected client
-> parse upstream response or SSE
-> PoolManager report outcome
-> RequestLedger records request
-> response returned to client
```

## Anthropic Messages Flow

```text
POST /v1/messages
-> parse and validate Anthropic request
-> profile request body, messages, tools, and estimated tokens
-> budget decision: pass, warn, observe_compact, compact, or reject
-> optionally compact old low-value context before upstream dispatch
-> build RequestContext
-> map public model
-> select node and transport
-> FreeModelKernel translates Anthropic to Zen-compatible request
-> Zen response translated back to Anthropic format
-> pool and ledger updated
```

## Failure Flow

### Overlarge Context

```text
request reaches ZenProxyRS ingress
-> ContextProfiler records body bytes, token estimate, messages, tools, largest message
-> ContextBudgeter compares against body and token thresholds
-> observe mode: record intended compaction, pass body unchanged
-> enforce mode: trim old tool results / old large text / old prefixes
-> if still above upstream-safe body limit: return structured 413
-> otherwise continue normal provider flow
```

Required behavior:

- ingress body limit is configurable and must exceed Axum's default 2MB.
- upstream-safe target stays below the standard 32MB body budget.
- current user input and recent tool chain are preserved.
- every compaction action is visible in request telemetry.

### 429 or FreeUsageLimitError

```text
Zen returns 429
-> ProviderAdapter returns UpstreamOutcome::RateLimited
-> PoolManager moves selected node to RateLimited
-> RetryPolicy decides whether to retry
-> Retry should prefer sticky same node before spending another node
-> Ledger records retry_after and selected node
```

### Transport Failure

```text
proxy connect timeout / refused / DNS / SOCKS failure
-> TransportError
-> PoolManager moves node to Dead
-> DeadProbePolicy schedules low-frequency future probing
-> Ledger records TransportError
```

### 5xx

```text
upstream 5xx
-> short backoff
-> retry if policy allows
-> repeated failure may move node to Dead or a degraded state
```

## Streaming Flow

Streaming is parsed by FreeModelKernel, not by generic proxy byte patching.

```text
Zen SSE
-> frame-aware parser
-> OpenAI stream or Anthropic stream formatter
-> TTFT measured
-> final usage captured when available
```

## Important Non-Flow

Do not implement this:

```text
PoolManager selects SOCKS node
-> reqwest uses SOCKS to call local free-model-client-rs HTTP
-> free-model-client-rs uses its own global client to call Zen
```

This breaks the egress contract.

## Request Outcome Classification

| Condition | Outcome | Pool action |
|---|---|---|
| 2xx | Success | release to Dispatch |
| 429 | RateLimited | move to RateLimited |
| FreeUsageLimitError | RateLimited | move to RateLimited |
| connect timeout | TransportError::Timeout | move to Dead |
| SOCKS handshake failure | TransportError::ProxyHandshake | move to Dead |
| connection refused | TransportError::ConnectionRefused | move to Dead |
| 500/502/503/504 | UpstreamError | retry/backoff, then policy decision |
