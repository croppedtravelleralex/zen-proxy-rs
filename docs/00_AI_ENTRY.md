# Zen Proxy RS — 当前项目入口与文档索引

## 项目定位

zen-proxy-rs 是一个 Rust 单进程 LLM API 反代核心，架构为：

```
New-API -> zen-proxy-rs -> WebShare SOCKS5 -> 出口 IP -> opencode.ai/zen
```

当前使用 100 个 WebShare 节点组成五状态机池（Dispatch/Active/RateLimited/Dead/Probe），提供 OpenAI 兼容接口转发。

## 当前真实代码入口

| 文件 | 职责 |
|---|---|
| `src/main.rs` | 主入口，配置加载、路由注册、后台任务 |
| `src/config.rs` | 从环境变量加载配置 |
| `src/proxy.rs` | 核心转发：模型映射、鉴权、重试、SSE、账本记录 |
| `src/ledger.rs` | **本轮新增**：节点/限流多维内存账本 + JSONL 写入 |
| `src/opencode_headers.rs` | **本轮新增**：official opencode headers 注入 |
| `src/sse.rs` | **本轮新增**：frame-aware SSE buffer |
| `src/state.rs` | AppState |
| `src/pool/` | 五池 trait 实现 |
| `src/collector/` | 遥测采集、WAL、聚合、导出 |
| `src/provider/webshare.rs` | WebShare SOCKS5 provider |
| `src/server.rs` | admin handlers（/admin/pools、/admin/fuse、/admin/health、/admin/nodes） |
| `tests/e2e_integration.rs` | E2E 测试 |

## 本轮已实现功能

1. **节点/限流账本**（`src/ledger.rs`）：内存多维聚合（by_node/by_model/by_key/by_stream），429/5xx/network/pool transition 写 JSONL，敏感字段脱敏（redact_node_url、short_hash）。JSONL 路径：`/tmp/zen-proxy-ledger-events.jsonl`
2. **opencode headers 注入**（`src/opencode_headers.rs`）：配置开关，注入 User-Agent、x-opencode-client/project/session/request，session 按 client 分桶，request 每次唯一。
3. **SSE 兼容修复**（`src/sse.rs`）：frame-aware 缓冲而非按 TCP chunk 硬 patch，修 `delta.reasoning_content → delta.content`，`[DONE]` 后丢弃额外事件。
4. **`GET /admin/nodes`**：返回账本摘要，复用 admin auth，E2E 验证通过。

## 当前真实路由

| 路由 | 鉴权 | 说明 |
|---|---|---|
| GET / | 无 | service info |
| GET /health | 无 | 池大小、fuse、backoff |
| GET /metrics | 无 | Prometheus |
| GET /v1/models | 无 | 入口模型列表 |
| ANY /v1/* | 无 | 代理转发 |
| GET /admin/pools | x-api-key | 池状态 |
| GET /admin/fuse | x-api-key | fuse 状态 |
| GET /admin/health | x-api-key | pool + upstream |
| GET /admin/nodes | x-api-key | **本轮新增**：账本统计 |

## 关键文档及其可信度

| 文档 | 可信度 | 说明 |
|---|---|---|
| `docs/10_429_根因分析.md` | ✅ 当前事实 | 429 根因分析、已验证的 IP 限流、fallback 低额度假设 |
| `docs/00_AI_ENTRY.md` | ✅ 本文已更新 | 当前项目入口，反映本轮实现 |
| `docs/01_概览与架构.md` 等 | ⚠️ 目标态/历史态 | 与当前代码有偏移，不全部可信 |
| `API_SPEC.md`（根目录） | ⚠️ 目标态 | 包含大量未实现模块定义 |
| `FEATURE_GAP.md`（根目录） | ⚠️ 部分过时 | 引用旧架构，已有偏移 |

## 验证命令

```bash
cargo fmt --check
cargo check
cargo test   # 42 passed, 0 failed（E2E admin 旧路由 404 为基线问题）
```

## 本轮未完成

- `/admin/nodes` 暂不支持 `?model=`、`?stream=` 等 query 过滤（后续增强）
- `retry_after`、`tokens`、`exit_ip` 字段已声明但尚未从上游响应中捕获（后续迭代）
