# V4.0 Acceptance and Risks

## Completion Definition

V4.0 is complete only when these claims are all backed by fresh evidence:

```text
contracts are explicit
selected egress is provable
protocol behavior is golden-tested
failure behavior is injected and verified
rollout is configurable and reversible
release gates pass
```

## Acceptance Matrix

| Gate | Required Evidence | Pass Criteria |
|---|---|---|
| Structure | file/module inspection | protocol logic is outside `proxy.rs`; pool does not build Zen bodies |
| Config | unit tests | `ZEN_PROVIDER_MODE` defaults to legacy; V4 mode can be enabled |
| Model API | API test | `/v1/models` returns only `deepseek-v4-flash` and `deepseek-v4-flash-lite` |
| Model Mapping | unit/API tests | flash maps to `deepseek-v4-flash-free`; flash-lite maps to `big-pickle` |
| OpenAI Non-Stream | mock Zen integration | valid OpenAI response, correct model, usage if available |
| OpenAI Stream | golden SSE test | valid SSE frames, `[DONE]`, TTFT recorded |
| Anthropic Non-Stream | mock Zen integration | valid Anthropic message response |
| Anthropic Stream | golden SSE test | valid Anthropic event sequence |
| Tool Call | golden fixture | tool deltas are preserved or fallback is deterministic |
| Reasoning-Only | golden fixture | no hidden reasoning leak; structured fallback/error behavior |
| Egress Proof | test-mode proof | selected node id matches observed egress evidence |
| 429 Handling | fault injection | node moves to RateLimited; retry-after recorded |
| Transport Failure | fault injection | timeout/refused/SOCKS failure moves node to Dead |
| Sticky Retry | fault injection | retry tries same node before spending another node |
| Dead Probe | scheduler tests | 60-120 minute jitter and adaptive batch rules hold |
| Context Ingress | e2e request-size test | >2MB bodies reach V4 handler under configured limit |
| Context Governance | e2e compaction test | old tool output is trimmed before upstream; latest user message preserved |
| Context Observability | admin request detail | original/effective bytes, token estimate, action, cache stats, trace recorded |
| No-Retry Semantics | fault injection | `POOL_MAX_RETRIES=0` sends exactly one upstream request |
| Observability | admin/WAL/metrics check | same request id appears across request detail, WAL, metrics-derived data |
| Rollback | runtime drill | switch between legacy and FreeModel mode without rebuild |
| Release | commands | fmt, clippy, tests, release build pass |

## Structural Acceptance

Required:

- `proxy.rs` handles HTTP boundary and orchestration only.
- provider protocol logic lives in `ProviderAdapter`/`FreeModelKernel`.
- pool code owns node lifecycle only.
- transport code owns egress clients only.
- model mapping lives in `ModelRegistry`.
- request facts live in `RequestRecord`.

Rejected:

- FreeModel kernel creates an unrelated global client for production requests.
- Dead pool sends hand-built provider-specific probe bodies.
- Provider adapter mutates pool state directly.
- Transport adapter branches on model names.
- Observability code changes request control flow.

## API Acceptance

### `GET /v1/models`

Expected ids:

```text
deepseek-v4-flash
deepseek-v4-flash-lite
```

No upstream-only model ids should appear.

### `POST /v1/chat/completions`

Required cases:

- `stream=false`
- `stream=true`
- unknown model
- request with tools
- reasoning-only upstream output
- upstream 429

### `POST /v1/messages`

Required cases:

- `stream=false`
- `stream=true`
- system field
- tool definitions
- tool result message
- upstream 429

## Egress Acceptance

Must prove:

```text
selected node -> selected transport client -> actual upstream request
```

Minimum request record fields:

```text
request_id
selected_node_id
selected_node_url_redacted
observed_exit_ip, when available
upstream_status
```

If Zen cannot expose the observed IP, use a controlled auxiliary endpoint in
test mode. Production may omit `observed_exit_ip`, but tests must still prove
transport selection.

Without this proof, V4.0 is not complete.

## Failure Acceptance

Fault injection must cover:

| Fault | Expected Result | Required Record |
|---|---|---|
| 429 | node enters RateLimited | status, retry_after, node id |
| FreeUsageLimitError | node enters RateLimited | error type, node id |
| timeout | node enters Dead | transport error kind, node id |
| connection refused | node enters Dead | transport error kind, node id |
| SOCKS failure | node enters Dead | transport error kind, node id |
| 500 | retry/backoff, then policy result | retry count, final outcome |
| partial SSE | structured stream error | request id and stream error |
| broken JSON | structured upstream parse error | request id and parse error |
| slow first token | response succeeds or timeout policy applies | TTFT |

## Dead Probe Acceptance

Required defaults:

```text
probe interval: 60-120 minutes with jitter
initial batch: max(1, dead_count * 1%)
if recovery rate >= 30%: double next batch
if recovery rate < 10%: reset to minimum
single batch cap: min(20, dead_count * 10%)
recovery: 2 successful probes or 1 complete non-429 chat success
```

Tests must prove:

- dead nodes are not scanned continuously.
- RateLimited nodes are not mixed into Dead probing.
- probes use the same provider and transport path as real requests.
- recovered nodes return through the expected pool transition.

## Observability Acceptance

Admin and storage must agree on request facts.

Required admin checks:

```text
GET /admin/health
GET /admin/pools
GET /admin/requests
GET /admin/requests/{request_id}
GET /admin/events
GET /admin/config
```

Required fields in request detail:

```text
public_model
upstream_model
protocol
stream
selected_node_id
selected_node_url_redacted
observed_exit_ip, when available
status
outcome
retry_count
latency_total_ms
upstream_ms
ttft_ms, for stream
context.original_body_bytes
context.effective_body_bytes
context.estimated_prompt_tokens
context.message_count
context.tools_count
context.largest_message_bytes
context.tool_result_bytes
context.action
context.trimmed_bytes
context.artifact_cache_hits
context.artifact_cache_writes
context.trace
```

No secrets may be emitted in admin, metrics, or WAL output.

## Release Acceptance

Required commands:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

Performance and process checks must use release artifacts.

Suggested baseline:

```text
process count: 1
idle RSS target: <= 50 MB unless justified
proxy overhead P95 target: <= 30 ms excluding upstream/model latency
```

## Large Context Acceptance

ZenProxyRS must separate ingress capability from upstream forwarding safety.

Required defaults:

```text
REQUEST_BODY_LIMIT_MB=64
CONTEXT_WARN_BODY_MB=24
CONTEXT_COMPACT_BODY_MB=30
CONTEXT_TARGET_BODY_MB=26
CONTEXT_UPSTREAM_BODY_LIMIT_MB=32
CONTEXT_TOKEN_WARN=600000
CONTEXT_TOKEN_COMPACT=850000
CONTEXT_TOKEN_TARGET=750000
ZEN_COMPACTOR_MODE=observe
ZEN_ARTIFACT_CACHE_MODE=metadata
```

Required evidence:

- a 3MB request is no longer rejected by Axum's default body limit.
- observe mode records that compaction would happen but does not mutate the body.
- enforce mode trims old tool output before upstream dispatch.
- the latest user message and recent tool chain are preserved.
- if a 32-64MB body cannot be safely reduced below the upstream-safe budget, the
  response is a structured 413.
- artifact cache is limited to large repeated content and has TTL/disk caps.

## Rollback Acceptance

V4.0 must keep a runtime switch until full acceptance:

```text
ZEN_PROVIDER_MODE=legacy
ZEN_PROVIDER_MODE=free_model_kernel
```

Rollback requirements:

- no rebuild.
- no data migration required.
- admin config shows active mode.
- request records include active provider mode.

## Known Shortcomings

- V4.0 does not make the upstream model smarter.
- V4.0 does not bypass upstream rate limits.
- Proxy quality remains an external constraint.
- Anthropic streaming and tool-call compatibility are the highest-risk protocol
  areas.
- Observed exit IP may require a test endpoint when Zen does not expose it.

## Risk Register

| Risk | Impact | Mitigation |
|---|---|---|
| FreeModel kernel keeps global client ownership | proxy rotation silently fails | kernel API must require caller-provided client |
| Anthropic stream edge cases drift | Claude Code compatibility breaks | golden event sequence tests |
| 429 and Dead are mixed | useful nodes get buried or bad nodes keep retrying | explicit `UpstreamOutcome` mapping tests |
| old legacy code remains active accidentally | V4 behavior is inconsistent | provider mode gate and request record mode field |
| observability has two truth sources | admin/debugging becomes unreliable | canonical `RequestRecord` |
| rollback is not tested | production recovery is slow | rollback drill in release gate |
| ingress limit is raised without compaction | upstream 32MB limit still fails | budgeter and enforce-mode compaction |
| compaction removes current context | answer quality drops | preserve latest user message and recent tool chain |
| cache grows without bounds | disk pressure | TTL, LRU cleanup, metadata default |
| summary is used too early | lost detail and wrong answers | structural trimming before any semantic summary |

## Score Target

The V4.0 implementation targets a 99-point engineering standard. The remaining
uncertainty is long-running production evidence after acceptance passes.
