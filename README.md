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
| `FREE_MODEL_REQUIRE_API_KEY` | `true` (set `0` to disable) | Require client auth |
| `FREE_MODEL_API_KEY` | development placeholder | Client API key; set a real value via env |
| `FREE_MODEL_TIMEOUT_MS` | `120000` | Upstream timeout (ms) |
| `FREE_MODEL_REQUEST_BODY_LIMIT_MB` | `64` | Incoming request body limit in MB |
| `ZEN_UPSTREAM_SESSION_TTL_SECS` | `3600` | Stable upstream session bucket TTL |

## Models

| Public model | Upstream model |
|--------------|----------------|
| `deepseek-v4-flash` | `deepseek-v4-flash-free` |
| `deepseek-v4-flash-lite` | `big-pickle` |

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
- Non-stream responses cap excessive output before upstream:
  - missing `max_tokens`: 2048
  - small prompt: max 4096
  - estimated prompt >= 50k tokens: max 2048
  - estimated prompt >= 100k tokens: max 1024
- Stream responses keep explicit `max_tokens` and default to 1024 when omitted.
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
