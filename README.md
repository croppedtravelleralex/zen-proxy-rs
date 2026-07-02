# free-model-client-rs

High-performance Rust HTTP reverse proxy for the NewAPI free model channel.
Translates OpenAI and Anthropic chat completion requests to an OpenAI-compatible
NewAPI upstream.

## Quick Start

```bash
# Set required env vars
export FREE_MODEL_API_KEY=sk-your-key
export FREE_MODEL_HOST=0.0.0.0
export FREE_MODEL_PORT=14118

# Build and run
cargo build --release
./target/release/free-model-client-rs
```

## Configuration

| Env Var | Default | Description |
|---------|---------|-------------|
| `FREE_MODEL_HOST` | `127.0.0.1` | Bind address |
| `FREE_MODEL_PORT` | `14118` | Bind port |
| `FREE_MODEL_NEWAPI_URL` | `http://127.0.0.1:8081` | NewAPI base URL |
| `FREE_MODEL_NEWAPI_KEY` | development placeholder | NewAPI API key; set a real value via env |
| `FREE_MODEL_ZEN_CHAT_URL` | derived from `FREE_MODEL_NEWAPI_URL` | Compatibility override for the upstream chat URL |
| `FREE_MODEL_ZEN_API_KEY` | `FREE_MODEL_NEWAPI_KEY` or development placeholder | Compatibility override for the upstream API key |
| `FREE_MODEL_DEEPSEEK_V4_FLASH_UPSTREAM` | `deepseek-v4-flash-free` | Upstream model for `deepseek-v4-flash` |
| `FREE_MODEL_DEEPSEEK_V4_FLASH_LITE_UPSTREAM` | `big-pickle` | Upstream model for `deepseek-v4-flash-lite` |
| `FREE_MODEL_MIMO_V2_5_UPSTREAM` | `mimo-v2.5-free` | Upstream model for `mimo-v2.5` |
| `FREE_MODEL_NORTH_MINI_CODE_UPSTREAM` | `north-mini-code-free` | Upstream model for `north-mini-code` |
| `FREE_MODEL_NEMOTRON_3_ULTRA_UPSTREAM` | `nemotron-3-ultra-free` | Upstream model for `nemotron-3-ultra` |
| `FREE_MODEL_MINIMAX_M3_UPSTREAM` | `minimax-m3-free` | Upstream model for `minimax-m3` |
| `FREE_MODEL_QWEN3_6_PLUS_UPSTREAM` | `qwen3.6-plus-free` | Upstream model for `qwen3.6-plus` |
| `FREE_MODEL_REQUIRE_API_KEY` | `true` (set `0` to disable) | Require client auth |
| `FREE_MODEL_API_KEY` | development placeholder | Client API key; set a real value via env |
| `FREE_MODEL_TIMEOUT_MS` | `120000` | Upstream timeout (ms) |
| `FREE_MODEL_REQUEST_BODY_LIMIT_MB` | `64` | Incoming request body limit in MB |
| `FREE_MODEL_TRUE_FIRST_TOKEN_FRT` | `true` | Delay stream prelude frames until real content/tool output so NewAPI FRT reflects the first real token instead of an empty protocol frame |
| `ZEN_UPSTREAM_SESSION_TTL_SECS` | `3600` | Stable upstream session bucket TTL |

## Models

| Public model | Upstream model |
|--------------|----------------|
| `deepseek-v4-flash` | `deepseek-v4-flash-free` |
| `deepseek-v4-flash-lite` | `big-pickle` |
| `mimo-v2.5` | `mimo-v2.5-free` |
| `north-mini-code` | `north-mini-code-free` |
| `nemotron-3-ultra` | `nemotron-3-ultra-free` |
| `minimax-m3` | `minimax-m3-free` |
| `qwen3.6-plus` | `qwen3.6-plus-free` |

## Endpoints

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| GET | `/health` | No | Health check |
| GET | `/v1/models` | Yes | Free model list |
| POST | `/v1/chat/completions` | Yes | OpenAI chat completions |
| POST | `/v1/messages` | Yes | Anthropic messages |

## Architecture

```
Client (Claude Code / API)
  -> POST /v1/messages (Anthropic) or /v1/chat/completions (OpenAI)
  -> Auth check (Bearer sk-key or x-api-key header)
  -> Protocol translation (Anthropic <-> OpenAI format)
  -> Model mapping (public model -> NewAPI upstream model)
  -> NewAPI upstream fetch (reqwest connection pool, 32 keepalive)
  -> SSE stream parsing (BytesMut zero-copy)
  -> Response formatting (Anthropic/OpenAI SSE or JSON)
  -> Structured error if upstream returns no assistant content or tool call
```

## Runtime Guards

- Client auth accepts `Authorization: Bearer ...` and `x-api-key`.
- Client-specific behavior can be selected with `x-fmc-client`, currently supporting `claude-code`, `hermes`, `openclaw`, `cherrystudio`, `openai-sdk`, `anthropic-sdk`, and `unknown`; automatic inference also checks body markers and tool names.
- Request bodies default to a 64MB limit via `FREE_MODEL_REQUEST_BODY_LIMIT_MB`.
- Explicit `max_tokens` values are passed through; missing `max_tokens` is not filled by this proxy.
- `deepseek-v4-flash`, `mimo-v2.5`, `north-mini-code`, and `nemotron-3-ultra` model families preserve large ClaudeCode context in this proxy instead of applying input compaction.
- `minimax-m3` and `qwen3.6-plus` are exposed as generic OpenCode free models; they do not opt into ClaudeCode/Hermes/OpenClaw deep compatibility policies.
- Empty upstream assistant content without tool calls is not converted into fake tool calls.
- Desensitized request-shape logs record token counts, message/tool counts, request kind, and prompt hash only; raw prompts, request bodies, and API keys are not logged.

## Deployment

### PM2
```bash
FREE_MODEL_HOST=0.0.0.0 pm2 start target/release/free-model-client-rs --name free-model-rs
```

### Memory
~5-15 MB RSS (vs ~85 MB for Node.js version)

## Build
```bash
cargo build --release     # Optimized binary
cargo test                # 69 library tests + 71 kernel golden tests
```
