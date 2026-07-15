# V4.0 Models and Routing

## Public Models

Production exposes exactly four stable public models:

```text
deepseek-v4-flash
big-pickle
mimo-v2.5
hy3
```

`GET /v1/models` must return only these four stable model ids unless an
explicit private candidate mode is enabled. `deepseek-v4-flash-lite` has been
withdrawn from the public contract.

V4.108 Phase 1 adds side-channel opencode model discovery. This does not change
the default public model contract. V4.109 may expose discovered free-looking
candidates directly only when `DYNAMIC_MODEL_PUBLIC_MODE=candidate_canary_or_active`
is explicitly enabled for a private self-use/test channel where users cannot
choose the candidate list. That mode is a test-channel shortcut, not a
canary/active promotion.

## Upstream Mapping

```text
deepseek-v4-flash -> deepseek-v4-flash-free
big-pickle        -> big-pickle
mimo-v2.5         -> mimo-v2.5-free
hy3               -> hy3-free
```

The public model name should be preserved in request records. The upstream model
name should also be recorded for debugging and capacity analysis.

`hy3` uses the `StaticGeneric` compatibility profile. Do not inherit Mimo or
ClaudeCode-specific request rewriting implicitly; any hy3 behavior change must
be covered by explicit protocol tests.

## Model Registry Rules

- Unknown public model returns a structured 400 error.
- Do not expose upstream-only model names directly.
- Do not let model mapping live in protocol formatting code.
- Do not inject system prompts for model behavior in V4.0.
- Do not auto-promote dynamically discovered models into `/v1/models`.
- Stable publication requires an explicit static public-to-upstream mapping;
  discovery alone is not a production promotion decision.
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

`hy3-free` is no longer only a generic candidate: it has an explicit stable
mapping to public `hy3`. Other discovered `-free` models remain candidates by
default.

## NewAPI External Contract

NewAPI is outside this repository, but production handoff depends on these
facts:

- channel 69 publishes the same four public names;
- `hy3` has enabled abilities for `defualt` and `oc`;
- `ModelPrice.hy3=0` and `ModelRatio.hy3=0`;
- authorized `/v1/models` was verified to include `hy3` on 2026-07-08.

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
