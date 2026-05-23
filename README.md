# free-model-client-rs

High-performance Rust HTTP reverse proxy for the OpenCode Zen free model API.
Translates OpenAI and Anthropic chat completion requests to Zen upstream format.

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
| `FREE_MODEL_ZEN_CHAT_URL` | `https://opencode.ai/zen/v1/chat/completions` | Zen upstream URL |
| `FREE_MODEL_ZEN_API_KEY` | `public` | Zen API key |
| `FREE_MODEL_REQUIRE_API_KEY` | `true` (set `0` to disable) | Require client auth |
| `FREE_MODEL_API_KEY` | `sk-dev` | Client API key |
| `FREE_MODEL_TIMEOUT_MS` | `120000` | Upstream timeout (ms) |

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
  -> Zen upstream fetch (reqwest connection pool, 32 keepalive)
  -> SSE stream parsing (BytesMut zero-copy)
  -> Response formatting (Anthropic/OpenAI SSE or JSON)
  -> Tool synthesis fallback (if upstream empty: Read/Bash/Task)
```

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
cargo test                # 44 unit tests
```
