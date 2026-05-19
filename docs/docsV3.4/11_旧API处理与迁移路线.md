# ZenProxyRS_M v3.4 旧 API 处理与迁移路线

## 1. 保留（不做修改）

| 路由 | 说明 |
|---|---|
| GET / | 服务信息 |
| GET /health | 公开健康检查 |
| GET /metrics | Prometheus |
| GET /v1/models | 模型列表 |
| ANY /v1/{*path} | 核心代理入口 |
| GET /admin/pools | 池状态 |
| GET /admin/health | 管理健康详情 |

---

## 2. 改造

### GET /admin/fuse

| 当前 | v3.4 |
|---|---|
| GET 单一 scope | GET + POST 多 scope |
| AtomicBool | FuseManager + FuseState |

向后兼容：默认返回 global scope 状态，与旧响应兼容。

### GET /admin/nodes

| 当前 | v3.4 |
|---|---|
| 只返回账本摘要 | 完整节点信息 |
| 仅 GET | 全 CRUD（POST/PATCH/DELETE）|

向后兼容：`GET /admin/nodes` 保持返回节点列表。

---

## 3. 新增

| 路由 | 说明 |
|---|---|
| /admin/models/* | CRUD + probe |
| /admin/providers/* | CRUD + route switch + probe |
| /admin/routes/* | CRUD + operate |
| /admin/requests/* | 查询 + 详情 + 聚合 |
| /admin/events/* | 查询 + 流 |
| /admin/health/live | 存活探针 |
| /admin/health/ready | 就绪探针 |
| /admin/metrics/summary | 指标摘要 |
| POST /admin/config:reload | 热加载触发 |

---

## 4. 废弃

### /admin/stats

当前不存在于代码中（已从 v2.0 移除）。不要恢复。

### /admin/models 旧语义

当前不存在。v3.4 新增 `/admin/models` 使用新语义。

---

## 5. 迁移策略

| 阶段 | 动作 | 兼容性 |
|---|---|---|
| Phase 2 | 先新增 /admin/requests + /admin/events | 不破坏旧 API |
| Phase 3 | 扩展 fuse GET → 增加 POST | GET 保持兼容 |
| Phase 4 | 新增 nodes CRUD | GET 保持兼容 |
| Phase 5 | 新增 routes/providers/models | 全新增 |
| Phase 6 | 如需废弃旧 API，返回 301 | 至少保持 1 个版本 |

---

## 6. API 覆盖范围分析

### 当前 v3.0 真实 API 清单

代码中注册了 7 个路由（main.rs:187-196），全部只读：

| # | 路由 | 方法 | 认证 | 功能 |
|---|---|---|---|---|
| 1 | `/` | GET | 无 | 服务信息 |
| 2 | `/health` | GET | 无 | 公共健康检查 |
| 3 | `/metrics` | GET | 无 | Prometheus 指标 |
| 4 | `/v1/models` | GET | 无 | 模型列表（硬编码 2 个）|
| 5 | `/v1/{*path}` | ANY | 无 | **核心代理入口** |
| 6 | `/admin/pools` | GET | x-api-key | 池统计 5 个数值 |
| 7 | `/admin/fuse` | GET | x-api-key | 熔断状态 true/false |
| 8 | `/admin/health` | GET | x-api-key | 详细健康含 uptime |
| 9 | `/admin/nodes` | GET | x-api-key | 账本摘要 |

### v3.4 目标 API 清单

| # | 路由 | 方法 | GET/POST | 资源 |
|---|---|---|---|---|
| 1-9 | 上面所有 | 保持 | 保持 | 兼容 |
| 10 | `/admin/health/live` | GET | 读 | 存活探针 |
| 11 | `/admin/health/ready` | GET | 读 | 就绪探针 |
| 12 | `/admin/metrics/summary` | GET | 读 | 指标聚合 |
| 13 | `/admin/requests` | GET | 读 | 请求记录查询 |
| 14 | `/admin/requests/{id}` | GET | 读 | 请求详情 |
| 15 | `/admin/requests/summary` | GET | 读 | 聚合统计 P1 |
| 16 | `/admin/events` | GET | 读 | 事件查询 |
| 17 | `/admin/events/stream` | GET | 读 | 事件流 P1 |
| 18 | `/admin/models` | GET | 读 | 模型列表 |
| 19 | `/admin/models/{id}` | GET | 读 | 模型详情 |
| 20 | `/admin/providers` | GET | 读 | Provider 列表 |
| 21 | `/admin/providers/{id}` | GET | 读 | Provider 详情 |
| 22 | `/admin/routes` | GET | 读 | Route 列表 |
| 23 | `/admin/routes/{id}` | GET | 读 | Route 详情 |
| 24 | `/admin/nodes/{id}` | GET | 读 | 节点详情 |
| 25 | `/admin/nodes/{id}/history` | GET | 读 | 节点请求历史 Phase 1a |
| 26 | `/admin/nodes/{id}/lifetime` | GET | 读 | 节点生命周期 Phase 1a |
| 27 | `/admin/fuse` | GET | 读 | 多 scope 熔断 |
| 28 | `/admin/fuse/policies` | GET | 读 | 熔断策略 P1 |

| # | 路由 | 方法 | 控制操作 |
|---|---|---|---|
| 29 | `POST /admin/config:reload` | POST | **配置热加载** |
| 30 | `POST /admin/models` | POST | 新增模型 |
| 31 | `PATCH /admin/models/{id}` | PATCH | 修改模型 |
| 32 | `DELETE /admin/models/{id}` | DELETE | 删除模型 |
| 33 | `POST /admin/models/probe` | POST | **触发模型探测** |
| 34 | `POST /admin/providers` | POST | 新增 Provider |
| 35 | `PATCH /admin/providers/{id}` | PATCH | 修改 Provider |
| 36 | `DELETE /admin/providers/{id}` | DELETE | 删除 Provider |
| 37 | `POST /admin/providers/{id}/route:switch` | POST | **切换 Provider 链路** |
| 38 | `POST /admin/providers/{id}/probe` | POST | **触发 Provider 探测** |
| 39 | `POST /admin/nodes` | POST | 新增节点 |
| 40 | `PATCH /admin/nodes/{id}` | PATCH | 修改节点 |
| 41 | `DELETE /admin/nodes/{id}` | DELETE | 删除节点 |
| 42 | `POST /admin/nodes/{id}:enable` | POST | **启用节点** |
| 43 | `POST /admin/nodes/{id}:disable` | POST | **禁用节点** |
| 44 | `POST /admin/nodes/{id}:probe` | POST | **触发节点探活** |
| 45 | `POST /admin/nodes/{id}:reset-health` | POST | **重置节点健康** |
| 46 | `POST /admin/routes` | POST | 新增 Route |
| 47 | `PATCH /admin/routes/{id}` | PATCH | 修改 Route |
| 48 | `DELETE /admin/routes/{id}` | DELETE | 删除 Route |
| 49 | `POST /admin/routes/{id}:enable` | POST | **启用 Route** |
| 50 | `POST /admin/routes/{id}:disable` | POST | **禁用 Route** |
| 51 | `POST /admin/routes/{id}:probe` | POST | **探测 Route** |
| 52 | `POST /admin/fuse/{s}/{e}:open` | POST | **打开熔断** |
| 53 | `POST /admin/fuse/{s}/{e}:close` | POST | **关闭熔断** |
| 54 | `POST /admin/fuse/{s}/{e}:reset` | POST | **重置熔断** |

### 覆盖缺口量化

| 维度 | v3.0 | v3.4 目标 | 覆盖率 |
|---|---|---|---|
| 路由总数 | 9 | **~55** | **16%** |
| GET 只读端点 | 9 | 28 | 32% |
| POST/PATCH/DELETE 控制端点 | **0** | **~26** | **0%** |
| 认证机制 | 简陋内联 | AuthGate 中间件 | 0% |
| 统一响应格式 | 无 | `ApiResponse<T>` | 0% |
| 审计 | 无 | EventStore 记录 | 0% |
| 分页 | 无 | cursor 分页 | 0% |

### 按 Phase 的 API 落地顺序

```
Phase 1a (节点可观测)
  └─ GET /admin/nodes/{id}/history     ← 读（依赖 NodeRequestHistory 新增）
  └─ GET /admin/nodes/{id}/lifetime    ← 读（依赖 PoolTransitionLog 修复）
  └─ GET /admin/events?source=pool     ← 读（依赖 record_pool 修复）
  └─ GET /admin/events?source=probe    ← 读（依赖 record_probe 修复）

Phase 2 (补查询)
  └─ GET /admin/requests               ← 读（依赖 RequestTelemetry 字段补全）
  └─ GET /admin/requests/{id}          ← 读
  └─ GET /admin/requests/summary       ← 读 P1
  └─ GET /admin/events                 ← 读
  └─ GET /admin/events/stream          ← 读 P1

Phase 3 (第一个控制 API)
  └─ POST /admin/fuse/...:open         ← **控制** 依赖 FuseManager
  └─ POST /admin/fuse/...:close        ← **控制**
  └─ POST /admin/fuse/...:reset        ← **控制**

Phase 4 (CRUD API)
  └─ /admin/models/*                   ← 读 + 控制
  └─ /admin/providers/*                ← 读 + 控制
  └─ /admin/nodes/* (CRUD)             ← 读 + **控制**
  └─ /admin/routes/*                   ← 读 + **控制**

Phase 8 (运维)
  └─ POST /admin/config:reload         ← **控制** 依赖 SIGHUP 修复
```

### 关键发现

1. **第一个能 control 的 API 在 Phase 3 才出现**（fuse 开关熔断）。在此之前全部是只读查询。

2. **当前认证有严重 bug：** `check_admin_auth()` 在 `admin_api_key` 为 None 时返回 false，意味着**没配 ADMIN_API_KEY 时所有 Admin API 返回 401**。生产环境就是这种情况。

3. **数据有了不等于 API 有了：** RingBuffer 有 10k 条请求记录，RollingAggregator 有聚合指标，但没有一个查询端点能读它们。

4. **"/admin/nodes" 当前返回的是账本摘要不是节点列表：** `GET /admin/nodes` 返回的是 `ledger.summary()`（按 node 聚合的计数），不是真正的节点清单。没有 `NodeRegistry` 的情况下，Admin API 无法返回节点列表。

5. **19 个控制操作（POST/PATCH/DELETE）全部需要新基础设施：** 没有 ProviderRegistry 就不能新增 Provider，没有 RouteRegistry 就不能创建 Route，没有 FuseManager 就没法开关 scope 级熔断。API 是 Registries 的外壳。
