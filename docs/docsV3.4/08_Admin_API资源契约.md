# ZenProxyRS_M v3.4 Admin API 资源契约

## 1. Models API

### GET /admin/models

查询模型清单。Query: `provider`, `enabled`, `freeOnly`, `q`, `capability`

### POST /admin/models

手动添加模型（provider_id, model_id, display_name, enabled, is_free, capabilities）。

### PATCH /admin/models/{provider_id}/{model_id}

修改：`enabled`, `display_name`, `is_free`, `capabilities`, `metadata`

### DELETE /admin/models/{provider_id}/{model_id}

默认软删除（disable），`?purge=true` 强制删除。

### POST /admin/models/probe

手动触发探测。Body: `provider_id`, `strategy`(list/sample_call/both), `candidate_models`, `timeout_ms`

**当前基线：** ❌ 全部不存在

**数据来源：** Admin API 是唯一权威写路径。详见总体契约第 0 章。

---

## 2. Providers API

### GET /admin/providers

返回：provider_id, kind, enabled, base_url, active_route_id, health, model_count

### POST /admin/providers

新增 provider（kind, base_url, api_key_ref, enabled, default_model, timeout_ms）。

### PATCH /admin/providers/{provider_id}

修改：enabled, base_url, default_model, timeout_ms, headers_policy

### DELETE /admin/providers/{provider_id}

有引用时需 `?force=true`。

### POST /admin/providers/{provider_id}/route:switch

切换链路。Body: `route_id`, `reason`

### POST /admin/providers/{provider_id}/probe

检测 base_url 可达性、认证有效性、/v1/models 可用性。

**当前基线：** ❌ 全部不存在

**数据来源：** Admin API 是唯一权威写路径。详见总体契约第 0 章。

---

## 3. Nodes API

### GET /admin/nodes

Query: `status`, `kind`, `region`, `enabled`, `tag`

返回节点详情含健康、延迟、出口 IP、活跃路由数。

### GET /admin/nodes/{node_id}

返回更多详情（配置脱敏、近期错误、健康历史、路由绑定）。

### POST /admin/nodes

新增节点（name, kind, endpoint, enabled, region, tags, limits）。

### PATCH/DELETE /admin/nodes/{node_id}

修改或删除（默认 disable，`?purge=true` 强制）。

### Node 操作

```
POST /admin/nodes/{node_id}:enable
POST /admin/nodes/{node_id}:disable
POST /admin/nodes/{node_id}:probe
POST /admin/nodes/{node_id}:reset-health
```

**当前基线：** ✅ `GET /admin/nodes` 已实现（含 `?model=`），❌ 其余不存在

**数据来源：** Admin API 是唯一权威写路径。详见总体契约第 0 章。当前 `GET /admin/nodes` 数据来自 LedgerCounters，与 JSON 配置文件无关。

---

## 4. Routes API

Route 是 v3.4 的关键新概念。

### GET /admin/routes

Query: `provider`, `node`, `enabled`, `status`

返回 route 详情含 provider/node 绑定和健康统计。

### POST /admin/routes

新增 route（provider_id, node_id, enabled, priority, weight）。

### Route 操作

```
PATCH /admin/routes/{route_id}
DELETE /admin/routes/{route_id}
POST /admin/routes/{route_id}:enable
POST /admin/routes/{route_id}:disable
POST /admin/routes/{route_id}:probe
```

**当前基线：** ❌ 全部不存在

**数据来源：** Admin API 是唯一权威写路径。详见总体契约第 0 章。

---

## 5. Requests API

P0 必须补的核心 API。数据已存在（LedgerCounters + DataCollector），只缺查询接口。

### GET /admin/requests

Query: `from`, `to`, `provider`, `model`, `node`, `route`, `status`, `http_status`, `stream`, `client_id`, `limit`, `cursor`

返回请求记录全字段（request_id, timestamp, model, node, latency, tokens, bytes, status 等）。

### GET /admin/requests/{request_id}

默认脱敏。Query: `include_payload`, `redacted`

### GET /admin/requests/summary（P1）

Query: `window`, `group_by`

**当前基线：** ✅ 数据源存在，❌ 查询端点不存在

**导出限制：** `GET /admin/requests/export` 上限 100,000 条，详见总体契约"导出限制"章节。

---

## 6. Events API

### GET /admin/events

Query: `from`, `to`, `type`, `source`, `level`, `entity_id`, `limit`, `cursor`

返回事件记录（event_id, timestamp, level, source, event_type, entity_id, message）。

### GET /admin/events/stream（P1）

使用 SSE。

**注：** 此端点为 P1，列入后续升级任务，不在 v3.4 Phase 0-3 范围内。Phase 2 先实现 REST 轮询版本。

**当前基线：** ✅ 数据源存在，❌ 查询端点不存在

---

## 7. Fuse API

### 当前（v3.0）：`GET /admin/fuse` 单一 scope

### v3.4 目标

### GET /admin/fuse

Query: `scope`, `entity_id`。返回多 scope 熔断状态。

### POST /admin/fuse/{scope}/{entity_id}:open

Body: `reason`, `ttl_seconds`

### POST /admin/fuse/{scope}/{entity_id}:close

### POST /admin/fuse/{scope}/{entity_id}:reset

scope: `global` / `provider` / `node` / `route` / `model`

### GET /admin/fuse/policies（P1）
### PATCH /admin/fuse/policies/{policy_id}（P1）

**当前基线：** ✅ `GET /admin/fuse` 已实现，❌ POST 操作不存在

---

## 8. Health / Metrics API

### GET /admin/health

返回完整内部状态（provider 健康、节点统计、路由、熔断、存储用量等）。

### GET /admin/health/live

容器存活探针（200）。

### GET /admin/health/ready

容器就绪探针（上游可达 + 健康节点）。

### GET /metrics

Prometheus 格式（已实现）。

### GET /admin/metrics/summary（P1）

Query: `window`, `group_by`

返回：requests, success/error_rate, p50/p95/p99 latency, tokens, bytes, tokens_per_kb

**当前基线：** ✅ `GET /admin/health` 和 `GET /metrics` 已实现，❌ live/ready/summary 不存在

---

## 9. Config API

### POST /admin/config:reload

触发配置热加载（SIGHUP 已实现但无 API 端点）。

---

## 10. 完整端点清单与性能评估

### 10.1 端点总览

共 **57 个端点**，分 6 组：

| 组 | 数量 | 读(READ) | 控制(CONTROL) | 依赖 |
|---|---|---|---|---|
| 系统健康与状态 | 8 | 8 | 0 | 无 |
| 池与节点操作 | 8 | 2 | 6 | 基础设施改造 |
| 请求记录 | 7 | 7 | 0 | DataCollector 查询方法 |
| 事件 | 4 | 3 | 1 | record_probe 修复 |
| 账本统计 | 6 | 6 | 0 | Ledger 访问器 |
| 配置与系统 | 8 | 5 | 3 | Config RwLock + SIGHUP 修复 |

### 10.2 各端点性能评估

以下评估基于 107 节点、10000 条 RingBuffer、10000 请求/小时的生产负载。

#### 第一组：系统健康与状态

| 路径 | 数据源 | 时间复杂度 | 延迟预估 | 资源开销说明 |
|---|---|---|---|---|
| `GET /admin/health` | PoolStats + UpstreamHealth + 计数器 | O(1) | < 1ms | 读 AtomicUsize，无锁，零内存分配 |
| `GET /admin/health/live` | 常量 200 | O(1) | < 0.01ms | 无数据访问，纯响应构造 |
| `GET /admin/health/ready` | PoolStats + UpstreamHealth | O(1) | < 1ms | 同上 |
| `GET /admin/stats` | DataSnapshot + LedgerSummary | O(1) | < 2ms | 5 个 AtomicUsize 读 + Ledger 3 个读锁 |
| `GET /admin/stats/models` | LedgerCounters.by_model | O(N_m) N_m≈10 | < 1ms | 读锁 + JSON 序列化 |
| `GET /admin/stats/nodes` | LedgerCounters.by_node | O(N_n) N_n=107 | < 2ms | 读锁 + 107 行 JSON |
| `GET /admin/stats/pools` | PoolDimensionStats | O(1) | < 1ms | 读锁 |
| `GET /admin/stats/upstream` | UpstreamHealth + Global429Detector | O(W) W=1000 | < 3ms | 需遍历 1000 条时间窗口统计 429 率 |

**批量调用建议：** `/admin/stats` 一次返回所有维度，客户端应优先使用聚合端点多而非逐个调子端点。

#### 第二组：池与节点操作

| 路径 | 时间复杂度 | 延迟预估 | 资源开销说明 |
|---|---|---|---|
| `GET /admin/pools` | O(1) | < 1ms | 4 个 pool.available() 调用，无锁 |
| `GET /admin/pools/{name}` | O(N_pool) | < 2ms | 遍历池内所有节点，序列化评分 |
| `POST /admin/fuse` | O(N) N=107 | < 1ms | AtomicBool store + 遍历 nodes HashMap 移池 |
| `POST /admin/nodes` | O(1) | < 1ms | HashMap insert，无 I/O |
| `DELETE /admin/nodes/{id}` | O(1)x4 | < 2ms | 4 个 HashMap remove，4 个读锁 |
| `POST /admin/nodes/{id}/probe` | O(1) + HTTP | **300ms-30s** | 内部发起 HTTP POST 到上游，受 upstream 响应速度限制 |
| `POST /admin/nodes/{id}/recover` | O(1) | < 1ms | 写锁 + HashMap insert/remove |

**关键风险：** `/admin/nodes/{id}/probe` 是 HTTP 调用，可能阻塞 30 秒。必须用 `tokio::spawn` 后台运行 + 轮询结果，或 WebSocket 返回。**绝不能直接在 handler 里 await probe 结果。**

#### 第三组：请求记录

| 路径 | 时间复杂度 | 延迟预估 | 资源开销说明 |
|---|---|---|---|
| `GET /admin/requests` | O(N) N=10000 | **3-8ms** | 遍历 RingBuffer 10k 条 + 内存过滤 + JSON |
| `GET /admin/requests/{rid}` | O(N) N=10000 | **3-5ms** | 最坏情况扫描全部 10k 条匹配 rid |
| `GET /admin/requests/summary` | O(W*D) W=12 D=4 | < 2ms | RollingAggregator JSON 序列化 |
| `GET /admin/requests/recent` | O(limit) | < 1ms | RingBuffer 头指针直接读 |
| `GET /admin/requests/export` | O(N) + I/O | **5-50ms** | JSONL 流式输出，含文件 I/O |
| `GET /admin/requests/models` | O(W*N_m) | < 2ms | aggregator 按 model 汇总 |
| `GET /admin/requests/nodes` | O(W*N_n) | < 3ms | aggregator 按 node 汇总 |

**RingBuffer 扫描性能：** 10k 条以内全扫 < 10ms。如果未来 RingBuffer 扩容到 100k，需加倒排索引（按 model/node_url 预分组），否则每次查询都扫 100k 条会到 50-100ms。

#### 第四组：事件

| 路径 | 时间复杂度 | 延迟预估 | 资源开销说明 |
|---|---|---|---|
| `GET /admin/events` | O(N_event) | < 2ms | VecDeque 遍历 + 过滤 |
| `GET /admin/events/recent` | O(limit) | < 1ms | VecDeque 尾读取 |
| `GET /admin/events/probes` | O(N_event) | < 2ms | 按 source=probe 过滤 |
| `POST /admin/probe/now` | O(1) + HTTP | **300ms-30s** | 同 node probe，必须异步 |

#### 第五组：账本统计

| 路径 | 复杂度 | 延迟 | 说明 |
|---|---|---|---|
| `GET /admin/ledger` | O(N_n+N_m) | < 3ms | 聚合所有维度 |
| `GET /admin/ledger/nodes` | O(N_n) N_n=107 | < 1ms | 已有 |
| `GET /admin/ledger/models` | O(N_m) N_m≈10 | < 1ms | 新增 |
| `GET /admin/ledger/keys` | O(N_k) N_k≈1-5 | < 1ms | 新增 |
| `GET /admin/ledger/streams` | O(2) | < 1ms | 新增 |

#### 第六组：配置与系统

| 路径 | 复杂度 | 延迟 | 说明 |
|---|---|---|---|
| `GET /admin/config` | O(1) | < 1ms | 读锁 + JSON 序列化 |
| `POST /admin/config/reload` | O(1) + I/O | **5-20ms** | 读环境变量 + Config 全量解析 + 写锁替换 |
| `GET /admin/config/validation` | O(1) | < 5ms | 校验所有配置项的合法性 |
| `GET /admin/system/uptime` | O(1) | < 0.1ms | Instant::elapsed |
| `POST /admin/system/log-level` | O(1) | < 0.1ms | tracing::filter 动态更新 |
| `GET /admin/system/info` | O(1) + O(N) | < 5ms | 聚合全部健康/状态/配置 |

### 10.3 性能汇总

| 指标 | 值 |
|---|---|
| 总端点数 | 57 |
| 读端点（GET） | 44 |
| 控制端点（POST/DELETE） | 13 |
| 快速端点（< 5ms） | **47 个（82%）** |
| 中等端点（5-50ms） | **5 个（9%）** |
| 慢端点（> 300ms，含 HTTP） | **5 个（9%）**——全是 probe 操作 |
| 单次 admin 请求平均服务端 CPU 开销 | < 2ms（不含 probe HTTP 耗时）|
| 单次 admin 请求最大内存分配 | ~50KB（requests 端点 JSON 序列化）|
| 同时并发 admin 请求推荐上限 | **50**（超过后 RwLock 竞争加剧）|
| 对核心代理路径的性能影响 | **零**（admin 端点是旁路，不接触 proxy_handler 路径）|

### 10.4 热点与风险

1. **RingBuffer 全扫（3-8ms）**——所有 requests 查询端点的瓶颈。10k 条目以内可以接受。扩容到 100k+ 需要加倒排索引。

2. **AdminService 认证检查**——每次请求多一次 `x-api-key` 字符串比较。开销 < 1μs，可忽略。

3. **`POST /admin/nodes/{id}/probe` 必须异步**——handler 里 `await` probe HTTP 请求会阻塞 axum worker 线程 30 秒。必须用 `tokio::spawn` + 返回 `{probe_id}`，客户端轮询结果。

4. **`POST /admin/config/reload` 写锁**——Config RwLock 写锁期间，所有读配置的 proxy 路径会短暂阻塞（< 20ms）。高峰期慎用。

5. **WAL 激活后的 I/O 开销**——每条请求追加一行 JSONL。磁盘 I/O 异步，不会阻塞请求路径。但批量调 admin/events 读 WAL 文件时，`replay()` 全读进内存可能耗 10-50ms。
