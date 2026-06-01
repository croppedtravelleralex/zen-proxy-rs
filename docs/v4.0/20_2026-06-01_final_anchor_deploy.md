# 2026-06-01 Final-Anchor Deployment And Smoke

## Purpose

Record the panda deployment that adds ClaudeCode huge-context final-user anchoring and buffered empty-output retry behavior from the embedded `free-model-client-rs` kernel.

This document is not a V4.8 full acceptance report. It only records the source-side deployment and panda-local smoke evidence.

## Deployment

| Item | Value |
|---|---|
| Target | panda `/opt/zen-proxy-rs/zen-proxy-rs` |
| Previous deployed hash | `94872cd91a558ec431e176af5a1fa8f257219c9518df3f729e7e5645b5cbb937` |
| Local release hash | `c44df2fb8ecae44a5c155e88953574e6990f07947b45b71e1f60468f2c00c06e` |
| Deployed stripped hash | `3a27f2c7cda56119b32dfc42738b06f3f1e08155a0ff89c48daca6ddc8aed1d4` |
| Backup | `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-final-anchor-20260601-94872cd` |
| Instances | `zen-proxy-rs@1:4001`, `zen-proxy-rs@2:4002`, `zen-proxy-rs@3:4004` |

Build command:

```bash
cd /home/lenovo/zen-proxy-rs
CARGO_INCREMENTAL=0 cargo build --release
```

The build uses the local path dependency:

```text
free-model-client-rs = { path = "../free-model-client-rs" }
```

## Health Evidence

After restart:

```text
zen-proxy-rs@1: active, /health 200
zen-proxy-rs@2: active, /health 200
zen-proxy-rs@3: active, /health 200
pools: total=90, dispatch=90, dead=0, ratelimited=0
upstream.backoff=false
```

## Smoke Evidence

panda-local NewAPI checks:

```text
GET  http://127.0.0.1:8081/v1/models -> 200
POST http://127.0.0.1:8081/v1/chat/completions deepseek-v4-flash non-stream -> OK
```

Huge stream source-side smoke:

| Model | Rounds | Result | Timing |
|---|---:|---|---|
| `deepseek-v4-flash` | 3 | 3/3 returned `HUGE_OK` | about 2.5s, 2.7s, 3.1s |
| `deepseek-v4-flash-lite` | 3 | 3/3 returned `HUGE_OK` | about 3.2s, 3.3s, 14.8s |

Request shape:

```text
endpoint: /v1/messages
stream: true
body size: about 1.0MB
source client inferred by ZenProxy: claude-code
```

ZenProxy log evidence:

```text
v4 ingress request path="messages" ... stream_seen_by_zenproxy=true source_client=claude-code
compacted streaming anthropic context before upstream before_tokens=251103 after_tokens=12009 compacted_messages=1 appended_latest_user_anchor=true
```

## Interpretation

The deployment proves that the source-side final-anchor path is active on panda and that the previous stale-context drift string, such as `git diff` or `inspect current state`, did not appear in the panda-local huge smoke.

The logs still show occasional upstream empty output:

```text
ClaudeCode huge stream buffered upstream returned empty output
```

That condition was retried by the kernel and did not leak to the smoke client. Formal dry run must still count its rate and tail-latency impact.

## Still Not Accepted

This smoke does not replace real client acceptance.

Still required:

```text
Windows ClaudeCode dry run
WSL ClaudeCode dry run
WSL Hermes dry run
WSL OpenClaw dry run
OpenClaw subagent dry-run-level verification
Hermes slow-path attribution
NewAPI/ZenProxy request reconciliation
```

Do not start the four-client 500-round full run until the dry run passes.
