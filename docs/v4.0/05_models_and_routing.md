# V4.0 Models and Routing

## Public Models

V4.0 exposes exactly two public models:

```text
deepseek-v4-flash
deepseek-v4-flash-lite
```

`GET /v1/models` must return only these two model ids.

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

