# V4.0 Implementation Plan

## Execution Rule

Implement V4.0 as small, verifiable slices. Do not start by rewriting
`proxy.rs` end to end.

Default mode:

```text
legacy path remains default
new FreeModel kernel path is behind ZEN_PROVIDER_MODE=free_model_kernel
each phase must compile before the next phase starts
```

## Task Board

| ID | Phase | Task | Owner Scope | Depends On | Completion Evidence |
|---|---|---|---|---|---|
| T0.1 | 0 | Add V4.0 config switches | `src/config.rs` | none | config tests cover defaults and env overrides |
| T0.2 | 0 | Add V4.0 domain contracts | new `src/domain/*` or `src/core/*` | none | contracts compile without changing legacy path |
| T0.3 | 0 | Add two-model registry | model registry module | T0.2 | `/v1/models` fixture returns only two public models |
| T0.4 | 0 | Add provider mode gate | application/proxy service boundary | T0.1 | legacy remains default |
| T1.1 | 1 | Convert `free-model-client-rs` to library + binary | `free-model-client-rs` crate layout | T0.2 | binary still runs, library exports kernel API |
| T1.2 | 1 | Make kernel accept caller-provided client | FreeModel kernel transport boundary | T1.1 | unit test uses mock/per-node client wrapper |
| T1.3 | 1 | Add OpenAI golden fixtures | FreeModel kernel tests | T1.2 | non-stream and stream golden tests pass |
| T1.4 | 1 | Add Anthropic golden fixtures | FreeModel kernel tests | T1.2 | non-stream and stream golden tests pass |
| T1.5 | 1 | Lock tool-call and reasoning-only behavior | FreeModel kernel tests | T1.3/T1.4 | golden tests for tool delta and reasoning-only |
| T2.1 | 2 | Add `FreeModelProviderAdapter` | `zen-proxy-rs` provider layer | T1.2 | adapter compiles behind feature/mode gate |
| T2.2 | 2 | Route OpenAI through adapter in new mode | public API handler/app service | T2.1 | OpenAI non-stream and stream pass in both modes |
| T2.3 | 2 | Route Anthropic through adapter in new mode | public API handler/app service | T2.1 | Anthropic non-stream and stream pass in both modes |
| T2.4 | 2 | Preserve legacy rollback path | app service/config | T2.2/T2.3 | runtime mode switch changes path without rebuild |
| T3.1 | 3 | Normalize upstream outcomes | provider/app service boundary | T2.2 | success/429/5xx/transport errors share one enum |
| T3.2 | 3 | Wire outcome to pool transitions | pool manager integration | T3.1 | 429 -> RateLimited, timeout -> Dead tests pass |
| T3.3 | 3 | Prove selected egress | transport test mode | T2.1 | selected node id maps to observed egress evidence |
| T3.4 | 3 | Preserve sticky retry | retry policy/pool manager | T3.1 | retry tries same node before spending another node |
| T4.1 | 4 | Add `DeadProbePolicy` | policy layer | T3.2 | policy unit tests cover interval and batch sizes |
| T4.2 | 4 | Implement 60-120 minute jitter | dead/probe scheduler | T4.1 | no continuous scan in scheduler tests |
| T4.3 | 4 | Implement adaptive dead batch | dead/probe scheduler | T4.1 | recovery rate controls next batch |
| T4.4 | 4 | Require recovery proof | probe result handling | T4.2/T4.3 | two successes or one complete non-429 chat success |
| T5.1 | 5 | Introduce canonical `RequestRecord` | observability layer | T3.1 | record contains selected node and public/upstream model |
| T5.2 | 5 | Align ledger and collector | ledger/collector modules | T5.1 | admin, WAL, metrics share request id |
| T5.3 | 5 | Add admin V4.0 views | admin service/router | T5.2 | pools, requests, events, config endpoints work |
| T5.4 | 5 | Add exit proof reporting | admin/ledger | T3.3/T5.1 | request detail shows selected node and proof when available |
| T6.1 | 6 | Add mock Zen server tests | integration tests | T2.2/T2.3 | protocol tests run offline |
| T6.2 | 6 | Add fault injection suite | integration tests | T3.2 | 429/500/timeout/bad SSE are covered |
| T6.3 | 6 | Run release gate | whole repo | all | fmt, clippy, test, release build pass |
| T6.4 | 6 | Rollback drill | config/runtime | T2.4 | switch to legacy and back without rebuild |

## Phase 0 - Contract Freeze

Goal: lock the V4.0 target before changing runtime behavior.

Tasks:

- Add `ZEN_PROVIDER_MODE=legacy | free_model_kernel`.
- Add `V4_MODEL_REGISTRY_ENABLED`, defaulting to enabled only for V4 path if
  needed during migration.
- Define:
  - `RequestContext`
  - `ProtocolKind`
  - `UpstreamOutcome`
  - `RequestRecord`
  - `ModelRegistry`
  - `ProviderAdapter`
  - `TransportProvider`
  - `DeadProbePolicy`
- Add the two-model mapping:

```text
deepseek-v4-flash      -> deepseek-v4-flash-free
deepseek-v4-flash-lite -> big-pickle
```

Acceptance:

- `cargo check` passes.
- legacy mode remains default.
- unit tests prove unknown models return structured errors.
- no FreeModel code is wired into production path yet.

Stop line:

- If adding contracts requires changing existing proxy behavior, split the
  behavior change into Phase 2.

## Phase 1 - Extract FreeModelKernel

Goal: make `free-model-client-rs` reusable without HTTP sidecar coupling.

Tasks:

- Split `free-model-client-rs` into a library and binary.
- Keep existing HTTP server as a thin binary wrapper.
- Move translation, Zen header building, Zen body building, SSE parsing, response
  formatting, and fallback synthesis into the library.
- Replace global client ownership with caller-provided client usage.
- Add golden fixtures for:
  - OpenAI non-stream
  - OpenAI stream
  - Anthropic non-stream
  - Anthropic stream
  - tool-call delta
  - reasoning-only
  - 429 error body

Acceptance:

- `free-model-client-rs cargo test` passes.
- HTTP binary behavior remains available.
- library API can be called with a provided `reqwest::Client`.
- no prompt injection exists.

Stop line:

- If the kernel cannot accept a caller-provided client, do not proceed. That
  would break V4.0 egress semantics.

## Phase 2 - Integrate FreeModelProviderAdapter

Goal: let `zen-proxy-rs` call the FreeModel kernel after selecting transport.

Tasks:

- Add `FreeModelProviderAdapter`.
- Resolve public models through `ModelRegistry`.
- Keep public request handlers thin.
- Use selected per-node transport client for kernel calls.
- Keep `ZEN_PROVIDER_MODE=legacy` rollback.

Acceptance:

- OpenAI non-stream works in legacy and FreeModel modes.
- OpenAI stream works in legacy and FreeModel modes.
- Anthropic non-stream works in legacy and FreeModel modes.
- Anthropic stream works in legacy and FreeModel modes.
- `/v1/models` returns only the two public models in V4 mode.

Stop line:

- If adapter code starts mutating pools directly, stop and move that logic to
  application service/policy.

## Phase 3 - Transport Proof and Pool Outcomes

Goal: prove that selected proxy nodes are used for real upstream calls.

Tasks:

- Add test-mode egress proof.
- Normalize all provider results into `UpstreamOutcome`.
- Map outcomes to pool transitions.
- Preserve sticky retry.
- Record selected node id and redacted URL for every request.

Acceptance:

- Selected SOCKS node is the egress path for the upstream request in test mode.
- 429 moves node to RateLimited.
- timeout or proxy connection failure moves node to Dead.
- success returns node to Dispatch.
- retry tries the same node first when policy allows.

Stop line:

- If egress proof cannot be produced, V4.0 cannot be marked complete.

## Phase 4 - DeadProbePolicy

Goal: replace aggressive dead probing with low-frequency progressive probing.

Policy defaults:

```text
probe interval: 60-120 minutes with jitter
initial batch: max(1, dead_count * 1%)
if recent recovery rate >= 30%: double next batch
if recent recovery rate < 10%: reset to minimum batch
single batch cap: min(20, dead_count * 10%)
recovery: 2 consecutive successful probes or 1 complete non-429 chat success
```

Acceptance:

- dead pool is not scanned continuously.
- batch size increases only when recent recovery is high.
- RateLimited nodes are not treated as Dead nodes.
- probes use the same provider/transport path as real requests.

Stop line:

- If probe traffic uses a different request builder than real traffic, probe
  results are not trusted.

## Phase 5 - Observability Unification

Goal: establish one request truth source.

Tasks:

- Introduce canonical `RequestRecord`.
- Align ledger, collector, metrics, and admin request views.
- Keep derived counters as caches only.
- Add request detail fields:
  - public model
  - upstream model
  - selected node id
  - selected node URL redacted
  - observed exit IP when available
  - retry count
  - outcome
  - TTFT

Acceptance:

- admin request detail, metrics, and WAL refer to the same request id.
- no endpoint depends on a separate incompatible request model.
- high-cardinality raw secrets are not emitted.

Stop line:

- If two systems disagree on request status or node id, resolve before moving to
  release gate.

## Phase 6 - Release Gate

Goal: make V4.0 safe to turn on by default.

Tasks:

- Run formatting and lint gates.
- Run unit and integration tests.
- Run mock Zen and fault-injection acceptance suite.
- Run release build.
- Run rollback drill.

Commands:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

Acceptance:

- all commands pass.
- `ZEN_PROVIDER_MODE=free_model_kernel` can be enabled and rolled back.
- release artifact is used for performance checks.
- V4.0 acceptance matrix is fully checked.

## Suggested Commit Shape

Use small commits:

```text
feat(v4): add provider mode and core contracts
feat(v4): add model registry
feat(free-model): extract reusable kernel
feat(v4): add FreeModel provider adapter
feat(v4): wire pool outcomes to provider results
feat(v4): add dead probe policy
feat(v4): unify request records
test(v4): add mock zen fault suite
chore(v4): pass release gate
```

Avoid commits that mix contracts, adapter wiring, pool behavior, and admin
changes at the same time.

