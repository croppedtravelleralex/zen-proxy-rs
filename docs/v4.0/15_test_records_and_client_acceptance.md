# V4.7 Test Records and Client Acceptance

## Goal

V4.7 adds a repeatable test evidence package for OpenClaw, Hermes, Claude Code,
CherryStudio, and direct SDK tests that enter through NewAPI and then reach
ZenProxy.

The target evidence chain is:

```text
client
-> NewAPI
-> ZenProxy
-> free-model-client-rs kernel
-> selected proxy node
-> upstream
-> ZenProxy audit/admin/metrics
```

The purpose is not to create a second observability backend. It is to make one
test run explainable after the fact without copying secrets, prompts, tool
outputs, or proxy credentials into the repository.

## Run Directory

Each run writes a directory under:

```text
test-records/runs/<run_id>/
```

Required files:

```text
manifest.json
summary.md
raw/
  zen-admin-runtime.redacted.json
  zen-admin-config.redacted.json
  zen-admin-pools.redacted.json
  zen-admin-budget.redacted.json
  zen-admin-budget-nodes.redacted.json
  zen-audit-summary.redacted.json
  zen-audit-anomalies.redacted.json
  zen-audit-export.redacted.jsonl
  zen-metrics.prom
derived/
  metrics.jsonl
  request-map.jsonl
  tool-repair-summary.json
```

Schemas live in:

```text
test-records/schemas/
```

Generated run output under `test-records/runs/` is intentionally ignored and
must not be committed. The legacy local build directory `target-1.86/` is also
ignored for new files, but any files already tracked there require a separate
index cleanup outside this documentation update.

## Collection Script

Use:

```bash
python scripts/collect_test_record.py \
  --scenario newapi-smoke \
  --zen-base-url http://127.0.0.1:4000 \
  --zen-admin-base-url http://127.0.0.1:4001 \
  --zen-admin-base-url http://127.0.0.1:4002 \
  --zen-admin-base-url http://127.0.0.1:4004 \
  --newapi-base-url http://127.0.0.1:8081 \
  --admin-key test-key
```

The script is read-only against ZenProxy. It collects admin and audit snapshots
for a time window, redacts obvious secrets, normalizes audit rows into
`derived/metrics.jsonl`, and writes a human-readable `summary.md`.

## NewAPI Log Import

Local NewAPI logs are available from the PostgreSQL container:

```text
container: new-api-postgres
database: new-api
user: newapi
table: logs
```

Use the run's `manifest.json` time window and convert milliseconds to seconds:

```bash
RUN_DIR=test-records/runs/20260527-095425-chain-smoke
FROM_S=1779846860
TO_S=1779846895

docker exec new-api-postgres psql -U newapi -d new-api -At <<SQL > "$RUN_DIR/raw/newapi-logs-db.redacted.jsonl"
select json_build_object(
  'request_id', request_id,
  'upstream_request_id', upstream_request_id,
  'channel_id', channel_id,
  'model_name', model_name,
  'status', case when type = 2 then 200 else null end,
  'type', type,
  'duration_ms', use_time * 1000,
  'created_at', created_at,
  'is_stream', is_stream,
  'prompt_tokens', prompt_tokens,
  'completion_tokens', completion_tokens
)
from logs
where created_at >= ${FROM_S} and created_at <= ${TO_S}
order by created_at, id;
SQL
```

Then rebuild derived evidence for the existing run:

```bash
python scripts/collect_test_record.py \
  --raw-run-path "$RUN_DIR" \
  --newapi-log-path "$RUN_DIR/raw/newapi-logs-db.redacted.jsonl" \
  --join-time-window-ms 5000
```

For a cloud NewAPI host, run the same SQL over SSH and copy only the redacted
JSONL fields above into the run directory. Do not export token names, usernames,
IP addresses, request bodies, headers, or raw `content` unless a separate
redaction review is done.

Accepted NewAPI import formats:

```json
{"request_id":"20260527015425798241258268d9d6mzoG6g0c","channel_id":19,"model_name":"deepseek-v4-flash","status":200,"duration_ms":3000,"created_at":1779846868,"is_stream":false,"prompt_tokens":27,"completion_tokens":7}
```

`created_at` may be epoch seconds, epoch milliseconds, or an ISO timestamp.
`duration_ms` is preferred. If only NewAPI `use_time` is present, the collector
treats it as seconds and converts it to milliseconds. When NewAPI request ids
are not propagated into ZenProxy `external_request_id`, `request-map.jsonl`
uses model/status plus nearest timestamp matching and records `join_delta_ms`.

For a minimal end-to-end smoke that first calls NewAPI and then collects the
same time window:

```bash
NEWAPI_API_KEY="[runtime env]" python scripts/run_chain_smoke.py \
  --newapi-base-url http://127.0.0.1:8081 \
  --zen-base-url http://127.0.0.1:4000 \
  --zen-admin-base-url http://127.0.0.1:4001 \
  --zen-admin-base-url http://127.0.0.1:4002 \
  --zen-admin-base-url http://127.0.0.1:4004
```

For a generated streaming TTFT probe:

```bash
NEWAPI_API_KEY="[runtime env]" python scripts/run_ttft_experiment.py \
  --tokens 1000 \
  --base-url http://127.0.0.1:8081
```

The TTFT runner is a guarded experiment entrypoint, not a default long-context
load generator. It writes `manifest.json`, `summary.md`, and
`ttft-metrics.jsonl` under `test-records/runs/<run_id>/`. Each metrics row
includes `run_id`, `case_id`, `attempt`, `token_bucket`, `cold_warm`, `model`,
`status`, `body_bytes`, `first_byte`, `first_content`, `total`, and `error`.
Timing values are milliseconds.

The default budget remains safe: one request and 1000 approximate input tokens.
Any repeat or long-context run must explicitly raise both request and token
budgets:

```bash
NEWAPI_API_KEY="[runtime env]" python scripts/run_ttft_experiment.py \
  --tokens 100000 200000 \
  --repeat 2 \
  --case-prefix p99-prefixfirst \
  --max-total-tokens 600000 \
  --max-requests 4 \
  --base-url http://127.0.0.1:8081
```

## OpenClaw / Hermes Client Commands

Use the WSL-local acceptance runner when OpenClaw and Hermes are already
installed on the same WSL host:

```bash
python scripts/run_openclaw_hermes_acceptance.py
```

Default mode is dry-run. It discovers local entrypoints, reports likely config
locations, and prints the commands that would be used without calling the
clients.

To execute the minimal P0 probes:

```bash
NEWAPI_API_KEY="$NEWAPI_API_KEY" python scripts/run_openclaw_hermes_acceptance.py \
  --execute \
  --base-url http://127.0.0.1:8081 \
  --model deepseek-v4-flash
```

The runner writes:

```text
test-records/runs/<run_id>/client-acceptance.md
```

Recorded evidence is limited to command shape, status/exit code, timing, byte
counts, hashes, and redacted diagnostics. API keys, Bearer tokens, prompts,
full completions, and tool outputs are not stored.

Local WSL discovery on 2026-05-27 found:

```text
Hermes executable: /home/lenovo/.local/bin/hermes
Hermes config: /home/lenovo/.hermes/config.yaml
OpenClaw executable: /home/lenovo/.local/node_modules/.bin/openclaw
OpenClaw config candidate: /home/lenovo/.openclaw-zenproxy-v46/openclaw.json
```

OpenClaw currently reports that Node.js v22.19+ is required while the default
runtime is v20.20.2. The runner therefore treats OpenClaw short-chat execution
as gated until `openclaw --help` can confirm a safe one-shot command shape.

### Subtask O Execute Probe 2026-05-27

Subtask O reran the WSL-local acceptance runner in execute mode without storing
keys:

```bash
python3 scripts/run_openclaw_hermes_acceptance.py \
  --execute \
  --run-id 20260527-openclaw-hermes-o-real \
  --base-url http://127.0.0.1:8081 \
  --model deepseek-v4-flash
```

Evidence:

```text
test-records/runs/20260527-openclaw-hermes-o-real/client-acceptance.md
```

Observed result:

```text
HTTP NewAPI probes: skipped because NEWAPI_API_KEY was not present in the runner environment.
Hermes help: exit=0, stdout_bytes=9108.
Hermes short-chat: blocked before model call by non-writable /home/lenovo/.hermes/logs/agent.log owned by root.
OpenClaw help: blocked by Node.js v22.19+ requirement; current node is /usr/bin/node v20.20.2.
Node 22 discovery: corepack exists; no nvm/fnm/volta/asdf executable or local node22 candidate was found by the runner.
```

No chmod, deletion, Node install, or system runtime change was attempted.
The runner now records Node runtime discovery and redacts prompt arguments in
recorded command shapes; short-chat command results keep hashes and byte counts
without storing model-output previews.

## Chain-of-Custody-144

`Chain-of-Custody-144` is the 99+ real-path acceptance matrix for OpenClaw and
Hermes. It must prove where each request entered, which gateway and channel it
used, which ZenProxy request id handled it, which proxy node was selected, and
whether protocol guard repaired or downgraded malformed tool history.

P0 cases:

- NewAPI `/v1/models` sees the two public models.
- OpenAI chat non-stream and stream succeed through NewAPI.
- Anthropic messages non-stream and stream succeed through NewAPI when the
  client supports that path.
- OpenAI tool history with missing `tool_call_id` is repaired or downgraded.
- Anthropic `tool_use/tool_result` history is repaired before upstream.
- Mixed `text + tool_result` histories do not leave orphan tool results.
- Large tool histories that trigger compaction still end with
  `protocol_guard.post_valid=true`.
- Empty-output, 429, 5xx, timeout, and transport errors are not marked as clean
  successes.
- Each test run produces `summary.md`, `metrics.jsonl`, and `request-map.jsonl`.

P1 cases:

- OpenClaw profile and Hermes profile both run through the same NewAPI base URL
  and key without source changes.
- Subagent or delegated tasks produce a traceable request sequence.
- 20+ round sessions do not corrupt role order or tool pair order.
- Proxy pool and global budget snapshots explain node selection and rate-limit
  behavior.

P2 cases:

- 8K, 32K, 64K, 128K context levels collect first chunk, first content, first
  tool-call, and stream complete timings.
- Request costs and token estimates can be reconciled with NewAPI logs.
- Slow TTFT and low-completion anomalies are visible in the summary.

Acceptance:

```text
P0 pass rate: 100%
P1 pass rate: >= 95%
P2 pass rate: >= 85%
ZenProxy audit to NewAPI log join rate: >= 95% when NewAPI request ids are available
No raw prompt, completion, tool output, API key, Bearer token, or proxy credential is stored
```

## P99 PrefixFirst-200K

`P99 PrefixFirst-200K` is the long-context TTFT experiment plan. It separates
true model TTFT from proxy overhead and client-visible feedback.

Required timing fields:

```text
client_send_ts
proxy_receive_ts
upstream_request_start
upstream_first_byte
first_sse_event
first_content_delta
first_visible_char
done_ts
prompt_tokens
cached_tokens
completion_tokens
```

Experiment groups:

```text
100k cold
100k warm
200k cold
200k warm
```

Success criteria:

```text
proxy_overhead p95 < 150ms
warm cached_tokens / prompt_tokens >= 70%
warm TTFT improves >= 30% over cold baseline
context-budget path reduces tokens >= 50% with quality loss <= 2%
```

Cold 100K-200K prompt prefill is upstream model work. ZenProxy can reduce its
own overhead, preserve cache-friendly prompt shape, and route by affinity, but
it cannot turn a cold upstream prefill into a sub-second model TTFT by itself.

## Privacy Rules

- API keys, Bearer tokens, cookies, and proxy credentials are redacted.
- Raw request bodies, messages, prompt text, completion text, and tool output
  are not stored.
- Failure messages are reduced to class/hash in normalized metrics.
- Node URLs must be redacted before writing.
- `observed_exit_ip` may be retained only for short test windows when needed to
  prove egress path.

## TTFT Small-Matrix Retest 2026-05-27

Subtask P reran only the safe TTFT matrix:

```bash
NEWAPI_API_KEY="[runtime env]" python3 scripts/run_ttft_experiment.py \
  --run-id 20260527-ttft-p-dryrun \
  --dry-run \
  --tokens 1000 \
  --repeat 1 \
  --case-prefix ttft-p \
  --max-total-tokens 1000 \
  --max-requests 1 \
  --base-url http://127.0.0.1:8081 \
  --model deepseek-v4-flash

NEWAPI_API_KEY="[runtime env]" python3 scripts/run_ttft_experiment.py \
  --run-id 20260527-ttft-p-real-1k \
  --tokens 1000 \
  --repeat 1 \
  --case-prefix ttft-p \
  --max-total-tokens 1000 \
  --max-requests 1 \
  --timeout 180 \
  --base-url http://127.0.0.1:8081 \
  --model deepseek-v4-flash
```

Evidence:

```text
test-records/runs/20260527-ttft-p-dryrun/
test-records/runs/20260527-ttft-p-real-1k/
```

Observed result:

```text
dry-run: status=dry-run, body_bytes=5940, planned_approx_tokens=1000
real 1k: status=200, first_byte=3612ms, first_content=3612ms, total=5382ms, bytes_received=9109
```

The real 1k request succeeded through `http://127.0.0.1:8081`, so no NewAPI,
ZenProxy, or model-path timeout diagnosis was required for this retest. No
100K/200K run was attempted.

## OpenClaw Provider / Model Alias Retest 2026-05-27

Subtask X updated the dedicated OpenClaw config at
`/home/lenovo/.openclaw-zenproxy-v46/openclaw.json` without modifying
OpenClaw source:

```text
provider: zenproxy
baseUrl: http://127.0.0.1:8081/v1
api: openai-completions
apiKey: SecretRef env NEWAPI_API_KEY
default model: zenproxy/deepseek-v4-flash
alias: deepseek-v4-flash -> zenproxy/deepseek-v4-flash
```

Validation command:

```bash
NEWAPI_API_KEY="[runtime env]" \
python3 scripts/run_openclaw_hermes_acceptance.py \
  --execute \
  --run-id 20260527-openclaw-x-alias-p0-v3 \
  --timeout 60
```

Evidence:

```text
test-records/runs/20260527-openclaw-x-alias-p0-v3/
```

Observed result:

```text
Node runtime: v22.19.0 at /home/lenovo/.local/node_modules/.bin/node
OpenClaw help: exit=0
OpenClaw models status: exit=0, aliases.deepseek-v4-flash=zenproxy/deepseek-v4-flash
HTTP /v1/models: status=200
HTTP short-chat: status=502
OpenClaw short-chat with --model deepseek-v4-flash: exit=1, stderr_bytes=731
```

Conclusion: OpenClaw now recognizes the bare model alias and routes it to the
`zenproxy` provider. The remaining P0 short-chat failure is downstream of model
resolution: the local `/v1` route is reachable, but upstream inference returned
502 during the retest. No API key, full prompt, or full model response was
stored.

## Hermes NewAPI Provider Retest 2026-05-27

Subtask Z1 updated the user-level Hermes config at
`/home/lenovo/.hermes/config.yaml` after first creating a same-directory
timestamped backup:

```text
backup: /home/lenovo/.hermes/config.yaml.bak-20260527-152225
provider: newapi
base_url: http://127.0.0.1:8081/v1
api_mode: chat_completions
key_env: NEWAPI_API_KEY
default provider/model: newapi / deepseek-v4-flash
```

No Hermes source code was modified. The local acceptance run used a runtime
environment key and did not write a key value into the repository.

Validation command:

```bash
NEWAPI_API_KEY="[runtime env]" \
python3 scripts/run_openclaw_hermes_acceptance.py \
  --execute \
  --base-url http://127.0.0.1:8081 \
  --model deepseek-v4-flash \
  --hermes-provider newapi \
  --run-id 20260527-z1-hermes-newapi-p0-rerun \
  --timeout 60
```

Evidence:

```text
test-records/runs/20260527-z1-hermes-newapi-p0-rerun/
```

Observed result:

```text
HTTP /v1/models: status=200
HTTP short-chat: status=200
Hermes help: exit=0, stdout_bytes=9108
Hermes short-chat via provider newapi: exit=0, stdout_bytes=891
OpenClaw short-chat: exit=0
```

The first Hermes short-chat attempt with `-Q/--quiet` timed out after 60s even
though the NewAPI route was reachable. The acceptance runner now uses normal
Hermes chat output for this P0 probe while still suppressing short-chat previews
in the saved report, so full prompts and full model responses are not stored.
