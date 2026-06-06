# 2026-06-06 Stream Leasefix And Usage Analysis

## Purpose

Record the panda production fix for `status_code=503, no proxy resources
available`, the post-deploy acceptance evidence, and the 2026-06-06 NewAPI /
ZenProxy usage analysis.

This report is the current source of truth for proxy-pool health after the
stream client-gone lease leak fix.

## Root Cause

The `no proxy resources available` incident was not caused by all Webshare
nodes dying.

Before the fix, panda had:

```text
nodes file: 90 nodes
zen-proxy-rs@1/@2/@3: active
dead nodes: only 3 during the incident window
correct Redis prefix: zprs:*
external upstream sockets: not enough to justify 100+ local leases
```

The failure was local stale accounting:

```text
stream downstream closed or stopped consuming
-> metered stream task marked client_gone in telemetry
-> PoolManager was not reported
-> DispatchPool local active_leases stayed high
-> all nodes looked locally leased/budget-limited
-> acquire returned no proxy resources available
```

There was a second risk in the same path: `tx.send(...).await` could wait
forever if the downstream was gone or backpressured.

## Code Fix

Implemented source-side lease release semantics:

```text
ResultKind::ClientGone
StreamLeaseGuard drop fallback
30s downstream stream-send timeout
client_gone branch reports PoolManager
client_gone releases local and Redis leases without training AIMD latency
```

Changed files:

```text
src/pool/mod.rs
src/pool/active.rs
src/pool/dispatch.rs
src/pool/manager.rs
src/v4/provider.rs
```

New tests:

```text
client_gone_releases_lease_without_learning_completion_latency
stream_send_detects_closed_downstream
```

## Local Verification

Verification was run from the native WSL repository path:

```bash
cd /home/lenovo/zen-proxy-rs
cargo fmt --check
CARGO_INCREMENTAL=0 cargo check
CARGO_INCREMENTAL=0 cargo test
CARGO_INCREMENTAL=0 cargo clippy --all-targets -- -D warnings
CARGO_INCREMENTAL=0 cargo build --release
```

Result:

```text
fmt: pass
check: pass
tests: pass, 134 unit tests and 27 e2e tests
clippy -D warnings: pass
release build: pass
local release hash: c2ff6e04da064e4dc8579bbc11a92e318afe1bcd267771a21b8819cb16a96c96
```

## Panda Deployment

Deployed to:

```text
/opt/zen-proxy-rs/zen-proxy-rs
```

Runtime services:

```text
zen-proxy-rs@1: 127.0.0.1:4001
zen-proxy-rs@2: 127.0.0.1:4002
zen-proxy-rs@3: 127.0.0.1:4004
nginx: public 4000 load balancer
NewAPI: 127.0.0.1:8081
```

Deployed binary hash:

```text
c2ff6e04da064e4dc8579bbc11a92e318afe1bcd267771a21b8819cb16a96c96
```

Post-restart pool state:

```text
4001 dispatch=90 active=0 dead=0 budget_limited=0 leased=0
4002 dispatch=90 active=0 dead=0 budget_limited=0 leased=0
4004 dispatch=90 active=0 dead=0 budget_limited=0 leased=0
Redis keys use the zprs:* prefix.
```

## Acceptance Evidence

Direct ZenProxy:

```text
POST http://127.0.0.1:4000/v1/chat/completions
model=deepseek-v4-flash
result=200 DIRECT_VERIFY_OK
time_starttransfer=3.586958s
```

NewAPI through ZenProxy:

```text
GET  http://127.0.0.1:8081/v1/models -> 200
models include deepseek-v4-flash and deepseek-v4-flash-lite

POST http://127.0.0.1:8081/v1/chat/completions
model=deepseek-v4-flash
result=200 LEASEFIX_VERIFY_OK
time_starttransfer=2.114673s
```

Post-smoke lease check:

```text
4001 active=0 budget_limited=0 cooldown=0 dispatch=90 leased=0
4002 active=0 budget_limited=0 cooldown=0 dispatch=90 leased=0
4004 active=0 budget_limited=0 cooldown=0 dispatch=90 leased=0
```

NewAPI container logs in the recent post-deploy window did not show:

```text
no proxy resources available
zenproxy lane is saturated
Failed to parse JSON
upstream returned no assistant content or tool call
```

The immediate production fix is accepted.

## 2026-06-06 NewAPI Usage

Window:

```text
2026-06-06 00:00:00 CST -> 2026-06-07 00:00:00 CST
```

Important correction:

```text
NewAPI logs table total history: 132,494 rows
2026-06-06 rows through 23:01 CST: 12,530 rows
ZenProxy channel 69 rows: 12,248 rows
```

The user-visible "about 120k calls" was not today's NewAPI log row count. It
matches the order of total historical log rows, not 2026-06-06 new calls.

Today aggregate:

```text
calls=12,530
success logs(type=2)=10,804
error logs(type=5)=1,726
prompt_tokens=981,668,200
completion_tokens=14,742,999
total_tokens=996,411,199
quota=7,789,578
```

Channel 69 / ZenProxy:

```text
calls=12,248
errors=1,725
total_tokens=976,666,938
p50_use_time=7s
p90_use_time=62s
p99_use_time=157s
```

Model split:

```text
deepseek-v4-flash calls=12,248 errors=1,725 avg_prompt=78,544 avg_completion=1,197 p90=62s
gpt-5.5           calls=154    errors=0     avg_prompt=108,304 avg_completion=395   p90=28.4s
gpt-5.4           calls=105    errors=1     avg_prompt=28,413  avg_completion=193   p90=8s
claude-sonnet-4-6 calls=23     errors=0     avg_prompt=33      avg_completion=11    p90=3s
```

## Error Analysis

Channel 69 error classes:

```text
lane_saturated     918  15:07:18-15:38:25
upstream_502       462  00:39:34-21:17:44
no_proxy_resources 259  21:50:41-22:14:38
upstream_504        41  00:48:00-21:22:53
reasoning_content   20  09:04:58-12:04:59
other               17
stream_truncated     5
decode_body          3
```

Post-deploy window after 22:43 CST:

```text
channel 69 calls=91
errors=0
p90_use_time=15s
```

Main conclusion:

```text
no_proxy_resources is fixed by the stream lease release patch.
lane_saturated was an earlier 31-minute overload event.
remaining historical 502/504 mostly predate this deployment and need separate upstream/long-nonstream work.
```

## Stream Versus Non-Stream

Channel 69 split:

```text
non-stream calls=4,689 errors=1,423 avg_prompt=3,313 avg_completion=2,194 p50=32s p90=94s p99=251s
stream     calls=7,559 errors=302   avg_prompt=125,211 avg_completion=579   p50=6s  p90=25s p99=130s
```

Interpretation:

```text
Non-stream is the unhealthy path.
It has small prompts but high completion pressure, high timeout rate, and much worse tail latency.
Large-context stream is not the main error source today.
```

Prompt-token buckets for channel 69:

```text
<50k      calls=6,644 errors=1,725 p90=83s
50-100k   calls=1,582 errors=0     p90=64s
100-150k  calls=1,277 errors=0     p90=16s
150-200k  calls=1,256 errors=0     p90=14s
200-250k  calls=609   errors=0     p90=14s
250-300k  calls=333   errors=0     p90=13s
300-350k  calls=426   errors=0     p90=11s
350-400k  calls=116   errors=0     p90=13.5s
400-500k  calls=2     errors=0     p90=64.9s
500k+     calls=3     errors=0     p90=19.8s
```

All channel 69 errors were in the `<50k` bucket. This points at short
non-stream / long-output / upstream response handling, not input-context size.

## Cache And First-Response Time

NewAPI `other` fields for channel 69 successes:

```text
rows_with_other=10,531
cache_tokens=1,427,126,144
cache_creation_tokens=0
cache_hit_calls=8,159
cache_create_calls=0
p50_frt_ms=155
p90_frt_ms=6,803
p99_frt_ms=35,575
```

Interpretation:

```text
Cache is being recorded as hit/read by NewAPI.
cache_creation_tokens=0 is a provider/reporting semantic gap, not proof that cache is absent.
cache_tokens can exceed prompt tokens when repeated large-context cache reads are counted.
```

Stream FRT by prompt bucket:

```text
<50k      p50=3,011ms p90=11,831ms p99=122,080ms
50-100k   p50=3,445ms p90=10,023ms p99=46,506ms
100-150k  p50=159ms   p90=6,462ms  p99=20,877ms
150-200k  p50=250ms   p90=7,445ms  p99=21,985ms
200-250k  p50=322ms   p90=5,990ms  p99=15,443ms
250-300k  p50=399ms   p90=6,894ms  p99=21,412ms
300-350k  p50=450ms   p90=6,832ms  p99=16,253ms
350-400k  p50=6,367ms p90=9,605ms  p99=21,284ms
```

Large cached contexts often have subsecond p50 FRT. The remaining problem is
tail latency, not median long-context startup.

## Per-Node Budget And Proxy Layer

Effective panda runtime limits:

```text
node_max_calls_per_window=100
node_max_tokens_per_window=10,000,000
node_max_kb_per_window=65,536
node_budget_window_secs=3,600
node_lease_ttl_secs=180
node_max_concurrency=16
global budget mode=sync_redis
instances=3
nodes per instance=90
```

This means a single node can admit at most:

```text
100 calls/hour
10M prompt+completion tokens/hour
64MB request traffic/hour by local request KB accounting
up to 16 concurrent leases, subject to AIMD and lane limits
```

At 90 nodes and 3 instances, the theoretical pool envelope is not simply
`90 * 3` because global Redis budget coordinates node usage across instances.
The practical upper bound is the shared 90-node budget plus lane concurrency:

```text
short_nonstream max=32 per instance
normal_stream max=96 per instance
large_context max=16 per instance
huge_context max=3 per instance
long_nonstream max=8 per instance
long_output max=8 per instance
tool_heavy max=24 per instance
```

Live post-deploy node-window examples:

```text
panda-zen-1 hot node: 34 calls, 5,650,561 tokens, 22,090 KB, 1 in-flight
panda-zen-2 hot node: 29 calls, 4,880,090 tokens, 19,078 KB, 0 in-flight
panda-zen-3 hot node: 32 calls, 5,338,422 tokens, 20,870 KB, 1 in-flight
```

Those hot nodes were under the single-window 100 call / 10M token caps.

ZenProxy audit node distribution for 2026-06-06:

```text
nodes=90
requests p50=110 p90=183 p99=228 max=264
prompt_tokens p50=8,230,412 p90=15,288,407 p99=19,030,414 max=20,343,750
5xx p50=0 p90=1 p99=1 max=1
```

The audit distribution is a day aggregate, not a one-hour budget snapshot.

Top request node:

```text
node=4b3a1120 requests=264 success=264 5xx=0 prompt=10,144,150 completion=308,511 p90_total=91,591ms p90_ttft=46,227ms
```

Top prompt node:

```text
node=f9b457ae requests=188 success=188 5xx=0 prompt=20,343,750 completion=188,707 p90_total=37,632ms p90_ttft=35,668ms
```

Slowest TTFT bucket examples:

```text
node=199a3807 p90_ttft=135,767ms requests=73 5xx=0
node=1b501b91 p90_ttft=133,963ms requests=59 5xx=0
node=c3b23818 p90_ttft=122,177ms requests=91 5xx=0
```

These nodes did not necessarily fail, but they should be down-weighted for
latency-sensitive lanes.

## ZenProxy Audit Reconciliation Gap

NewAPI channel 69 counted 1,725 error logs today, while ZenProxy durable audit
node aggregation counted only 12 Zen-side 5xx records for the same date window.

This is expected but important:

```text
lane admission failures and some gateway-visible failures are recorded in NewAPI
but are not fully represented in per-node audit aggregation because no selected
node exists or the request failed before node accounting.
```

Optimization work must reconcile both sources:

```text
NewAPI logs: user-visible truth
ZenProxy audit: selected-node and upstream-path truth
Lane/admin counters: pre-node admission truth
```

## Optimization Targets

P0. Keep the lease fix monitored:

```text
alert if local leased stays > 0 while there are no upstream sockets and no matching zprs:lease:* key
alert if no_proxy_resources appears after client_gone spikes
```

P1. Add first-class lane rejection telemetry:

```text
record lane name, wait time, in-flight count, max, request profile, and source client
include lane rejections in durable audit even when no node is selected
```

P2. Fix non-stream health:

```text
non-stream currently has about 30% error rate on channel 69
force long-output non-stream into long_nonstream/long_output lanes
cap or reject pathological non-stream outputs before 300s timeout
prefer stream for ClaudeCode/Hermes/OpenClaw-style agent traffic
```

P3. Split node scoring by lane and token bucket:

```text
do not let a node with 120s+ p90 TTFT serve latency-sensitive short requests
down-weight slow TTFT nodes only for affected buckets
keep high-throughput but slow nodes available for non-latency-sensitive work
```

P4. Improve cache reporting semantics:

```text
separate attempted / accepted / rejected / ignored cache events
record provider usage/header/body cache signals separately
document why cache_tokens can exceed prompt_tokens
```

P5. Continue upstream error normalization:

```text
reasoning_content errors need a dedicated DeepSeek thinking-history policy
502/504 need upstream body class, retry chain, and selected-node correlation
stream decode/truncated errors need retry/downgrade evidence instead of generic 500
```

P6. Build a daily operator report:

```text
calls by user/token/channel/model
stream/non-stream split
error classes
FRT and total latency percentiles
cache read/create semantics
node budget and node latency outliers
optimization suggestions written for low-capability analysis models
```

## Current Status

As of the post-deploy verification:

```text
no_proxy_resources: fixed in the observed window
current pool: healthy
live zprs lease keys can be nonzero while a real stream is in flight
NewAPI -> ZenProxy -> free-model-client-rs -> upstream: smoke passed
single-node budget: visible and under live caps
remaining highest-impact issue: non-stream small-prompt/long-output failure path
```
