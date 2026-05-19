# ZenProxyRS_M v3.4 Public API 契约

保持 OpenAI-compatible。所有公开 API 不需要认证（或可选代理认证）。

## 路由清单

| 方法 | 路由 | 认证 | 实现状态 |
|---|---|---|---|
| GET | / | 无 | ✅ 已实现 |
| GET | /health | 无 | ✅ 已实现 |
| GET | /metrics | 无 | ✅ 已实现 |
| GET | /v1/models | 无 | ✅ 已实现 |
| ANY | /v1/{*path} | 无 | ✅ 已实现 |

---

## 1. GET /

服务信息。

当前实现（server.rs: index_handler）：`{"service":"zen-proxy-rs","version":"0.2.0","status":"ok"}`

v3.4 保持兼容，可增加 version/mode/uptime。

---

## 2. GET /health

公开健康检查，只返回简单状态。

当前（server.rs: health_handler）：池大小 + uptime + pid + backoff 状态。

v3.4 建议：简约版本 `{"status":"ok|degraded|down","version":"3.4","uptime_secs":123}`，详细状态移至 `/admin/health`。

---

## 3. GET /metrics

Prometheus 格式。当前通过 `PrometheusBackend` 已实现，输出 ~50 个指标 family。v3.4 保持兼容，可新增但不可删除。

---

## 4. GET /v1/models

OpenAI-compatible 模型列表。当前硬编码 2 个模型。

v3.4 目标：数据源从 `ModelRegistry` 来，不再是硬编码。Public API 保持 OpenAI-compatible 格式。

---

## 5. ANY /v1/{*path}

核心代理入口。当前透传方法、路径、body，支持 SSE 流式，支持重试。

v3.4 保持核心转发逻辑不变，逐步通过 ProviderAdapter 抽象上游特化逻辑。

---

## 兼容性保证

v3.4 必须保证以下 API 不变（格式可扩展但不可破坏）：

```bash
# 所有公共端点返回 200
curl -s -o /dev/null -w "%{http_code}" http://localhost:4000/
curl -s -o /dev/null -w "%{http_code}" http://localhost:4000/health
curl -s -o /dev/null -w "%{http_code}" http://localhost:4000/metrics
curl -s -o /dev/null -w "%{http_code}" http://localhost:4000/v1/models
```
