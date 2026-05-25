# ZenProxyRS V4.0 AI Entry

## One-Line Status

ZenProxyRS V4.0/V4.1-A is a single-process Rust proxy control plane that keeps
`zen-proxy-rs` responsible for proxy rotation, pool state, retry, admin, and
observability, while moving Zen protocol adaptation into a reusable
`free-model-client-rs` kernel. V4.1-A has landed real node latency scoring,
retry-chain/failure telemetry, a bounded V4 retry budget, and the first
durable 99+ audit ledger path.

## Current Goal

Replace the current hand-built Zen reverse-proxy path in `zen-proxy-rs` with a
FreeModel kernel integration, without losing proxy rotation or operational
visibility.

Target chain:

```text
Client / external gateway / Claude Code
-> ZenProxyRS V4.0 Public API
-> Auth / RequestContext / ModelRegistry
-> PoolManager selects a proxy node
-> Transport creates or reuses a per-node reqwest::Client
-> FreeModelKernel builds and sends the Zen request with that client
-> Zen upstream
-> RequestLedger / Metrics / Admin API
-> Client
```

Rejected chain:

```text
zen-proxy-rs -> HTTP sidecar free-model-client-rs -> Zen
```

That sidecar shape is rejected because the true Zen egress IP would belong to
the sidecar's own client, not the proxy node selected by `zen-proxy-rs`.

## Version Boundary

V4.0 replaces all older documentation. Do not revive archived legacy docs or
root-level legacy audit reports as active guidance.

## Required Reading Order

1. [Positioning and Scope](./01_positioning_and_scope.md)
2. [Architecture](./02_architecture.md)
3. [Contracts and Interfaces](./03_contracts_and_interfaces.md)
4. [Request Flow](./04_request_flow.md)
5. [Implementation Plan](./08_implementation_plan.md)
6. [Acceptance and Risks](./09_acceptance_and_risks.md)
7. [2026-05-25 Operations Report](./10_2026-05-25_operations_report.md)

## Hard Decisions

- Public model list is limited to two names:
  - `deepseek-v4-flash`
  - `deepseek-v4-flash-lite`
- Model mapping:
  - `deepseek-v4-flash -> deepseek-v4-flash-free`
  - `deepseek-v4-flash-lite -> big-pickle`
- No prompt injection in V4.0.
- FreeModel behavior must be embedded as a kernel/library path, not an HTTP
  sidecar.
- The selected proxy node must be the transport used for the actual Zen request.
- Dead-pool probing is low-frequency and progressive, not continuous scanning.
- V4.0 must support rollback to the legacy path until acceptance is complete.
- NewAPI is not part of this repository and must not be modified when fixing
  ZenProxy behavior. The intended external chain is:
  `client -> NewAPI -> ZenProxyRS :4000 -> free-model-client kernel -> Zen`.

## Current Runtime Entry Points

Use these for local maintenance:

```text
ZenProxy public API: http://127.0.0.1:4000/v1
ZenProxy admin API:  http://127.0.0.1:4000/admin
ZenProxy admin key:  test-key
Proxy API key:       sk-dev
NewAPI base URL:     http://127.0.0.1:8081
NewAPI dev key:      sk-dev
```

Current systemd service:

```text
zen-proxy-rs.service
WorkingDirectory=/home/lenovo/zen-proxy-rs
ExecStart=/home/lenovo/zen-proxy-rs/target/release/zen-proxy-rs
```

## Current V4.1-A Evidence

Confirmed on 2026-05-25:

- `ZEN_PROVIDER_MODE=free_model_kernel`
- `V4_MODEL_REGISTRY_ENABLED=true`
- `REQUEST_BODY_LIMIT_MB=64`
- `ZEN_COMPACTOR_MODE=enforce`
- `V4_RETRY_BUDGET_MS=45000`
- `AUDIT_LOG_ENABLED=true`
- `AUDIT_LOG_DIR=/tmp/zen-proxy-audit` unless overridden
- NewAPI channel 19 is the active user path into ZenProxy.
- NewAPI logs show 940 calls on 2026-05-25 CST at the time of analysis.

## Verification Commands

Use these after implementation work:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

For V4.0 completion, code checks are not enough. The acceptance suite must also
prove that the observed Zen egress path matches the selected node.

## Runtime Data Sources

For operations analysis, use sources in this order:

1. NewAPI PostgreSQL `logs` table for user-visible call counts and durations.
2. ZenProxy `/admin/audit/*` for durable ZenProxy request history after the
   99+ audit ledger landed.
3. ZenProxy `/admin/requests/*` for current-process request detail and timings.
4. Redis `zprs:budget:*` keys for global node budget distribution.
5. `/tmp/zen-proxy-ledger-events.jsonl` for V4 ledger events from the current
   WAL file.
6. systemd service environment and logs for effective runtime configuration.
