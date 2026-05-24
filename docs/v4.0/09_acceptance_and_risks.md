# V4.0 Acceptance and Risks

## Acceptance Gates

### Structure Gate

- `proxy.rs` does not own provider protocol details.
- pool code does not build Zen request bodies.
- transport code does not know model mapping.
- FreeModel kernel does not choose proxy nodes.

### API Gate

- `/v1/models` returns only:
  - `deepseek-v4-flash`
  - `deepseek-v4-flash-lite`
- `/v1/chat/completions` supports stream and non-stream.
- `/v1/messages` supports stream and non-stream.

### Egress Gate

Must prove:

```text
selected node -> selected transport client -> actual upstream request
```

Without this proof, V4.0 is not complete.

### Failure Gate

Fault injection must cover:

- 429
- 500
- timeout
- connection refused
- SOCKS failure
- partial SSE
- broken JSON
- slow first token

Expected outcomes:

| Fault | Expected result |
|---|---|
| 429 | node enters RateLimited |
| timeout | node enters Dead |
| connection refused | node enters Dead |
| 500 | retry/backoff, then policy result |
| partial SSE | structured upstream/stream error |
| slow first token | TTFT recorded |

### Release Gate

Required commands:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

Performance must be measured from release artifacts, not dev mode.

## Rollback

V4.0 must keep a runtime switch until acceptance is complete:

```text
ZEN_PROVIDER_MODE=legacy
ZEN_PROVIDER_MODE=free_model_kernel
```

Rollback must not require rebuilding.

## Known Shortcomings

- V4.0 does not make the upstream model smarter.
- V4.0 does not bypass upstream rate limits.
- Proxy quality remains an external constraint.
- Anthropic streaming and tool-call compatibility are the highest-risk protocol
  areas.
- Observed exit IP may require a test endpoint when Zen does not expose it.

## Score Target

The V4.0 design targets a 99-point engineering standard:

- interface contracts are explicit.
- selected egress is provable.
- protocol behavior is golden-tested.
- failure behavior is injected and verified.
- rollout is configurable and reversible.
- release gates are mandatory.

The remaining uncertainty is long-running production evidence.

