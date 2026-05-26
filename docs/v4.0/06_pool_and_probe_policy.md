# V4.0 Pool and Probe Policy

## Pool States

```text
Dispatch      available for new requests
Active        currently checked out
RateLimited   hit 429 or FreeUsageLimitError
Dead          transport or proxy failure
ProbePeriod   being tested for recovery
```

## RateLimited Pool

A node enters RateLimited when the selected transport successfully reaches Zen
but Zen returns rate-limit semantics.

Signals:

- HTTP 429
- upstream error body indicating `FreeUsageLimitError`
- valid `Retry-After` header

Policy:

- respect `Retry-After` when available.
- do not aggressively probe before retry window.
- keep this separate from Dead; 429 means the proxy works but its upstream quota
  is limited.

## Dead Pool

A node enters Dead when the proxy path itself is unhealthy.

Signals:

- SOCKS handshake failure
- connection refused
- DNS failure

Soft upstream failures must not enter Dead directly:

- upstream 5xx
- app-level upstream busy
- empty assistant output
- request timeout after the request reached the upstream path
- retry-budget exhaustion

These soft failures reduce score and node concurrency, but keep the node in
Dispatch. A node is buried only when evidence points to the proxy path itself,
not when Zen or the model returned a bad application result.

## Dead Probe Policy

Dead probing must be low-frequency and progressive.

Defaults:

```text
probe interval: 60-120 minutes with jitter
initial batch: max(1, dead_count * 1%)
if recent recovery rate >= 30%: double next batch
if recent recovery rate < 10%: reset to minimum batch
single batch cap: min(20, dead_count * 10%)
```

Recovery condition:

```text
2 consecutive successful probes
or
1 complete chat probe success with non-429 status
```

Implementation notes:

- automatic Dead probing is driven by `PoolManager::probe_dead_adaptive`.
- the background scheduler runs at low frequency; it does not call `probe_all`.
- probe batches are selected from due Dead entries only, using the V4 adaptive
  batch policy.
- probe requests go through the embedded `free-model-client-rs` kernel with the
  selected node's `reqwest::Client`.

## Probe Requirements

Probe requests must use the same transport class and provider request builder as
real requests. A probe that uses a different protocol path is not trusted.

Minimum probe body:

```json
{
  "model": "deepseek-v4-flash",
  "messages": [{"role": "user", "content": "Reply exactly: OK"}],
  "stream": false,
  "max_tokens": 32
}
```

The provider adapter resolves the public model before sending upstream.

## Anti-Patterns

- scanning all dead nodes every minute.
- using `probe_all` as the automatic Dead recovery loop.
- using probe traffic that does not include the same auth/header path as real
  requests.
- moving 429 nodes to Dead.
- burying nodes for upstream 5xx, empty-output, or retry-budget exhaustion.
- letting one globally best node monopolize dispatch selection.
