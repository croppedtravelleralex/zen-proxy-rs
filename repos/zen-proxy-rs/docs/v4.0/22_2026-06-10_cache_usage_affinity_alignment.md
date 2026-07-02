# 2026-06-10 Cache Usage And Affinity Alignment

## Purpose

Record the V4.107 source-side patch for cache usage display alignment and
medium-context cache affinity.

This is not a prompt, context, output, thinking, NewAPI, ClaudeCode, or
ccswitch change. It only changes how ZenProxy merges upstream usage telemetry
and how it builds the non-secret affinity key used for proxy-node selection.

## Trigger

After V4.106, production data showed two different cache views:

```text
provider-side cache observation: high for many long requests
NewAPI / ccswitch display: lower, often around 60%-65% in mixed windows
```

The views are not identical by definition, but two source-side issues could
make the display path worse than it needed to be:

1. Some streaming usage frames only include output/cache fields. If a later
   frame omits input or cache fields, ZenProxy must not overwrite previously
   observed values with zero.
2. Medium contexts can grow across body-size buckets even while the stable
   conversation prefix remains the same. Affinity should follow that stable
   prefix, not the transient body-size bucket.

## Code Changes

Changed file:

```text
src/v4/provider.rs
```

Usage handling:

```text
StreamMetrics now updates only fields that are present in a usage frame.
DeepSeek prompt_cache_hit_tokens is recognized as cache read.
OpenAI and Anthropic extraction both map real provider cache-hit fields into
cached_tokens / cache_read_input_tokens.
```

Affinity handling:

```text
AFFINITY_MIN_BODY_BYTES = 32KB
AFFINITY_MEDIUM_PREFIX_BYTES = 32KB
AFFINITY_LARGE_PREFIX_BYTES = 256KB
```

For 32KB+ requests, including non-streaming ClaudeCode tool/JSON paths:

```text
body < 32KB: no affinity key
body >= 32KB: affinity key enabled
messages material <= 256KB: hash first 32KB
messages material > 256KB: hash first 256KB
```

The key no longer includes `body_size_bucket`, so a stable-prefix conversation
does not lose affinity only because the request body crossed a bucket boundary.

The key still includes:

```text
model
path
client bucket
messages prefix hash
tools hash
tool_choice hash
```

## FreeModel Kernel Companion Change

The path dependency `../free-model-client-rs` was updated in the same release
candidate:

```text
Anthropic final message_delta.usage now includes input_tokens when the upstream
response has prompt_tokens/input_tokens available.
```

This helps downstream usage mergers preserve real input token counts in the
final stream usage frame. It does not invent cache tokens.

## Tests

Added or updated unit/golden coverage:

```text
extracts_deepseek_prompt_cache_hit_usage_counts
stream_metrics_merges_anthropic_usage_without_zeroing_prompt_or_cache
stream_metrics_accepts_deepseek_prompt_cache_hit_tokens
affinity_key_is_for_medium_and_large_streaming_requests
affinity_key_keeps_medium_stable_prefix_when_tail_grows
affinity_key_does_not_change_when_context_crosses_body_bucket
anthropic_stream_preserves_cache_usage_metadata
```

Required local verification before deployment:

```bash
cd /home/lenovo/free-model-client-rs
cargo fmt -- --check
cargo test
cargo clippy --all-targets -- -D warnings

cd /home/lenovo/zen-proxy-rs
cargo fmt -- --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

Use a temporary `CARGO_TARGET_DIR` outside the repository when possible.
2026-07-02 cleanup removed the previously tracked `target-1.86` build artifacts
from the index; keep future build output ignored.

## Deployment Boundary

V4.107 must be deployed as the `zen-proxy-rs` release binary, because the binary
links the local `free-model-client-rs` path dependency.

Do not deploy by changing:

```text
NewAPI
ClaudeCode
ccswitch
client prompts
client model settings
```

## Panda Deployment

Deployment time:

```text
2026-06-10 11:28 CST
```

Deployed stripped binary:

```text
e3001320300b37e8daf05266e7c1899652df8f42729a8f029db4c8602d4cd3c5
```

Previous binary backup:

```text
/opt/zen-proxy-rs/backups/zen-proxy-rs.20260610-112855.pre-v4107-b401c9463e29
```

Rolling restart result:

```text
zen-proxy-rs@1 active, port 4001, health ok, pid 1255624
zen-proxy-rs@2 active, port 4002, health ok, pid 1255757
zen-proxy-rs@3 active, port 4004, health ok, pid 1255952
nginx/front port 4000 health ok
```

Pool snapshot after deploy:

```text
dead=0
ratelimited=0
dispatch around 88-90 depending on in-flight user traffic
```

The deployment used a single shared binary replacement followed by per-instance
systemd restarts. It did not modify NewAPI, ClaudeCode, ccswitch, node files, or
environment settings.

## Acceptance

Minimum smoke after deployment:

```text
4001 /health -> ok
4002 /health -> ok
4004 /health -> ok
4000 /health -> ok
4000 /v1/models -> deepseek-v4-flash and deepseek-v4-flash-lite
direct OpenAI minimal request -> 200
direct Anthropic minimal request -> 200
panda NewAPI channel 69 OpenAI minimal request -> 200
panda NewAPI channel 69 Anthropic minimal request -> 200
```

Observed smoke:

```text
4001 /health -> ok
4002 /health -> ok
4004 /health -> ok
4000 /health -> ok
4000 /v1/models -> deepseek-v4-flash, deepseek-v4-flash-lite
direct OpenAI minimal request -> 200 PONG_V4107_DIRECT_OPENAI, about 2.9s
direct Anthropic minimal request -> 200 PONG_V4107_DIRECT_ANTHROPIC, about 2.1s
panda NewAPI channel 69 OpenAI minimal request -> 200 PONG_V4107_OPENAI, about 2.2s
panda NewAPI channel 69 Anthropic minimal request -> 200 PONG_V4107_ANTHROPIC, about 1.8s
```

Production observation:

```text
Use at least a 15-30 minute post-deploy window.
Ignore the first few minutes of affinity/cache warm-up as final evidence.
Split by input bucket: <10k, 10k-50k, 50k-100k, 100k-200k, 200k+.
Compare average-request cache hit and token-weighted cache hit.
Keep provider cache observation and NewAPI/ccswitch display mouthpieces separate.
```

Failure criteria:

```text
Invalid tool parameters increases
Failed to parse JSON increases
lane is saturated returns
no proxy resources available returns
provider_missing_reasoning_content returns
first-content latency regresses materially
ClaudeCode output quality or tool behavior regresses
```

Early post-deploy NewAPI channel 69 window:

```text
window: about 8 minutes after deploy
records: 1367
final errors: 0
stream records: 20
non-stream records: 1347
200k+ records: 18
overall weighted cache hit: about 48.30%
200k+ weighted cache hit: about 51.33%
```

This early cache window is warm-up and mixed with many small non-stream rows. It
must not be treated as final proof that the cache target has or has not been
met.

## Expected Effect

Expected improvements are narrow and measurable:

```text
medium-context affinity should be more stable
cross-body-bucket conversations should keep the same affinity key
large non-streaming requests can reuse recently successful cache-warm nodes
stream usage should stop losing input/cache counts on partial usage frames
DeepSeek cache-hit fields should be visible to downstream usage accounting
```

If NewAPI/ccswitch cache hit remains around 60%-65% after this patch, the next
root-cause search should focus on real session resets, provider/account cache
behavior, prefix instability, client_gone rows, and short non-stream probes.
Do not solve that by cutting context or fabricating cache usage.

## 2026-06-22 Follow-up: Header And Source-Client Stability

Current production work is allowed by the user, but still must use the bounded
deployment flow and must not leak keys, tokens, or proxy credentials.

Current public channel-69 boundary:

```text
public: deepseek-v4-flash, deepseek-v4-flash-lite, mimo-v2.5
hidden route only: north-mini-code, nemotron-3-ultra, minimax-m3, qwen3.6-plus
dev/new test domain: https://new.relai.asia/
production domain: https://sub2api.closeapi.top/
```

Read-only production baseline at 2026-06-22 08:23 CST:

```text
deepseek-v4-flash-free ClaudeCode, last 120m:
  cache observation accepted=38 rejected=14
  accepted token hit about 96.94%

deepseek-v4-flash-free ClaudeCode, last 24h audit success rows:
  n=13097
  token-weighted cache hit about 84.26%

mimo-v2.5-free ClaudeCode, last 24h audit success rows:
  n=491
  token-weighted cache hit about 18.04%
  50k-100k bucket hit about 9.29%, affinity 175/183
  100k-200k bucket hit about 5.32%, affinity 105/105
```

The important shape is unchanged from the earlier cache investigations:
`prefix_4k_hash` and `prefix_32k_hash` can remain stable while
`prefix_128k_hash`, `prefix_256k_hash`, and `cache_material_bytes` move as the
tail grows. That means the safe next steps are request/header/session stability,
not prompt trimming or fake usage.

Follow-up source changes:

```text
free-model-client-rs/src/zen/client.rs:
  x-opencode-request is now deterministic from the complete normalized upstream
  body instead of a random value.

zen-proxy-rs/src/v4/provider.rs:
  affinity_key includes source_client, so ClaudeCode, Hermes, OpenClaw, and
  unknown traffic do not share the same affinity key under the same API key and
  stable prefix.
```

Quality boundary:

```text
no request body changes
no context trimming
no tool schema changes
no WebFetch/WebSearch/Bash disablement
no output cap
no default disabled thinking expansion
no fabricated cache usage
```

Required acceptance after deployment:

```text
4001/4002/4004/4000 health ok
/v1/models exposes only deepseek-v4-flash, deepseek-v4-flash-lite, mimo-v2.5
ClaudeCode smoke covers deepseek-v4-flash and mimo-v2.5
smoke dimensions include Bash, WebFetch, WebSearch, text, json, stream-json
post-deploy 15-30 minute window recomputes accepted/rejected, prefix stability,
token-weighted cache hit, TTFT/first_content, and tool quality regressions
```

Deployment result:

```text
2026-06-22 10:12 CST:
  deployed V4.111 to panda production ZenProxy 4001/4002/4004
  binary sha256 ee6393093d61b9fedd77112db67f093e469cdeceb5b7f9cfdd9c885d7fc2dc38
  xz sha256     c8ffdf66797f66023096c6682b965ab054f382e04a254e93797a7027ee863efb
  backup        /opt/zen-proxy-rs/backups/zen-proxy-rs.20260622-101213.pre-v4111-cache-header-4bb606dc25c7
  GitHub release/tag v4111-cache-header-20260622-1002 deleted after deploy
```

Post-deploy acceptance:

```text
4001/4002/4004/4000 health ok after rolling restart, dispatch=100, dead=0, ratelimited=0
11:24 CST follow-up: 4004 had dead=1/dispatch=99 while 4001/4002 and 4000
aggregate stayed status=ok; keep observing node health separately from cache.
4000 /v1/models exposes only:
  deepseek-v4-flash
  deepseek-v4-flash-lite
  mimo-v2.5

Direct ZenProxy OpenAI-compatible smoke:
  deepseek-v4-flash HTTP 200, content OK
  mimo-v2.5         HTTP 200, content OK, cache_read_input_tokens=192

Windows official claude.orig.exe bridge smoke:
  deepseek-v4-flash Bash/WebFetch/WebSearch x text/json/stream-json: 9/9 pass
  mimo-v2.5 runner report: 8 pass + 1 slow_pass
  caveat: panda ingress showed those mimo-labeled ClaudeCode requests still used
          model=deepseek-v4-flash, so this is not real mimo ClaudeCode evidence.

WSL /home/lenovo/.local/bin/claude:
  currently a clawgod launcher; first two deepseek Bash cases failed and the
  matrix was stopped. Do not use it as official ClaudeCode evidence.
```

Post-deploy cache window:

```text
2026-06-22 10:12-10:58 CST, deepseek-v4-flash-free + ClaudeCode:
  cache rows 153
  accepted 102
  rejected 51
  token read/miss about 2,565,376 / 1,816,886
  read_pct about 58.54%
  50k-100k bucket accepted 31 / rejected 4, read_pct about 81.16%
  top repeated prefix_4k/prefix_32k groups appeared 63 and 30 times
  prefix_128k/prefix_256k still moved with tail growth
```

Remaining blocker:

```text
Need a confirmed ClaudeCode path that actually sends model=mimo-v2.5, then rerun
Bash/WebFetch/WebSearch x text/json/stream-json and collect a 30m+ real cache
window. Do not count Windows bridge mimo-labeled pass results until ingress logs
show model=mimo-v2.5.
```

## 2026-06-22 Follow-up: Non-Streaming Affinity

Trigger:

```text
The existing 32KB+ affinity key was still stream-only. Production logs showed
repeated ClaudeCode non-streaming requests around 50KB-66KB with stable
prefix_32k_hash values. Those requests can benefit from the same soft
cache-warm node reuse without changing prompt semantics.
```

Production baseline before deploy:

```text
2026-06-22 14:17 CST, last 120m:
  deepseek-v4-flash-free + ClaudeCode stream rows:
    n=140, accepted/rejected=110/30, token read_pct about 72.29%
  deepseek-v4-flash-free + ClaudeCode non-stream rows:
    n=131, accepted/rejected=124/7, token read_pct about 92.87%
  deepseek-v4-flash + claude-code ingress:
    n=272, non-stream=130, stream=142, 32KB+=240
```

Code change:

```text
src/v4/provider.rs:
  build_affinity_key no longer returns empty for stream=false.
  32KB+ streaming and non-streaming requests share the same stable-prefix
  affinity logic.

Key dimensions remain:
  model
  path
  source_client
  client bucket
  messages prefix hash
  tools hash
  tool_choice hash
```

Quality boundary:

```text
no request body changes
no prompt, messages, tools, tool_choice changes
no context trimming
no output cap
no default disabled thinking expansion
no fabricated cache usage
```

Local verification:

```text
cargo test affinity_key_ -- --nocapture: 5 passed
cargo fmt -- --check: passed
cargo clippy --all-targets -- -D warnings: passed
cargo test: unit 194 passed, e2e 44 passed
```

Deployment:

```text
2026-06-22 14:32 CST:
  deployed V4.112 to panda production ZenProxy 4001/4002/4004
  binary sha256 766eef7f3e51b7eb8e3af57bf058db35da538e1b3fa14074dd3a4f5f789dcbca
  xz sha256     739a54ba07783dd0bbb2b697f7e08a13b0568d5cc6fbfdaa6c2f7eeb64a30b88
  backup        /opt/zen-proxy-rs/backups/zen-proxy-rs.20260622-143230.pre-v4112-cache-affinity-ee6393093d61
```

Post-deploy checks:

```text
4001/4002/4004/4000 health ok after rolling restart:
  dispatch=100
  dead=0
  ratelimited=0

4000 /v1/models exposes only:
  deepseek-v4-flash
  deepseek-v4-flash-lite
  mimo-v2.5

Temporary GitHub releases/tags deleted:
  croppedtravelleralex/new v4112-cache-affinity-20260622-1428 -> release not found
  croppedtravelleralex/zen-proxy-rs v4112-cache-affinity-20260622-1424 -> release not found
  croppedtravelleralex/new contents -> []
```

Post-deploy early cache window:

```text
2026-06-22 14:32-14:51 CST, deepseek-v4-flash-free + ClaudeCode:
  stream cache rows 205
  stream accepted/rejected 173/32
  stream token read/miss about 10,105,472 / 2,587,885
  stream read_pct about 79.61%

  non-stream cache rows 105
  non-stream accepted/rejected 101/4
  non-stream token read/miss about 1,200,896 / 142,260
  non-stream read_pct about 89.41%

  combined stream+non-stream token read_pct about 80.55%

Compared with the 120m pre-deploy baseline, the comparable combined read_pct
rose from about 77.36% to about 80.55%, and stream rose from 72.29% to 79.61%.
Non-stream stayed high but did not improve in the early window, moving from
92.87% to 89.41%. Treat this as an early production signal, not a final
90%+/95%+ acceptance.
```

Remaining verification:

```text
The current shell environment did not have ANTHROPIC_API_KEY, so the full
ClaudeCode acceptance runner was not executed in this deployment window.
Direct 4000 smoke with a fake key returned 403 and is classified as auth, not a
model failure. Use a valid non-printed API key or existing cc-switch bridge for
the next Bash/WebFetch/WebSearch x text/json/stream-json matrix.

Collect a longer post-deploy production window and compare:
  stream/non-stream token read_pct
  prefix_32k repeated groups
  affinity_hit and affinity_node_id
  first_content_ms / first_tool_call_ms
  non-stream guard retry rate
  tool quality errors
```

Longer post-deploy observation:

```text
2026-06-22 14:32-15:04 CST, deepseek-v4-flash-free + ClaudeCode:
  provider cache rows about 528
  accepted/rejected about 455/73
  token read/miss about 27,573,504 / 6,397,125
  combined token read_pct about 81.17%

Split by prompt_hash matched to stream completion summaries:
  stream rows about 375
  stream accepted/rejected 312/63
  stream read_pct about 80.77%

  non-stream/unpaired rows about 153
  non-stream accepted/rejected 143/10
  non-stream read_pct about 87.07%

ClaudeCode stream first real text/tool:
  accepted P50 about 2856ms, P95 about 8257ms
  rejected P50 about 5124ms, P95 about 11154ms
```

The stream side kept improving versus the pre-deploy 72.29% baseline. The
non-stream side stayed high but did not beat the pre-deploy 92.87% window. This
means V4.112 is useful, but not enough to claim global 90%+ cache hit across
mixed ClaudeCode traffic.

No regression was observed in this window for:

```text
no proxy resources
lane is saturated
panic
Invalid tool parameters
Failed to parse JSON
stream truncated before DONE
```

Remaining speed/cache blockers:

```text
provider_missing_reasoning_content first-attempt failures
reasoning_only_length retries, often with max_tokens=64
missing real affinity_hit/audit stats because admin audit was not available
no confirmed ClaudeCode path that truly sends model=mimo-v2.5
```

Do not expand first-attempt disabled-thinking from this evidence alone. It
would reduce some retry cost, but it changes upstream reasoning behavior and is
outside the quality-preserving cache/header/session boundary unless a narrower
request class is proven safe.
