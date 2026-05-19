# ZenProxyRS_M v3.4 Admin API 总体契约

## 0. 数据源权威：API 优先

### 设计原则

**Admin API 是运行时状态管理的唯一权威路径。JSON 配置文件只做启动加载。**

```
Admin API (POST/PATCH/DELETE) → 运行时内存状态
        ↑
JSON 配置文件 → 进程启动时加载 → 仅做初始状态
        ↑
SIGHUP → 重新加载 JSON 配置文件 → 重置运行时状态（相当于"恢复出厂"）
```

### 含义

| 操作 | 数据流向 |
|---|---|
| API 创建 provider | 写入内存，JSON 文件不变 |
| API 删除节点 | 删除内存，JSON 文件不变 |
| 运维改 JSON 文件 | 需发 SIGHUP 或重启才能生效 |
| 重启进程 | 从 JSON 文件重建初始状态 |
| API 的增删改不持久化到磁盘 | 依赖 WAL 做 crash recovery |

### 原因

1. 避免双写冲突——运行时和配置文件互相覆盖是最常见的运维事故
2. JSON 文件作为"基础设施即代码"的声明式配置（启动时确保基线）
3. API 做运行时热调整（临时禁用某个节点、切换 route）
4. 如果希望 API 的变更持久化，后续可加 `POST /admin/config:save` 将当前状态写回 JSON

---

## 1. 统一认证

### 当前（v3.0）

内联函数 `check_admin_auth()`（server.rs）：检查 `x-api-key` header 是否匹配 `config.admin_api_key`。无中间件层，无审计。

### v3.4 目标

```
Authorization: Bearer <token>
```

向下兼容 `x-api-key`。统一成 admin auth middleware（Tower 层或 Axum 提取器）。

P0 可以只有一个 admin token。后续可支持：

| 权限 | 说明 |
|---|---|
| read | 查询类操作 |
| write | 创建/更新操作 |
| operate | 开关/探测/切换 |

**关键要求：** 不能依赖"没配置 key 就放行"，认证失败返回 401。

---

## 2. 通用响应格式

### 成功

```json
{
    "success": true,
    "data": {},
    "error": null,
    "meta": {
        "request_id": "req_abc123",
        "timestamp": "2026-05-19T12:00:00Z"
    }
}
```

### 错误

```json
{
    "success": false,
    "data": null,
    "error": {
        "code": "NODE_NOT_FOUND",
        "message": "node not found: abc123",
        "details": {}
    },
    "meta": {
        "request_id": "req_abc123",
        "timestamp": "2026-05-19T12:00:00Z"
    }
}
```

### 错误码清单

| 错误码 | 含义 | HTTP 状态 |
|---|---|---|
| UNAUTHORIZED | 未认证 | 401 |
| FORBIDDEN | 无权限 | 403 |
| NOT_FOUND | 资源不存在 | 404 |
| CONFLICT | 资源冲突 | 409 |
| VALIDATION_ERROR | 参数校验失败 | 422 |
| RATE_LIMITED | 限流 | 429 |
| INTERNAL_ERROR | 内部错误 | 500 |

### 旧端点兼容

旧 `server.rs` 中的 4 个端点（`GET /admin/pools`、`GET /admin/fuse`、`GET /admin/health`、`GET /admin/nodes`）在新 `admin/` 模块部署后继续可用，但：
- 旧端点返回原有格式（无统一 `{success, data, error, meta}` 包装）
- 新端点使用统一格式
- 过渡期后（Phase 3 完成），旧端点标记为 DEPRECATED
- 迁移方案：客户端统一指向新端点地址后，移除旧 handler

---

## 3. 分页

### Cursor-based（推荐）

```
GET /admin/requests?cursor=req_100&limit=50
```

返回 `meta.next_cursor` + `meta.has_more`。

### Offset-based（兼容）

```
GET /admin/requests?offset=0&limit=50
```

### 导出限制

`GET /admin/requests/export` 支持数据导出，但有以下限制：
- 默认最大返回 10,000 条（通过 `?limit=` 可调，上限 100,000）
- 超出上限时返回 413 Payload Too Large，建议分批 + `?since=` / `?until=` 按时间窗口导出
- 导出格式为 JSONL（每行一条 JSON 记录）

---

## 4. 时间范围

| 参数 | 格式 | 示例 |
|---|---|---|
| from | ISO 8601 | `2026-05-19T00:00:00Z` |
| to | ISO 8601 | `2026-05-19T23:59:59Z` |
| window | 时长 | `5m`, `1h`, `24h` |

---

## 5. 当前实现状态

| 组件 | 当前状态 | v3.4 行动 |
|---|---|---|
| 认证 | `x-api-key` 内联函数 | 增加 `Authorization: Bearer` + AuthGate 抽象 |
| 中间件 | 无 | 增加 Tower 中间件或 Axum 提取器 |
| 响应格式 | 无统一包装 | 增加 `ApiResponse<T>` / `ApiError` |
| 审计 | 无 | 记录到 EventStore |
| 分页 | 无 | 增加 cursor 分页 |
