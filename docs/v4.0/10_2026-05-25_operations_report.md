# 2026-05-25 Operations Report

## Scope

This report records the V4.1-A maintenance state and analyzes NewAPI calls for
2026-05-25 CST. It is an AI maintenance handoff document for future work on
`zen-proxy-rs`.

## Current Chain

The active runtime chain is:

```text
Claude Code / Cherry Studio / client
-> NewAPI http://127.0.0.1:8081
-> channel 19 Zenproxyrs4.0
-> ZenProxyRS http://127.0.0.1:4000
-> free_model_kernel
-> selected proxy node
-> Zen upstream
```

NewAPI is only the external gateway. Do not fix ZenProxy bugs by changing
NewAPI unless the user explicitly asks.

## V4.1-A Landed State

Confirmed code/runtime changes:

- Node release now records real upstream latency into pool scoring.
- `/admin/requests/timings` and recent request records expose
  `failure_kind` and `retry_chain`.
- `V4_RETRY_BUDGET_MS` is available through config and defaults to `45000`.
- `zen-proxy-rs.service` runs the release binary from
  `/home/lenovo/zen-proxy-rs/target/release/zen-proxy-rs`.
- Active config includes `ZEN_PROVIDER_MODE=free_model_kernel`,
  `V4_MODEL_REGISTRY_ENABLED=true`, Redis global budget, 64 MB ingress limit,
  and enforced context governance.

Verification already performed:

```text
cargo fmt
cargo check
cargo test
cargo build --release
NewAPI streaming request through http://127.0.0.1:8081 succeeded
ZenProxy admin telemetry showed retry_chain and real node avg_latency_ms
```

Commit:

```text
c0ca126 feat: add v4 retry diagnostics and latency scoring
```

## 2026-05-25 NewAPI Call Analysis

Data source:

```text
Container: new-api-postgres
Database: new-api
Table: logs
Window: created_at >= 1779638400 and < 1779724800
Channel: 19
Timezone: Asia/Shanghai
```

At the time of analysis, NewAPI showed 940 calls for 2026-05-25. All 940 were
on channel 19.

Summary:

| Metric | Value |
|---|---:|
| Total calls | 940 |
| Channel 19 calls | 940 |
| `deepseek-v4-flash` calls | 935 |
| `deepseek-v4-flash-lite` calls | 5 |
| Prompt tokens | 35,780,845 |
| Completion tokens | 66,366 |
| Total tokens | 35,847,211 |
| Average duration | 8.48s |
| Max duration | 100s |
| P50 duration | 6s |
| P90 duration | 16s |
| P95 duration | 19.05s |
| P99 duration | 27s |
| P50 prompt tokens | 20,502 |
| P90 prompt tokens | 106,081.6 |
| P95 prompt tokens | 120,957.3 |
| Max prompt tokens | 262,161 |

Stream split:

| Stream | Calls | Prompt Tokens | Completion Tokens | Avg Duration | Max Duration |
|---|---:|---:|---:|---:|---:|
| true | 662 | 34,731,187 | 64,348 | 9.46s | 32s |
| false | 278 | 1,049,658 | 2,018 | 6.15s | 100s |

Model split:

| Model | Stream | Calls | Avg Prompt | Avg Completion | Avg Duration | Max Duration |
|---|---:|---:|---:|---:|---:|---:|
| deepseek-v4-flash | true | 661 | 52,543.4 | 97.3 | 9.47s | 32s |
| deepseek-v4-flash | false | 274 | 3,830.2 | 7.2 | 6.20s | 100s |
| deepseek-v4-flash-lite | false | 4 | 48.8 | 13.8 | 2.50s | 4s |
| deepseek-v4-flash-lite | true | 1 | 1.0 | 3.0 | 4.00s | 4s |

Path split:

| Path | Semantic | Calls | Stream | Non-Stream | Avg FRT | Max FRT |
|---|---|---:|---:|---:|---:|---:|
| `/v1/messages` | anthropic | 916 | 655 | 261 | 3091.6ms | 19079ms |
| `/v1/chat/completions` | anthropic | 21 | 5 | 16 | -210.8ms | 2862ms |
| `/v1/chat/completions` | unknown | 3 | 2 | 1 | 1369.3ms | 2874ms |

Hourly distribution in CST:

| Hour | Calls | Stream | Non-Stream | Avg Duration | Max Duration | Tokens |
|---:|---:|---:|---:|---:|---:|---:|
| 00 | 8 | 6 | 2 | 6.13s | 9s | 106,374 |
| 08 | 1 | 1 | 0 | 3.00s | 3s | 3 |
| 09 | 14 | 10 | 4 | 4.71s | 14s | 157,325 |
| 10 | 100 | 91 | 9 | 4.98s | 12s | 974,433 |
| 11 | 178 | 48 | 130 | 6.49s | 49s | 3,944,107 |
| 12 | 243 | 240 | 3 | 11.84s | 100s | 16,262,245 |
| 13 | 98 | 98 | 0 | 7.91s | 29s | 3,269,982 |
| 14 | 298 | 168 | 130 | 8.56s | 98s | 11,132,742 |

Quality and risk signals:

| Signal | Count |
|---|---:|
| Completion tokens = 0 | 10 |
| Completion tokens 1-3 | 278 |
| Completion tokens 4-20 | 117 |
| Completion tokens > 20 | 535 |
| Prompt tokens >= 100k | 123 |
| Prompt tokens >= 200k | 4 |
| Duration >= 30s | 6 |
| Duration >= 60s | 2 |

Interpretation:

- The 900+ count is real in NewAPI, but ZenProxy current-process memory only
  held recent requests after restart. Use NewAPI `logs` for user-visible totals.
- Most traffic is `/v1/messages` Anthropic-compatible traffic from Claude Code.
- Large prompt loads are normal for this workload. P90 prompt size is above
  100k tokens, and max prompt size reached 262,161 tokens.
- Non-stream calls are not all errors; many are Claude/NewAPI converted
  requests. However, the longest outliers are non-stream calls with very low
  completion tokens, which match the user's previous "empty run / no useful
  reply" symptoms.
- `frt=-1000` in NewAPI logs means first-response timing was unavailable for
  those rows, usually non-stream or incomplete stream timing. It should not be
  treated as negative latency.
- NewAPI `perf_metrics` flushing showed an upstream NewAPI SQL warning:
  `column reference "generation_ms" is ambiguous`. This affects NewAPI's
  dashboard aggregation, not ZenProxy request serving. Do not fix it in this
  repo unless the user asks to modify NewAPI.

## ZenProxy Runtime Evidence

Current ZenProxy admin and runtime observations:

- `/admin/requests/summary` counted only current-process records after restart.
- `/admin/requests/export` showed recent request details with V4.1-A telemetry.
- `/admin/budget/nodes` showed Redis-backed global budget distribution.
- Redis `zprs:budget:*` showed 373 budgeted calls across current retained
  budget buckets at the time of analysis.
- `/tmp/zen-proxy-ledger-events.jsonl` contained 307 ledger events at the time
  of analysis.

Important distinction:

```text
NewAPI logs: user-visible call history and 900+ total.
ZenProxy ring buffer: current-process recent details.
Redis budget: retained per-node budget windows.
Ledger WAL: current WAL file events, not a complete historical billing source.
```

## Current Risks

1. Current-process ZenProxy telemetry is not enough for full-day analysis after
   restarts. NewAPI logs and Redis budgets must be used for historical views.
2. Non-stream calls with 1-3 completion tokens are common enough to monitor.
   Today there were 278 such calls, mostly from `deepseek-v4-flash` non-stream.
3. Very large prompts are normal for this chain. Any compaction or request-size
   change must preserve recent user messages and tool-call semantics.
4. First-token latency is mostly upstream/proxy path, not dispatch wait. Keep
   real node latency scoring enabled and watch whether slow nodes naturally
   lose score.
5. NewAPI dashboard perf aggregation has its own SQL warning. It is outside
   this repo's fix boundary.

## Next Maintenance Actions

- Add a ZenProxy admin endpoint or script that joins NewAPI request ids with
  ZenProxy request ids when available.
- Persist ZenProxy request telemetry beyond the current ring buffer if full-day
  Zen-only analysis is required.
- Add an operations script that exports:
  NewAPI logs, ZenProxy recent requests, Redis budget buckets, and WAL counts
  into one timestamped report.
- Add alert thresholds for:
  completion tokens <= 3, duration >= 30s, prompt tokens >= 200k, and
  `frt=-1000` on stream requests.
- Keep NewAPI read-only during ZenProxy maintenance unless the task explicitly
  targets NewAPI.
