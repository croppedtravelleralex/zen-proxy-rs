# V4.0 Positioning and Scope

## Positioning

ZenProxyRS V4.0 is a Rust control plane for free and low-cost model access. It
is not just a pass-through proxy and it is not a model-quality enhancer.

Primary responsibility:

```text
OpenAI/Anthropic-compatible API
+ proxy rotation
+ model mapping
+ pool state
+ retry and backoff
+ request ledger
+ admin visibility
+ FreeModel Zen protocol kernel
```

## Why V4.0 Exists

The current project has overlapping responsibilities:

- `zen-proxy-rs` selects proxy nodes and also hand-builds upstream Zen requests.
- `free-model-client-rs` already contains a cleaner Rust implementation of the
  FreeModel-to-Zen protocol path.
- Directly chaining the two services over HTTP would break proxy egress
  semantics.

V4.0 resolves this by making `free-model-client-rs` a reusable protocol kernel
that runs inside the `zen-proxy-rs` request path.

## Goals

- Keep `zen-proxy-rs` as the single long-running process.
- Keep proxy rotation, dead/rate-limited pools, retry, fuse, admin, and metrics.
- Reuse the FreeModel Rust kernel for OpenAI/Anthropic to Zen translation.
- Ensure the selected proxy node is the client used for the real Zen request.
- Expose only two stable public models.
- Provide verifiable request records and rollback.

## Non-Goals

- No prompt injection.
- No new Node.js service.
- No HTTP sidecar dependency for the main path.
- No dashboard in V4.0.
- No SQLite requirement in V4.0.
- No broad model catalog.
- No attempt to bypass or defeat upstream service limits.

## Public Model Surface

Only these models are public:

```text
deepseek-v4-flash
deepseek-v4-flash-lite
```

Internal upstream mapping:

```text
deepseek-v4-flash      -> deepseek-v4-flash-free
deepseek-v4-flash-lite -> big-pickle
```

## Completion Standard

V4.0 is complete only when:

- OpenAI non-stream and stream work through the FreeModel kernel.
- Anthropic non-stream and stream work through the FreeModel kernel.
- The selected proxy node is used by the Zen request.
- `429` moves a node to RateLimited.
- timeout or proxy connection failure moves a node to Dead.
- Dead probing uses the V4.0 low-frequency progressive policy.
- Legacy mode can be restored by configuration.

