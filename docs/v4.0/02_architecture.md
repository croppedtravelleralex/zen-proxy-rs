# V4.0 Architecture

## Architecture Shape

V4.0 uses a "mortise and tenon" structure: each layer owns one kind of decision
and exposes a narrow contract to the next layer.

```text
L7 HTTP/API
L6 Application Services
L5 Policy
L4 Provider Adapter / FreeModelKernel
L3 Transport
L2 Pool State
L1 Observability
L0 Config and Storage
```

## Layer Responsibilities

### L7 HTTP/API

Owns request and response protocol boundaries:

- `/health`
- `/metrics`
- `/v1/models`
- `/v1/chat/completions`
- `/v1/messages`
- `/admin/*`

It must not build Zen requests or mutate pool internals directly.

### L6 Application Services

Owns orchestration:

- `ProxyService`
- `AdminService`
- `ProbeService`

It builds `RequestContext`, calls policy, calls pool/transport/provider, and
records outcomes.

### L5 Policy

Owns decisions:

- `ModelRegistry`
- `RouteSelector`
- `RetryPolicy`
- `FusePolicy`
- `DeadProbePolicy`
- `RateLimitPolicy`

Policy does not send HTTP requests.

### L4 Provider Adapter / FreeModelKernel

Owns upstream protocol semantics:

- OpenAI request normalization.
- Anthropic request normalization.
- Zen request body.
- Zen headers.
- Zen SSE parsing.
- OpenAI/Anthropic response formatting.
- tool-call fallback.
- reasoning-only fallback.

This layer receives a selected transport/client. It does not choose proxy nodes.

### L3 Transport

Owns egress:

- `DirectTransport`
- `Socks5Transport`
- future `HttpProxyTransport`
- future provider-specific transport wrappers.

Transport creates or reuses `reqwest::Client` instances for selected nodes.

### L2 Pool State

Owns node lifecycle:

- Dispatch
- Active
- RateLimited
- Dead
- ProbePeriod

It does not know OpenAI, Anthropic, or Zen body formats.

### L1 Observability

Owns facts:

- `RequestRecord`
- `EventRecord`
- metrics
- WAL/JSONL
- admin query views

There should be one canonical request record model. Derived counters must come
from it or explicitly document why they are separate.

### L0 Config and Storage

Owns configuration and durable state:

- env defaults
- `nodes.json`
- `policies.json`
- model mapping config
- WAL files
- snapshots

## Dependency Rule

Allowed direction:

```text
HTTP -> Application -> Policy -> Provider/Transport/Pool -> Observability
```

Forbidden direction:

```text
Pool -> Provider protocol
Transport -> model mapping
Provider -> pool mutation
Observability -> request orchestration
```

## Main Design Constraint

The actual Zen request must use the transport selected by `PoolManager`. Any
implementation that lets the FreeModel path create an unrelated global client is
not V4.0-compliant.

