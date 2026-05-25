# ZenProxyRS V4.0 AI Entry

## One-Line Status

ZenProxyRS V4.0 is a single-process Rust proxy control plane that keeps
`zen-proxy-rs` responsible for proxy rotation, pool state, retry, admin, and
observability, while moving Zen protocol adaptation into a reusable
`free-model-client-rs` kernel.

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
