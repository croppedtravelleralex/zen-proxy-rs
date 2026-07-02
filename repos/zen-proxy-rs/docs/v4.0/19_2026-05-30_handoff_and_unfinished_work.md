# 2026-05-30 Handoff And Unfinished Work

## Purpose

This document is the handoff point for the interrupted V4.8 client-acceptance work. It records only what was verified, what was not verified, and what still has to be implemented or tested.

Do not treat this file as proof that V4.8 passed. It is a recovery document for the next maintainer or AI session.

## Current Ground Truth

The latest user correction changed the immediate test boundary:

```text
Primary requested test path for this handoff:
WSL Hermes / WSL OpenClaw -> panda NewAPI -> closeapi

Do not count that path as ZenProxy acceptance unless panda NewAPI channel logs prove it forwarded the request to ZenProxy.
```

The original V4 production target remains:

```text
client -> NewAPI -> ZenProxyRS -> embedded free-model-client-rs -> proxy node -> upstream
```

These are different validation targets. The next operator must keep them separate in reports.

## Verified During The Last Investigation

### panda NewAPI Reachability

From WSL, with proxy environment variables removed, the tailnet address worked:

```text
base URL: http://100.69.228.93:8081
endpoint: /v1/models
status: 200
elapsed: about 501 ms
```

The model list included:

```text
deepseek-v4-flash
deepseek-v4-flash-lite
gpt-5.5
gpt-5.4
gpt-5.4-mini
```

The public addresses previously tried from WSL were not reliable for this test:

```text
http://43.156.233.219:8081 -> timed out
http://8.163.32.25:8081   -> timed out
```

### panda NewAPI OpenAI-Compatible Minimal Call

The OpenAI-compatible endpoint on panda NewAPI returned a normal answer:

```text
POST /v1/chat/completions
model: deepseek-v4-flash
stream: false
status: 200
elapsed: about 3204 ms
assistant content: PONG
returned model name: deepseek-v4-flash-free
```

Streaming also returned HTTP 200:

```text
POST /v1/chat/completions
model: deepseek-v4-flash
stream: true
status: 200
protocol first byte: about 1663 ms
first real content appeared after empty assistant-role deltas
assistant content fragments: P + ONG
```

Important interpretation:

```text
The stream produced empty protocol deltas before real content. Reports must not use protocol_first_byte as first_content.
```

### panda NewAPI Anthropic-Compatible Minimal Call

The Anthropic-compatible endpoint on panda NewAPI returned a normal answer:

```text
POST /v1/messages
model: deepseek-v4-flash
stream: false
status: 200
elapsed: about 2250 ms
assistant content: PONG
returned model name: deepseek-v4-flash-free
```

### Direct closeapi Baseline

Direct access to `https://sub2api.closeapi.top/v1/models` returned Cloudflare 403 with error code 1010 in the earlier investigation. That direct baseline is not the same as testing through panda NewAPI.

### Hermes Local State

The WSL Hermes executable exists and starts:

```text
executable: /home/lenovo/.local/bin/hermes
version: Hermes Agent v0.14.0
```

`hermes status` showed:

```text
default model: deepseek-v4-flash
default provider: closeapi
gateway: running
Telegram: configured
active sessions: 3
```

The relevant config state at the time of handoff:

```text
custom provider closeapi:
  base_url: https://sub2api.closeapi.top/v1
  model: deepseek-v4-flash

custom provider newapi:
  base_url: http://127.0.0.1:8081/v1
  model: deepseek-v4-flash
```

Therefore, Hermes was not yet proven to use panda NewAPI. Its default provider was still `closeapi`, and its `newapi` provider still pointed at local `127.0.0.1:8081`.

### Hermes Gateway Ownership Fix Already Done

Before this handoff, a root-owned Hermes gateway process and root-owned lock file had caused user-service restart problems. The earlier corrective action was:

```text
kill the root-owned gateway process
remove /home/lenovo/.hermes/gateway.lock
chown /home/lenovo/.hermes and /home/lenovo/.local/state/hermes back to lenovo
restart hermes-gateway.service as the user service
```

Later status still contained this warning:

```text
Service: installed but not managing the current running gateway
```

That warning is unresolved. Do not assume Telegram-driven Hermes tests are healthy until this service-manager mismatch is checked.

### OpenClaw Local State

OpenClaw is installed under:

```text
/home/lenovo/.local/node_modules/openclaw
```

Package metadata showed:

```text
version: 2026.5.22
required node: >=22.19.0
```

The current default WSL runtime showed:

```text
node: /usr/bin/node
node version: v20.20.2
npm version: 10.8.2
```

Therefore OpenClaw was not ready for execution. It needs an isolated Node 22.19+ runtime before real panda NewAPI tests can be run.

## Not Verified Yet

The following items are not complete and must not be reported as passed:

```text
1. Hermes -> panda NewAPI -> closeapi full client call.
2. Hermes tool-call test through panda NewAPI.
3. Hermes web/search capability through panda NewAPI.
4. Hermes Telegram-bot path through panda NewAPI.
5. OpenClaw one-shot chat through panda NewAPI.
6. OpenClaw tool-call test through panda NewAPI.
7. OpenClaw web/search capability through panda NewAPI.
8. OpenClaw subagent or task-like behavior through panda NewAPI.
9. panda NewAPI channel/log proof that a given request used closeapi or ZenProxy.
10. Any formal V4.8 500-round run for any client.
11. Any four-client parallel pressure run.
12. Any 24-hour production soak.
13. Any latest evidence reconciliation between NewAPI logs and ZenProxy admin/audit rows.
```

## Unimplemented Or Incomplete Work

### 1. Four-Client 500-Round Harness

`docs/v4.0/18_v4.8_four_client_500_round_acceptance.md` defines the 99+ acceptance target, but the full orchestrator is not implemented as a single repeatable production harness.

Still needed:

```text
per-client runner abstraction
500-round case scheduler per client
mixed token/body/tool/subagent scenario generator
client stdout/stderr/timing collector
NewAPI log importer for panda
ZenProxy admin/audit importer for panda when ZenProxy is in the path
request correlation and attribution report
resume support after interrupted runs
stop-condition enforcement
redaction check before writing artifacts
```

Existing scripts are useful building blocks, not the complete V4.8 harness:

```text
scripts/run_chain_smoke.py
scripts/run_ttft_experiment.py
scripts/run_openclaw_hermes_acceptance.py
scripts/collect_test_record.py
```

### 2. panda-Only Execution Discipline

The latest requirement says not to use local WSL as the formal execution host. The acceptance work must run on panda or prove that traffic leaves from panda.

Still needed:

```text
install or verify required clients on panda
pin client versions in the run manifest
run smoke from panda before pressure
record source host and source IP evidence
prevent local WSL results from being mixed into panda acceptance reports
```

### 3. Hermes Temporary panda Provider

Hermes needs a temporary, reversible provider configuration for panda NewAPI. Do not overwrite the user's normal config without a backup.

Recommended safe approach:

```text
backup ~/.hermes/config.yaml
add or override a provider named panda-newapi
base_url = http://100.69.228.93:8081/v1
key_env = PANDA_NEWAPI_API_KEY
run only explicit --provider panda-newapi commands
restore or leave the provider clearly documented
```

The next operator must first check whether Hermes supports an alternate `HERMES_HOME` or project config path. If it does, prefer that over modifying `~/.hermes/config.yaml`.

### 4. Hermes Gateway Manager Mismatch

The Hermes gateway status warning must be resolved before Telegram-based tests:

```text
Service installed but not managing the current running gateway
```

Still needed:

```text
list all hermes gateway PIDs
check systemd user service ExecStart and PID
check ~/.hermes/gateway.lock ownership and content
restart via systemctl --user only after confirming no user task is active
verify hermes status no longer reports manager mismatch
```

### 5. OpenClaw Node 22 Runtime

OpenClaw cannot be accepted while the default runtime is Node 20.

Still needed:

```text
install isolated Node >=22.19.0 under the user account or use an existing one
do not replace system /usr/bin/node blindly
prepend the isolated Node path only for OpenClaw tests
run openclaw --help and record version/runtime
discover the exact OpenClaw provider config format
configure panda NewAPI with redacted credentials
```

### 6. OpenClaw Provider Configuration

OpenClaw config was not fully inspected before the investigation was interrupted. The next operator must discover the supported configuration shape from installed docs or `openclaw --help` after Node is fixed.

Do not assume OpenAI environment variable names are enough until confirmed.

Minimum evidence required:

```text
effective model provider name
effective base URL
effective model
whether it uses OpenAI chat, OpenAI responses, Anthropic messages, or a custom transport
one successful minimal call through panda NewAPI
```

### 7. panda NewAPI Channel Proof

The latest minimal requests proved panda NewAPI works. They did not prove which NewAPI channel served them.

Still needed on panda:

```text
identify NewAPI database/container/service
query recent logs for the exact request time window
record channel id, model, status, duration, stream flag, request id
verify whether the upstream was closeapi, ZenProxy, or another provider
redact keys, usernames, headers, bodies, and raw prompts
```

If the channel is closeapi, those results are valid for client compatibility with panda NewAPI but not for ZenProxy V4 acceptance.

### 8. ZenProxy V4.8 Acceptance Not Yet Re-Run

After the latest changes and interrupted tests, no fresh full-chain proof exists for:

```text
NewAPI -> ZenProxy -> embedded free-model-client-rs -> proxy node -> upstream
```

Still needed if the goal returns to ZenProxy acceptance:

```text
/v1/models via NewAPI channel pointing to ZenProxy
OpenAI stream/non-stream smoke
Anthropic stream/non-stream smoke
tool history repair smoke
Hermes-style mixed text + tool_result history
OpenClaw-style tool history
large non-stream guard smoke
large context compactor smoke
lane isolation under mixed pressure
proxy pool rotation and dead-pool probe evidence
Redis global budget evidence
admin/audit/NewAPI reconciliation
```

## Immediate Next Steps

Run these in order. Stop at the first failure and document it before changing anything else.

### Step 1: Freeze The Target Chain

Write the target at the top of the run notes before testing:

```text
Test A: WSL Hermes/OpenClaw -> panda NewAPI -> closeapi
or
Test B: client -> panda/local NewAPI -> ZenProxy -> free-model-client-rs -> upstream
```

Do not mix evidence between Test A and Test B.

### Step 2: Re-Probe panda NewAPI

Use the tailnet address from WSL or panda itself:

```text
GET  http://100.69.228.93:8081/v1/models
POST http://100.69.228.93:8081/v1/chat/completions
POST http://100.69.228.93:8081/v1/messages
```

Record only:

```text
status
elapsed_ms
model
stream flag
first protocol byte
first real content
error class
redacted request id if present
```

### Step 3: Prove panda NewAPI Channel

Query panda NewAPI logs for the exact time window and record the channel. This is mandatory before making any statement about closeapi or ZenProxy involvement.

### Step 4: Fix OpenClaw Runtime Without Touching System Node

Install or locate Node 22.19+ in an isolated path and run:

```text
openclaw --help
openclaw version or equivalent version command
```

Only after that, configure panda NewAPI.

### Step 5: Run Hermes Minimal panda Test

Use an explicit provider pointing at panda NewAPI. Do not rely on Hermes' current default `closeapi` provider.

Minimum cases:

```text
short answer: only reply PONG
file/tool task in a temporary directory
web/search task if the tool is configured
```

### Step 6: Run OpenClaw Minimal panda Test

After Node and provider config are proven:

```text
short answer
file/tool task in a temporary directory
web/search task if configured
```

### Step 7: Decide Whether To Resume V4.8 500-Round Acceptance

Do not start 500-round pressure until the minimal client tests pass and channel logs prove the intended upstream path.

## Acceptance Gates Still Open

The following V4.8 gates are still open:

```text
P0 config freeze: incomplete for Hermes/OpenClaw panda target
P1 four clients, 10 smoke rounds each: not run
P2 four clients, 50 pre-pressure rounds each: not run
P3 one client at a time, full 500 rounds: not run
P4 two clients in parallel, 500 rounds each: not run
P5 four clients in parallel, 500 rounds each: not run
P6 boundary scenario补测: not run
P7 evidence reconciliation and report: not run
```

## Reporting Rules For The Next Session

Use this wording discipline:

```text
"panda NewAPI responded 200" means only NewAPI responded.
"panda NewAPI used closeapi" requires NewAPI log/channel evidence.
"ZenProxy path passed" requires NewAPI -> ZenProxy request correlation or direct ZenProxy evidence.
"Hermes passed" requires Hermes command evidence, not raw curl.
"OpenClaw passed" requires OpenClaw command evidence with Node 22+ runtime.
"first byte" must be split into protocol_first_byte and first_content.
```

Never report a raw curl test as Hermes/OpenClaw acceptance.

## Security And Safety Boundary

Security-defense tests are allowed only in controlled local or owned test fixtures. Do not run real public-target exploitation, credential theft, persistence, evasion, or weaponized payload tests as part of this acceptance suite. The safe coverage target is:

```text
prompt-injection resistance
tool-output injection resistance
secret redaction
environment-leak prevention
safe refusal for clearly harmful requests
defensive code review and local sandbox checks
```

## Final Handoff Summary

Current status is:

```text
panda NewAPI minimal OpenAI call: verified 200
panda NewAPI minimal Anthropic call: verified 200
panda NewAPI model list: verified 200
panda NewAPI actual channel/upstream: not verified in logs during this handoff
Hermes installed: verified
Hermes default provider: closeapi, not panda NewAPI
Hermes newapi provider: local 127.0.0.1:8081, not panda
Hermes through panda NewAPI: not verified
OpenClaw installed: verified
OpenClaw runtime: blocked by Node v20.20.2, requires >=22.19.0
OpenClaw through panda NewAPI: not verified
V4.8 4-client 500-round acceptance: not implemented as a full harness and not run
ZenProxy full-chain acceptance after latest interruption: not re-run
```

The next session should not start with architectural claims. It should first reconfirm the target chain, probe panda NewAPI, inspect panda NewAPI logs for channel evidence, then unblock Hermes/OpenClaw one client at a time.
