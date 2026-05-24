# V4.0 Implementation Plan

## Phase 0 - Contract Freeze

Goal: lock the V4.0 target before changing runtime behavior.

Tasks:

- Add `ModelRegistry` contract and two-model mapping.
- Define `RequestContext`, `UpstreamOutcome`, and `RequestRecord`.
- Define `ProviderAdapter`, `FreeModelKernel`, `TransportProvider`, and
  `DeadProbePolicy` interfaces.
- Add feature/config switch:

```text
ZEN_PROVIDER_MODE=legacy | free_model_kernel
```

Acceptance:

- Code compiles with new interfaces.
- Legacy behavior remains default.

## Phase 1 - Extract FreeModelKernel

Goal: make `free-model-client-rs` reusable without HTTP sidecar coupling.

Tasks:

- Move protocol translation into a library module.
- Make Zen requests accept an externally supplied `reqwest::Client`.
- Preserve the existing HTTP server as a thin wrapper.
- Add golden tests for OpenAI/Anthropic non-stream and stream.

Acceptance:

- `free-model-client-rs` tests pass.
- Kernel can be called with a caller-provided client.

## Phase 2 - Add FreeModelProviderAdapter

Goal: let `zen-proxy-rs` call the FreeModel kernel after selecting transport.

Tasks:

- Add `FreeModelProviderAdapter`.
- Convert public model names to upstream model names through `ModelRegistry`.
- Route OpenAI and Anthropic public handlers through the adapter when
  `ZEN_PROVIDER_MODE=free_model_kernel`.
- Keep legacy path available.

Acceptance:

- OpenAI non-stream and stream work in both legacy and FreeModel modes.
- Anthropic non-stream and stream work in both modes.

## Phase 3 - Transport Proof and Pool Outcomes

Goal: prove that selected proxy nodes are used for real upstream calls.

Tasks:

- Add test-mode egress proof using selected node metadata and observed IP.
- Normalize provider outcomes into `UpstreamOutcome`.
- Map 429 to RateLimited.
- Map transport failures to Dead.
- Preserve sticky retry.

Acceptance:

- A selected SOCKS node is the egress path for the Zen request.
- 429, timeout, and success each produce the correct pool transition.

## Phase 4 - DeadProbePolicy

Goal: replace aggressive dead probing with low-frequency progressive probing.

Tasks:

- Implement 60-120 minute jittered schedule.
- Implement adaptive batch sizing.
- Require two successful probes or one complete non-429 chat success for
  recovery.

Acceptance:

- Dead pool is not scanned continuously.
- Recovery expands probing only when recent recovery rate is high.

## Phase 5 - Observability Unification

Goal: establish one request truth source.

Tasks:

- Introduce canonical `RequestRecord`.
- Align ledger, collector, metrics, and admin request views.
- Keep derived counters as caches only.

Acceptance:

- Admin request detail, metrics, and WAL refer to the same request id and node id.

## Phase 6 - Release Gate

Goal: make V4.0 safe to turn on by default.

Tasks:

- Run `cargo fmt --check`.
- Run `cargo clippy --all-targets -- -D warnings`.
- Run `cargo test`.
- Run `cargo build --release`.
- Run mock Zen and fault-injection acceptance suite.

Acceptance:

- All gates pass.
- `ZEN_PROVIDER_MODE=free_model_kernel` can be enabled and rolled back.

