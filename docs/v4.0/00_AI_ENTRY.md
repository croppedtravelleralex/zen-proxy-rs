# ZenProxyRS V4.0 AI Entry

## One-Line Status

ZenProxyRS V4.0/V4.4 is a Rust proxy control plane that keeps `zen-proxy-rs`
responsible for proxy rotation, pool state, retry, admin, and observability,
while moving Zen protocol adaptation into a reusable `free-model-client-rs`
kernel. V4.3 moved the scaling path from coarse "more full processes" toward a
measurable data plane: lanes, sharded dispatch, node registry, adaptive node
concurrency, and reduced Redis budget overhead. V4.4 hardens pool fault
classification so upstream 5xx, timeout, empty-output, and retry-budget
failures do not incorrectly bury healthy proxy nodes. V4.5 adds cache-affinity
and effective-TTFT routing so repeated large contexts prefer paths that have
already shown good cache and first-content behavior. V4.6 adds a protocol graph
guard, pair-aware compactor, source-side quality policy, non-stream output
guard, long-request lane isolation, and split timing metrics so malformed
tool-call history and large non-stream requests are handled before they can
stall the production path. Compatibility targets include Hermes, OpenClaw,
Claude Code, CherryStudio, OpenAI SDKs, and Anthropic SDKs.

## Current Goal

Replace the current hand-built Zen reverse-proxy path in `zen-proxy-rs` with a
FreeModel kernel integration, without losing proxy rotation or operational
visibility.

Target chain:

```text
Client / external gateway / Claude Code
-> NewAPI or direct OpenAI-compatible client
-> ZenProxyRS on http://127.0.0.1:4000
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

Latest handoff status must be checked in [2026-05-30 Handoff And Unfinished Work](./19_2026-05-30_handoff_and_unfinished_work.md) and then [2026-06-01 Final-Anchor Deployment And Smoke](./20_2026-06-01_final_anchor_deploy.md) before making any claim about panda NewAPI, Hermes, OpenClaw, closeapi, ClaudeCode huge-context behavior, or V4.8 acceptance progress.

## Required Reading Order

1. [Positioning and Scope](./01_positioning_and_scope.md)
2. [Architecture](./02_architecture.md)
3. [Contracts and Interfaces](./03_contracts_and_interfaces.md)
4. [Request Flow](./04_request_flow.md)
5. [Implementation Plan](./08_implementation_plan.md)
6. [Acceptance and Risks](./09_acceptance_and_risks.md)
7. [2026-05-25 Operations Report](./10_2026-05-25_operations_report.md)
8. [V4.3 Scalable Data Plane](./11_v4.3_scalable_data_plane.md)
9. [V4.4 Pool Fault Isolation](./12_v4.4_pool_fault_isolation.md)
10. [V4.5 Cache Affinity and Effective TTFT](./13_v4.5_cache_affinity_ttft.md)
11. [V4.6 Protocol Graph Guard and Pair-Aware Compactor](./14_v4.6_protocol_guard.md)
12. [V4.7 Test Records and Client Acceptance](./15_test_records_and_client_acceptance.md)
13. [V4.5 P8 95+ Acceptance Plan](./16_v4.5_p8_95_plus_acceptance.md)
14. [V4.6 99+ Runtime Policy](./17_v4.6_99plus_runtime_policy.md)
15. [V4.8 Four-Client 500-Round Acceptance](./18_v4.8_four_client_500_round_acceptance.md)
16. [2026-05-30 Handoff And Unfinished Work](./19_2026-05-30_handoff_and_unfinished_work.md)
17. [2026-06-01 Final-Anchor Deployment And Smoke](./20_2026-06-01_final_anchor_deploy.md)

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
- Protocol compatibility fixes belong in ZenProxy/free-model-client, not in
  NewAPI or Claude Code. Tool-call history must be repaired or downgraded before
  it reaches the DeepSeek-compatible upstream.

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

Current runtime services:

```text
Current panda deployment:
nginx.service: listens on 0.0.0.0:4000 and load-balances ZenProxy backends
zen-proxy-rs@1.service: 127.0.0.1:4001, INSTANCE_ID=panda-zen-1
zen-proxy-rs@2.service: 127.0.0.1:4002, INSTANCE_ID=panda-zen-2
zen-proxy-rs@3.service: 127.0.0.1:4004, INSTANCE_ID=panda-zen-3
redis-server.service: 127.0.0.1:6379 for V4.3 global node budget
```

## Current V4.1-A Evidence

Confirmed on 2026-05-25:

- `ZEN_PROVIDER_MODE=free_model_kernel`
- `V4_MODEL_REGISTRY_ENABLED=true`
- `REQUEST_BODY_LIMIT_MB=64`
- `ZEN_COMPACTOR_MODE=enforce`
- `V4_RETRY_BUDGET_MS=120000`
- `V1_MAX_CONCURRENT_REQUESTS=12` per ZenProxy instance
- `AUDIT_LOG_ENABLED=true`
- `AUDIT_LOG_DIR=/tmp/zen-proxy-audit` unless overridden
- V4.3 lane isolation is implemented in code but must be enabled through
  `V43_LANES_ENABLED=true` and process restart.
- V4.3 async collector is implemented but disabled by default through
  `V43_ASYNC_COLLECTOR_ENABLED=false`; enable only after release-mode soak.
- V4.3 request history no longer uses the old unsafe ring buffer.
- V4.3 dispatch uses configurable ready-node shards through
  `V43_DISPATCH_SHARDS`.
- V4.3 node concurrency uses AIMD controls:
  `V43_NODE_MIN_CONCURRENCY`, `V43_NODE_MAX_CONCURRENCY`,
  `V43_AIMD_SUCCESS_STEP`, `V43_AIMD_FAILURE_PERCENT`,
  `V43_AIMD_SLOW_LATENCY_MS`.
- V4.3 global budget mode is explicit through `V43_GLOBAL_BUDGET_MODE`; keep
  `sync_redis` for multi-instance runtime and use `off` only for single-process
  profiles.
- V4.3 global budget fail-open is explicit through
  `V43_GLOBAL_BUDGET_FAIL_OPEN`; keep it true so Docker Redis restart windows
  degrade to local budget instead of no available node.
- `/admin/runtime` exposes `data_plane.node_registry` and
  `data_plane.transport` for the current process.
- NewAPI channel 19 is the active user path into ZenProxy.

Current V4.4 verification on 2026-05-26:

- `panda` now uses nginx on port 4000 in front of three ZenProxyRS instances.
- deployed binary SHA1 after TTFT-aware dispatch update:
  `b4d8fdcb2f922690e9903b0b83a0eaafda826c37`.
- `V4_RETRY_BUDGET_MS=60000`.
- each instance uses `V43_GLOBAL_BUDGET_MODE=sync_redis` and
  `GLOBAL_BUDGET_REDIS_URL=redis://127.0.0.1:6379/`.
- after multi-instance rollout, all three instances showed `dispatch=90`,
  `dead=0`, `active=0`, `leased=0`.
- V4 stream dispatch now records first-chunk latency as a non-releasing node
  hint, so long streams no longer teach the pool only by full stream duration.
- Large streaming requests increase dispatch score weight for recent node
  latency. This is a source-side optimization only; NewAPI and Claude Code are
  unchanged.
- Public cache research and production data agree on the boundary: sub-second
  TTFT for 100k-500k context requires real upstream prompt/KV cache hits and a
  fast selected node. ZenProxy can improve node choice and avoid extra delay,
  but cannot force a slow upstream cache read into 1s.
- V4.5 landed after that slice: stream telemetry now splits first-frame,
  first-content, and first-tool-call timing; large streaming requests get
  cache-affinity routing; node learning now tracks request-size buckets.
- Deployed V4.5 production binary SHA1:
  `2b11dbb19861b1d0f16597d1841678d4ad3e3173`.
- V4.5 production smoke confirmed `/admin/audit/requests` records
  `affinity_key`, `affinity_hit`, `body_size_bucket`, and
  `timings.first_content_token_ms`; a repeated 150KB same-path stream improved
  from about 2.33s first chunk to about 1.09s first chunk with
  `affinity_hit=true`.
- V4.6 landed and is deployed on `panda` after V4.5: `ProtocolGuard` repairs or downgrades
  malformed OpenAI/Anthropic tool history before typed free-model-client
  conversion, and the final guard pass runs after context compaction so orphan
  tool results are not sent upstream. Deployed release SHA1:
  `27b14d90f6c9ebaccc6252ec896bc3593d2080f7`; production malformed
  `tool_call_id` smoke returned HTTP 200 and audit showed `post_valid=true`.
- V4.6 follow-up found one more boundary in production Hermes-style traffic:
  raw Anthropic `/v1/messages` guard could pass while the embedded
  free-model-client Anthropic-to-OpenAI translation later produced an upstream
  OpenAI `role=tool` message without `tool_call_id`. The fix is a kernel-side
  OpenAI tool-history canonicalizer in `free-model-client-rs`, applied both to
  direct OpenAI requests and after Anthropic translation. No NewAPI or client
  workaround is part of the fix.
- V4.6 second follow-up tightened the kernel translator for real
  Hermes/OpenClaw tool histories: mixed Anthropic `text + tool_result` user
  content is translated with OpenAI tool messages adjacent to the preceding
  assistant tool call, and the OpenAI canonicalizer refuses to pair tool
  results across an intervening non-tool message. This prevents strict upstream
  errors caused by invalid tool-pair ordering, not just missing IDs.
- Local V4.6 second follow-up release SHA1:
  `8bfd54f83f4f25555bcdc93ec6e9466e0a7d1f95`. Hermes/OpenClaw local smokes now
  reach ZenProxy through NewAPI but currently fail on Webshare-to-Zen
  `transport_error`, not protocol guard validation.
- 2026-05-26 production smoke after deploy: direct ZenProxy stream returned
  HTTP 200 with `time_starttransfer=1.410657s`; recent 220k+ cached-token
  audit rows were still around 2.4-4.0s TTFT, showing the remaining bottleneck
  is mainly upstream/cache/node path quality.
- NewAPI `http://127.0.0.1:8081` with key `sk-dev` returned HTTP 200 for
  `deepseek-v4-flash` and `deepseek-v4-flash-lite`, both streaming and
  non-streaming.
- a 12-call NewAPI smoke distributed evenly through nginx:
  4 calls on `zen-proxy-rs@1`, 4 on `zen-proxy-rs@2`, and 4 on
  `zen-proxy-rs@3`.

Current V4.6 99+ local source-side update on 2026-05-28:

- `free-model-client-rs` no longer disables thinking by default for ordinary
  requests and no longer rewrites terse prompts such as `1`, `继续`, or `执行`.
- tool-use requests still set `thinking={"type":"disabled"}` to keep JSON/tool
  deltas stable.
- ZenProxy V4 NonStream Output Guard caps large `stream=false` outputs before
  upstream dispatch and rejects huge prompt plus very large output combinations
  with a clear 422.
- V4 lanes now expose and isolate `long_nonstream`, `long_output`, and
  `tool_heavy` lanes.
- `/admin/requests/timings` now exposes `protocol_first_byte_ms`,
  `first_content_token_ms`, and `first_tool_call_ms` averages and recent rows.

Latest V4.3 verification on 2026-05-25:

- `cargo fmt --check`, `cargo check`, `cargo test`, and
  `cargo build --release` passed.
- systemd runtime uses `/home/lenovo/zen-proxy-rs/target/release/zen-proxy-rs`.
- `zen-proxy-rs@1`, `zen-proxy-rs@2`, `zen-proxy-rs@3`, and `nginx` restarted
  active.
- `http://127.0.0.1:4000/v1/models` returns only the two public models.
- direct ZenProxy chat and NewAPI -> ZenProxy chat both returned HTTP 200.
- `/admin/runtime` shows `data_plane.node_registry.nodes=100`,
  `v43_lanes.enabled=true`, `v43_lanes.dispatch_shards=16`, and
  `v43_lanes.global_budget_mode=sync_redis`.
- After the no-reply incident on 2026-05-25, V4.3 global budget fail-open was
  enabled and verified: a temporary instance with Redis pointed at a dead port
  still returned HTTP 200 by degrading to local node budget, while logging
  `global budget unavailable; failing open to local budget`.
- NewAPI logs show 940 calls on 2026-05-25 CST at the time of analysis.
- Nginx uses `least_conn` over `127.0.0.1:4001`, `127.0.0.1:4002`,
  and `127.0.0.1:4004`. Port 4003 was skipped because it hit a local
  `AddrInUse` condition during rollout.

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

For V4.5/P8 evidence runs, use the unified runner:

```bash
scripts/run_v45_p8_acceptance.sh plan
NEWAPI_API_KEY=sk-dev scripts/run_v45_p8_acceptance.sh smoke
NEWAPI_API_KEY=sk-dev scripts/run_v45_p8_acceptance.sh clients
NEWAPI_API_KEY=sk-dev scripts/run_v45_p8_acceptance.sh ttft --tokens 1000
```

The full P0-P8 matrix, Windows/WSL/panda workflows, and NewAPI/ZenProxy
reconciliation rules are maintained in
[V4.5 P8 95+ Acceptance Plan](./16_v4.5_p8_95_plus_acceptance.md).

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
7. `test-records/runs/<run_id>/summary.md` for user-visible acceptance run
   summaries created by `scripts/collect_test_record.py`.

## Repository Hygiene Notes

- `test-records/runs/` is intentionally ignored because it contains generated
  acceptance evidence packages.
- `target-1.86/` is now ignored for future local build artifacts, but files
  already tracked under that directory still need a separate index cleanup.
