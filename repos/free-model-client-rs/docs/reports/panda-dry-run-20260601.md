# Panda Four-Client Dry Run Report - 2026-06-01

## Scope

Dry run only. This is not the 4 x 500 full acceptance run.

Chain:

```text
client -> panda NewAPI http://100.69.228.93:8081 -> ZenProxyRS -> free-model-client-rs kernel -> upstream
```

API key is recorded as `sk-***` only.

## Runs

| Client | Run dir | Rounds |
|---|---|---:|
| WSL ClaudeCode | `.codex_tmp/panda-pressure/20260601-1738-dry-wsl-three` | 50 |
| WSL Hermes | `.codex_tmp/panda-pressure/20260601-1738-dry-wsl-three` | 50 |
| WSL OpenClaw | `.codex_tmp/panda-pressure/20260601-1912-dry-openclaw` | 50 |
| Windows ClaudeCode | `.codex_tmp/panda-pressure/20260601-1932-dry-windows-claudecode` | 50 |

## Result Summary

| Client | API OK | Semantic OK | Main errors | P50 total | P90 total | P99 total | P90 first content |
|---|---:|---:|---|---:|---:|---:|---:|
| Windows ClaudeCode | 50/50 | 43/50 | 6 context_drift, 1 subagent_not_triggered | 7.8s | 27.3s | 39.3s | 20.0s |
| WSL ClaudeCode | 50/50 | 44/50 | 6 context_drift | 6.5s | 23.8s | 64.2s | 23.7s |
| WSL Hermes | 50/50 | 50/50 | none | 54.3s | 69.5s | 103.5s | 69.4s |
| WSL OpenClaw | 50/50 | 49/50 | 1 context_drift | 14.6s | 32.7s | 66.6s | 32.6s |

Global:

```text
total rounds: 200
API OK: 200/200
semantic OK: 186/200
protocol/model/auth/502/504/300s timeout: 0 observed in runner summary
panda health after run: 3/3 instances healthy, total=90 dispatch=90 dead=0 ratelimited=0
```

## What Passed

- Model discovery and minimal chat preflight passed.
- No request failed at API connectivity/auth/model layer.
- No client hit 300s timeout in this dry run.
- WSL Hermes completed 50/50 semantically, including long and huge contexts.
- WSL OpenClaw completed 49/50 semantically; subagent observed 5/5.
- WSL ClaudeCode and Windows ClaudeCode short, medium, long, tool, and most subagent cases generally worked.

## Blocking Issues

### 1. ClaudeCode huge_context still drifts

Both WSL and Windows ClaudeCode failed all six huge_context cases.

Observed behavior:

```text
Instead of answering the final controlled instruction, the model tries to read ClaudeCode transcript JSONL files, inspect git status, or continue an older task.
```

This means the source-side final-anchor deployment fixed the panda-local huge stream smoke, but real ClaudeCode huge prompts still carry enough ClaudeCode transcript/session pressure to override the final test instruction.

### 2. Windows ClaudeCode runner uses a UNC workspace

Windows ClaudeCode was run from `\\wsl.localhost\...`. CMD reports:

```text
UNC paths are not supported. Defaulting to Windows directory.
```

This affected at least one subagent case and made the failure partially a runner/workspace issue, not only a model/proxy issue.

### 3. Hermes is functionally stable but too slow

Hermes passed 50/50, but latency is not acceptable for full acceptance:

```text
P50 total: about 54s
P90 total: about 69s
P99 total: about 103s
```

This is likely Hermes CLI/gateway/agent-loop overhead plus model latency. It is not currently a ZenProxy 5xx or timeout issue.

### 4. OpenClaw has one long_context drift

OpenClaw passed 49/50. The single failed case was `long_context` on `deepseek-v4-flash-lite`; output tried to read the tail/final instruction instead of directly answering.

OpenClaw also logged a local gateway secrets warning in that sample, but resolved secrets locally and continued.

## Current Decision

Do not start the 4 x 500 full run yet.

Full run is blocked by:

```text
ClaudeCode huge_context semantic success: not met
Hermes latency gate: not met
Windows ClaudeCode runner path hygiene: not met
OpenClaw long_context/lite drift: needs one fix or repeat confirmation
```

## Recommended Next Fixes

1. Add a ClaudeCode-specific huge-context compaction policy that preserves the final user instruction as the final message even when ClaudeCode session transcript/tool history is present.
2. Add a server-side guard for ClaudeCode huge_context that strips or downgrades old transcript-read / resume-current-workflow pressure before upstream, without modifying ClaudeCode itself.
3. Run Windows ClaudeCode from a real Windows workspace path, not `\\wsl.localhost`, before judging Windows subagent behavior.
4. Split Hermes timing into CLI startup, gateway/agent loop, NewAPI/ZenProxy request time, and model response time.
5. Re-run dry run after fixes before any full 500-round run.
