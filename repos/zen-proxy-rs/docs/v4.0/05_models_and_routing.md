# V4.0 Models and Routing

## Public Models

V4.0 exposes exactly two public models:

```text
deepseek-v4-flash
deepseek-v4-flash-lite
```

`GET /v1/models` must return only these two model ids.

V4.108 Phase 1 adds side-channel opencode model discovery. This does not change
the default public model contract. V4.109 may expose discovered free-looking
candidates directly only when `DYNAMIC_MODEL_PUBLIC_MODE=candidate_canary_or_active`
is explicitly enabled for a private self-use/test channel where users cannot
choose the candidate list. That mode is a test-channel shortcut, not a
canary/active promotion.

## Upstream Mapping

```text
deepseek-v4-flash      -> deepseek-v4-flash-free
deepseek-v4-flash-lite -> big-pickle
```

The public model name should be preserved in request records. The upstream model
name should also be recorded for debugging and capacity analysis.

## Model Registry Rules

- Unknown public model returns a structured 400 error.
- Do not expose upstream-only model names directly.
- Do not let model mapping live in protocol formatting code.
- Do not inject system prompts for model behavior in V4.0.
- Do not auto-promote dynamically discovered models into `/v1/models`.
- Do not apply ClaudeCode/Hermes/OpenClaw behavior policies to a new model until
  that model has an explicit compatibility profile.

## Dynamic Discovery Candidate Rules

Phase 1 candidate detection is deliberately conservative:

```text
candidate if id == big-pickle
candidate if id ends_with "-free"
ignored otherwise
```

Every candidate starts as:

```text
probe_required=true
auto_promoted=false
public=false
```

The admin API may show these candidates. The data plane must still reject an
unknown candidate model by default. It may route candidates only in explicit
private `candidate_canary_or_active` mode, and that must still keep
`probe_required=true` and `auto_promoted=false` until the probe/canary gates
are actually satisfied.

## Routing Inputs

Route selection may consider:

- public model
- upstream model
- stream or non-stream
- request body size
- client identity hash
- current pool health
- recent 429 rate
- recent timeout rate

## Sticky Policy

Sticky retry is required:

```text
first attempt selects node A
retry should try node A first when policy allows
only then fall back to a new node
```

This avoids burning multiple exit IPs for a single upstream failure.

## Future Provider Routing

V4.0 starts with one provider:

```text
FreeModelProviderAdapter
```

Future providers must be added behind `ProviderAdapter`, not by adding
provider-specific branches to `proxy.rs`.
