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

For streaming requests:

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

Use a temporary `CARGO_TARGET_DIR` outside the repository when possible. This
repository currently has unrelated tracked `target-1.86` deletion noise that
must not be mixed into the V4.107 patch.

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
stream usage should stop losing input/cache counts on partial usage frames
DeepSeek cache-hit fields should be visible to downstream usage accounting
```

If NewAPI/ccswitch cache hit remains around 60%-65% after this patch, the next
root-cause search should focus on real session resets, provider/account cache
behavior, prefix instability, client_gone rows, and short non-stream probes.
Do not solve that by cutting context or fabricating cache usage.
