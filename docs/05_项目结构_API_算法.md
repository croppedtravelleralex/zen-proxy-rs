# 05 | 项目结构 API 算法

> 榫卯架构版 v3.0 | 2026-05-17

---

## 一、项目结构

### 1.1 源码结构 (v3.0 榫卯版)

```
zen-proxy-rs/
├── Cargo.toml                  # 依赖声明 (约20个 crate, 删除了未使用的依赖)
├── Cargo.lock
├── nodes.json                  # 100 个 WebShare 节点
│
├── src/
│   ├── main.rs                 # 榫卯组装车间 (~80行)
│   ├── config.rs               # 配置 (EnvConfigProvider + SIGHUP 热加载)
│   ├── server.rs               # HTTP 服务 (axum 路由 + 管理端点)
│   ├── proxy.rs                # 核心代理 (~200行)
│   │   ├── proxy_handler (泛型, 认 trait 不认具体类型)
│   │   ├── proxy_with_retry
│   │   ├── read_full_body + stream_to_axum
│   │   └── 只调 PoolManager + DataCollector trait
│   │
│   ├── pool/                   # 六池模块组
│   │   ├── mod.rs              # trait Pool, PoolManager
│   │   ├── manager.rs          # PoolManagerImpl (紧耦合状态机)
│   │   ├── dispatch.rs         # DispatchPool (加权随机 + node_provider + client缓存)
│   │   ├── active.rs           # ActivePool (引用计数 + max_concurrent)
│   │   ├── ratelimited.rs      # RateLimitedPool (429隔离 + 探活)
│   │   ├── dead.rs             # DeadPool (连续失败 + 每日探活)
│   │   └── probe_period.rs     # 探活期异步任务 (非独立池)
│   │
│   ├── collector/              # 数据采集器 (含脱敏/归一化)
│   │   ├── mod.rs              # trait DataCollector, StorageBackend
│   │   ├── telemetry.rs        # RequestTelemetry (22字段)
│   │   ├── ring_buffer.rs      # RingBuffer<T> + WAL 追加写 + 崩溃回放
│   │   ├── aggregator.rs       # RollingAggregator (5min窗口, 快照恢复)
│   │   ├── wal.rs              # WAL 管理 (归档/滚动/清理)
│   │   ├── default.rs          # DefaultCollector (连接 ring_buffer+aggregator+export)
│   │   └── export.rs           # JsonBackend, PrometheusBackend, MultiBackend
│   │
│   ├── provider/               # 节点提供商
│   │   ├── mod.rs              # trait NodeProvider, NodeIdentity
│   │   └── webshare.rs         # WebShareProvider
│   │
│   ├── health.rs               # UpstreamHealth (仅监控展示)
│   └── utils.rs                # 工具函数 (smart_backoff 等)
│
└── (已删除文件)
    ├── token_bucket.rs     -> 由 ActivePool::max_concurrent 替代
    ├── selector.rs         -> 拆分为 pool/dispatch.rs + pool/active.rs
    ├── pool.rs             -> pool/dispatch.rs 内建 client 缓存
    ├── node_probe.rs       -> pool/probe_period.rs
    ├── node_db.rs          -> 并入 collector/
    ├── metrics.rs          -> 并入 collector/
    ├── bandwidth.rs        -> 并入 collector/
    ├── admin.rs            -> server.rs
    ├── state.rs            -> 不再需要上帝对象
    └── proxy_tail.rs       -> 死代码文件
```

### 1.2 文件职责矩阵

```
文件                    接口层(trait)                     实现层(impl)                 对其他模块暴露
────                   ────────────                     ────────────                 ──────────────
pool/mod.rs            Pool, PoolManager, FuseController —                            给 proxy/server/probe
pool/manager.rs          —                              PoolManagerImpl              仅通过 trait
pool/dispatch.rs         —                              DispatchPool                 仅 PoolManager
pool/active.rs           —                              ActivePool                   仅 PoolManager
pool/ratelimited.rs      —                              RateLimitedPool              仅 PoolManager
pool/dead.rs             —                              DeadPool                     仅 PoolManager
pool/session.rs        ClientFactory, ClientCache        ReqwestClientFactory         仅 DispatchPool
collector/mod.rs          DataCollector, StorageBackend    —                            给 proxy.rs + 各 pool 调
collector/default.rs      —                              DefaultCollector             仅通过 trait
collector/export.rs       —                              Json/Prometheus/MultiBackend 仅 DefaultCollector
provider/mod.rs           NodeProvider                   —                            给 DispatchPool
provider/webshare.rs      —                              WebShareProvider             仅 main.rs 实例化
health.rs                 —                              UpstreamHealth               保留原文件
server.rs                 —                              axum 路由 + 管理端点         给 main.rs
proxy.rs                  —                              proxy_handler               认 trait 不认具体
```

### 1.3 文件行数预估 (v3.0)

```
文件                    预估行数          对比旧版(v2.0)
────                    ──────           ────────────
main.rs                 ~80              +3 (更简洁)
config.rs               ~100             -41 (删减)
server.rs               ~120             -12
proxy.rs                ~200             -20 (更清晰)
pool/mod.rs             ~60              新增
pool/manager.rs         ~150             新增
pool/dispatch.rs        ~200             新增
pool/active.rs          ~120             新增
pool/ratelimited.rs     ~100             新增
pool/dead.rs            ~80              新增
pool/probe_period.rs    ~60              新增
collector/mod.rs        ~40              新增
collector/telemetry.rs  ~50              新增 (22字段数据模型)
collector/ring_buffer.rs ~200            新增 (环形缓冲区 + WAL + 回放)
collector/aggregator.rs ~250            新增 (滚动窗口聚合器 + 快照)
collector/wal.rs        ~100            新增 (WAL 归档/滚动/清理)
collector/default.rs    ~200            新增 (连接各组件)
collector/export.rs     ~100            新增 (Prometheus 标签化导出)
provider/mod.rs         ~30              新增
provider/webshare.rs    ~40              新增
health.rs               ~250             -61
utils.rs                ~110             不变
──────────────────────────────────
合计                    ~3,050           -143 (精简4%) 
注: 比初版预增 ~1,080 行, 主要是 collector/ 5 个文件的完整实现 (ring_buffer + aggregator + WAL + telemetry + 升级版 export)
实际运行内存: ~2.3MB (与原 v2.0 比增加 ~200KB, 其中环形缓冲区 ~2MB 固定预分配)
```

---

## 二、API 设计

### 2.1 公开端点

`05_项目结构_API_算法.md` 是 API 唯一事实源。其他文档若出现旧端点，以本节为准。

```
路径                    方法    功能                            认证
────                    ────    ────                            ────
/                       GET     服务信息 (版本/状态)              无
/health                 GET     公开健康检查 (ok/degraded only)   无
/metrics                GET     Prometheus 标签化指标 (~50 families) 无
/v1/models              GET     模型列表                         无
/v1/{*path}             ANY     核心代理入口                      透传

/admin/health           GET     管理健康详情 (上游/全局429/熔断)    x-api-key
/admin/pools            GET     人类可读池状态 (大小/健康度/恢复批次) x-api-key
/admin/fuse             POST    手动熔断/分批恢复                  x-api-key
/admin/requests         GET     通用查询: 请求明细 + 聚合统计       x-api-key
/admin/events           GET     节点事件流 (探活/降级/恢复/熔断)    x-api-key
```

端点保留规则:
- `/admin/pools` **保留**，用于人工排障；`/metrics` 用于 Prometheus，不替代人工 JSON。
- `/admin/stats` 废弃，由 `/admin/requests?group=...&aggregate=...` 替代。
- `/admin/models` 废弃，由 `/v1/models` 或 `/admin/requests?group=model` 替代。
- `/health` 只给外部探活，详细内部状态必须走 `/admin/health`。

### 2.2 端点详细规格

#### `GET /metrics` (升级版, 取代旧 16 个裸键值对)

```
输出: Prometheus 文本格式, ~50 个 metric families, 带 TYPE/HELP/标签

安全规则:
  - 禁止把完整 SOCKS5 URL 放入 label。
  - 禁止把代理账号、密码、Bearer token、client 原文写入 label。
  - node 统一使用 provider + node_id，例如 provider="webshare", node_id="ws-001"。
  - model 进入 label 前必须由 Sanitizer.normalize_model_label() 白名单/归一化。
  - /metrics 不暴露 client_id，按 client 查询只能走 /admin/requests。

# HELP zen_proxy_requests_total Total requests by model, status, pool
# TYPE zen_proxy_requests_total counter
zen_proxy_requests_total{model="deepseek-v4",status="200",pool="dispatch"} 800
zen_proxy_requests_total{model="deepseek-v4",status="429",pool="ratelimited"} 50

# HELP zen_proxy_tokens_total Token consumption by model and type
# TYPE zen_proxy_tokens_total counter
zen_proxy_tokens_total{model="deepseek-v4",type="prompt"} 120000
zen_proxy_tokens_total{model="deepseek-v4",type="completion"} 48000

# HELP zen_proxy_node_latency Node latency percentile
# TYPE zen_proxy_node_latency gauge
zen_proxy_node_latency{provider="webshare",node_id="ws-001",quantile="p50"} 320
zen_proxy_node_latency{provider="webshare",node_id="ws-001",quantile="p95"} 2100

# HELP zen_proxy_node_concurrent Current concurrent requests per node
# TYPE zen_proxy_node_concurrent gauge
zen_proxy_node_concurrent{provider="webshare",node_id="ws-001"} 3

# HELP zen_proxy_active_pool_size Active 5-pool sizes
# TYPE zen_proxy_active_pool_size gauge
zen_proxy_active_pool_size{pool="dispatch"} 85
zen_proxy_active_pool_size{pool="active"} 3
zen_proxy_active_pool_size{pool="ratelimited"} 7
zen_proxy_active_pool_size{pool="dead"} 5
zen_proxy_active_pool_size{pool="probe_period"} 2

# HELP zen_proxy_global_rate_limit Global upstream rate limit state
# TYPE zen_proxy_global_rate_limit gauge
zen_proxy_global_rate_limit{state="normal"} 0
zen_proxy_global_rate_limit{state="suspected"} 1
zen_proxy_global_rate_limit{state="confirmed"} 0

数据来源: DefaultCollector 全局原子计数器 + RollingAggregator 当前窗口 + UpstreamHealth
```

#### `GET /admin/requests` — 通用查询 (消费层 + 排查层)

```
查询参数:
  ?rid=zen-12f              精确请求 ID 查询 (不走扫描, O(1))

  # 排查类查询 (原始明细, 走环形缓冲区扫描)
  ?since=5m                 时间范围 (ISO8601 或 "5m"/"1h"/"24h")
  &until=2026-05-17T14:30:00Z
  &status=429               按状态码过滤
  &model=deepseek-v4        按模型过滤 (先归一化)
  &node_id=ws-001           按脱敏节点 ID 过滤
  &provider=webshare        按节点提供商过滤
  &pool=ratelimited         按池过滤
  &client_id=sha256:8f3a91c2 按客户端 token hash 前缀过滤
  &limit=50                 返回条数 (默认 100, 最大 1000, 由 QueryLimiter 裁剪)
  &cursor=eyJzZXEiOjEyM30   游标分页: base64url({"seq":123})
  &sort=latency&order=desc  排序 (ts / latency / tokens)

  # 消费类查询 (聚合统计, 走预计算聚合器 O(1))
  ?since=24h&until=now
  &group=model              分组维度 (model/node/pool/client_id/status)
  &aggregate=tokens,bytes   聚合指标 (tokens/bytes/count/latency_avg)
  &group=model,pool         多维组合 (走环形缓冲区扫描, ~5ms)

响应 (原始明细):
{
  "total": 42,
  "truncated": false,
  "cursor": "zen-13a",
  "requests": [
    {
      "rid": "zen-12f",
      "ts": "2026-05-17T14:30:01.123Z",
      "model": "deepseek-v4-flash-free",
      "client_id": "sk-xxx",
      "node_id": "ws-001",
      "provider": "webshare",
      "node_label": "webshare/ws-001",
      "pool": "ratelimited",
      "exit_ip": "45.39.73.11",
      "status": 429,
      "is_streaming": false,
      "retry_count": 3,
      "latency_total_ms": 3204,
      "upstream_ms": 3100,
      "ttft_ms": null,
      "prompt_tokens": 150,
      "completion_tokens": 0,
      "total_tokens": 150,
      "bytes_sent": 420,
      "bytes_received": 280,
      "tokens_per_kb": 548.6,
      "rate_limited": true
    }
  ]
}

响应 (聚合):
{
  "since": "2026-05-17T13:30:00Z",
  "until": "2026-05-17T14:30:00Z",
  "windows": [
    {
      "window_start": "13:30",
      "window_end": "13:35",
      "groups": [
        {
          "key": "deepseek-v4",
          "requests": 800,
          "total_tokens": 168000,
          "total_bytes": 672000,
          "avg_latency_ms": 320,
          "count_429": 50
        }
      ]
    }
  ],
  "fallback_to_scan": false
}
```

#### `POST /admin/fuse` — 手动熔断 / 分批恢复

```
Header: x-api-key: xxx

请求:
{
  "action": "on" | "off" | "status",
  "reason": "global upstream 429",
  "dry_run": false,
  "restore_batch_size": 5,
  "restore_interval_secs": 60
}

语义:
  action=on      立即停止新调度, 将可调度节点标记为 fused, 不直接删除历史池状态
  action=off     不一次性全量恢复, 交给 ProbeScheduler 分批探活后回 DispatchPool
  action=status  只返回当前熔断状态
  dry_run=true   只返回预计影响节点数, 不改变状态

响应:
{
  "status": "ok",
  "fuse": true,
  "dry_run": false,
  "reason": "global upstream 429",
  "affected": {
    "dispatch_paused": 85,
    "active_waiting_release": 3,
    "ratelimited_kept": 7,
    "dead_kept": 5
  },
  "restore_policy": {
    "mode": "probe_then_dispatch",
    "batch_size": 5,
    "interval_secs": 60
  }
}

审计:
  - AuthGate 必须验证 x-api-key。
  - AuditSink 必须记录 action/reason/dry_run/remote_ip/affected/request_id。
  - 熔断关闭只能触发分批恢复, 禁止一次性把全部节点直接放回调度池。
```

#### `GET /admin/health` — 管理健康详情

```
响应:
{
  "status": "degraded",
  "uptime_secs": 3600,
  "fuse": false,
  "upstream": {
    "backoff": true,
    "backoff_until_ms": 1715940000000
  },
  "global_rate_limit": {
    "state": "suspected",
    "window_secs": 60,
    "distinct_nodes": 28,
    "distinct_exit_ips": 24,
    "rate_429": 0.82,
    "direct_probe_429": true,
    "action": "global_backoff_no_node_penalty"
  },
  "pools": {
    "dispatch": 68,
    "active": 3,
    "ratelimited": 24,
    "dead": 5
  }
}
```

#### `GET /admin/events` — 节点事件流 (运维层)

```
?node_id=ws-001                    按脱敏节点 ID 过滤
&provider=webshare                 按提供商过滤
&event=demoted                     按事件类型 (probe_ok/probe_fail/429/recovered/demoted/fuse_on/fuse_off/global_429)
&since=24h                         时间范围
&limit=100                         返回条数 (默认100, 最大1000)

响应:
{
  "total": 35,
  "events": [
    {
      "id": "evt-000123",
      "ts": "2026-05-17T14:28:01Z",
      "rid": "zen-12f",
      "provider": "webshare",
      "node_id": "ws-001",
      "from_pool": "dispatch",
      "to_pool": "ratelimited",
      "event": "rate_limited",
      "reason": "http_429",
      "status": 429,
      "score_before": 68.0,
      "score_after": 0.0
    }
  ]
}

持久化:
  - 事件写入 DataCollector 的 RingBuffer + WAL。
  - 节点事件和请求明细共用 seq 游标, 但 event_id 独立展示。
  - 所有 node 字段均为 node_id/provider, 不落完整代理 URL。
```

#### 已废弃端点 (被 /admin/requests 替代)

| 旧端点 | 替代方式 |
|--------|---------|
| `/admin/stats` | `/admin/requests?group=model&aggregate=count` 或 `/admin/health` |
| `/admin/models` | `/v1/models` 或 `/admin/requests?group=model&aggregate=count` |

### 2.3 内部接口 (trait) 清单

```
trait PoolManager:
  fn dispatch(&self, req: &RequestMeta) -> Result<DispatchResult, DispatchError>
  fn report(&self, node_id, result, latency_us)
  fn pool_stats(&self) -> PoolStats

trait FuseController (embedded in PoolManager):
  fn fuse_all(&self);                    // 一键全部熔断
  fn unfuse_all(&self);                  // 一键全部恢复
  fn is_fused(&self) -> bool;           // 当前熔断状态

trait Pool:
  type NodeId: Clone + Hash + Eq;
  fn acquire(&self) -> Option<NodeRef>
  fn release(&self, node, result)
  fn remove(&self, node_id)
  fn add(&self, node)
  fn available(&self) -> usize
  fn name(&self) -> &str

trait RateLimitedPool (extends Pool):
  fn quarantine(&self, node_id)
  fn select_for_probe(&self, batch_size) -> Vec<NodeId>
  fn recover(&self, node_id)
  fn quarantined_today(&self) -> usize

trait DeadPool (extends Pool):
  fn bury(&self, node_id)
  fn select_all_for_probe(&self) -> Vec<NodeId>
  fn recover(&self, node_id)
  fn dead_count(&self, node_id) -> u32

trait DataCollector:
  fn record_request(&self, tele: &RequestTelemetry)   // 22字段
  fn record_pool(&self, event: PoolEvent)
  fn record_schedule(&self, event: ScheduleEvent)
  fn record_probe(&self, event: ProbeEvent)
  fn record_system(&self, event: SystemEvent)
  fn snapshot(&self) -> DataSnapshot
  fn set_backend(&self, backend: Box<dyn StorageBackend>)

trait StorageBackend:
  fn write(&self, snapshot: &DataSnapshot)
  fn name(&self) -> &str

trait NodeProvider:
  type NodeId: Clone + Hash + Eq + Debug
  fn all_urls(&self) -> Vec<String>
  fn id_for_url(&self, url: &str) -> Self::NodeId
  fn name(&self) -> &str

trait UpstreamHealth:
  fn record(&self, status: u16)
  fn is_backoff(&self) -> bool
  fn stats(&self) -> HealthStats

trait AuthGate:
  fn authorize(&self, request: &AdminRequest) -> Result<AdminPrincipal, AuthError>

trait AuditSink:
  fn record_admin_action(&self, event: AdminAuditEvent)

trait Sanitizer:
  fn redact_url(&self, url: &str) -> RedactedUrl
  fn node_label(&self, identity: &NodeIdentity) -> String
  fn normalize_model_label(&self, model: &str) -> String
  fn hash_client_token(&self, bearer: &str) -> ClientId

trait QueryLimiter:
  fn limit_request_query(&self, query: RequestQuery) -> RequestQuery
```

---

## 三、关键算法

### 3.1 调度池加权随机选人

```rust
// 位置: pool/dispatch.rs
// 复杂度: O(n)

fn select_node(&self) -> Option<usize> {
    let now = unix_ms();
    let scores: Vec<(usize, f64)> = self.nodes.iter()
        .enumerate()
        .filter(|(_, n)| n.is_available(now))
        .map(|(i, n)| (i, n.score(now)))
        .collect();

    let total: f64 = scores.iter().map(|(_, s)| s).sum();
    if total <= 0.0 { return None; }

    let pick = fastrand::f64() * total;
    let mut cumulative = 0.0;
    for (idx, score) in &scores {
        cumulative += score;
        if pick <= cumulative { return Some(*idx); }
    }
    None
}
```

### 3.2 节点健康度评分

```rust
// 位置: pool/dispatch.rs PoolNode::score()

fn score(&self, now: i64) -> f64 {
    // 5 个维度加权求和
    let health    = f64::from_bits(self.base_score.load(Ordering::Relaxed)) / 100.0; // 0~1
    let success   = self.recent_success_rate() * 0.5;                  // 0~0.5
    let idle      = self.idle_factor(now) * 0.15;                      // 0~0.15
    let latency   = self.latency_factor() * 0.10;                      // 0~0.10
    let momentum  = self.momentum_factor() * 0.05;                     // 0~0.05

    (health * 0.50 + success / 0.5 * 0.20 + idle + latency + momentum)
        .clamp(0.0, 1.0)
}

// 各维度计算:
//   success_rate: 最近20次中成功比例
//   idle_factor:  (now - idle_since) / 120s, clamp 0~1
//   latency_factor: 1 - (avg_latency / 10000ms), clamp 0~1
//   momentum: consecutive_successes / 50, clamp 0~1
```

### 3.3 全局 429 判定 + 节点惩罚门控

```rust
// 位置: health/global_429.rs
// 目的: 区分“单节点被限流”和“上游/API key/模型/ASN 全局限流”

状态:
  Normal      正常, 429 按节点惩罚, 进入 RateLimitedPool
  Suspected   短窗口内 429 占比异常, 暂缓重罚节点, 开始直连探测
  Confirmed   多节点/多出口 + 直连探测均 429, 进入全局退避, 不继续清空调度池

判定条件 (默认):
  window_secs = 60
  min_distinct_nodes = 20
  min_distinct_exit_ips = 10
  rate_429_threshold = 0.70
  direct_probe_required = true

fn observe(event: RateLimitObservation) -> GlobalRateLimitState {
    window.push(event);
    let stats = window.stats();

    if stats.rate_429 >= 0.70
        && stats.distinct_nodes >= 20
        && stats.distinct_exit_ips >= 10
    {
        if direct_probe_to_upstream() == ProbeResult::RateLimited {
            return Confirmed { backoff: exponential_backoff() };
        }
        return Suspected;
    }
    Normal
}

PoolManager.report(RateLimited) 行为:
  Normal     -> 惩罚该节点, 移入 RateLimitedPool
  Suspected  -> 轻惩罚, 不立即移出所有节点, 降低调度权重
  Confirmed  -> 记录全局429, 开启全局退避, 不再把每个节点都打入429池

恢复:
  全局退避期结束后, ProbeScheduler 先小批量真实 HTTP 探测。
  连续成功达到阈值后回 Normal, 再恢复节点级惩罚。
```

### 3.4 使用池动态并发控制

```rust
// 位置: pool/active.rs

初始值: max_concurrent = 5
上限: 20
下限: 1

每次成功: max_concurrent = min(max_concurrent + 1, 20)
每次失败: max_concurrent = max(max_concurrent / 2, 1)

调度时:
  if active_requests >= max_concurrent:
      PoolManager 暂不通过该节点发新请求
```

### 3.4 智能退避 (保留原实现)

```rust
// 位置: utils.rs

pub fn smart_backoff(attempt: u32, _status: Option<u16>) -> f64 {
    (0.5 * (2.0f64).powi(attempt as i32)).min(30.0)
}

pub fn should_retry(status: u16, attempt: u32, max: u32) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504) && attempt < max
}
```

### 3.5 探活期 3轮探测

```rust
// 位置: pool/probe_period.rs

async fn probe_period(node, dispatch, dead, collector) {
    let successes = (0..3)
        .map(|_| probe_node(node))
        .timeout(30s)
        .await
        .filter(|r| r == Ok(200))
        .count();

    if successes >= 1 {
        // 网络波动 -> 5分钟观察
        sleep(300s).await;
        if probe_node(node).await != Ok(200) {
            return probe_period(node, dispatch, dead, collector).await;
        }
        dispatch.add(node);
    } else {
        dead.bury(node);
    }
}
```

### 3.6 环形缓冲区扫描 + 游标分页

```rust
// 位置: collector/ring_buffer.rs
// 复杂度: O(N), N=当前存活条数
// 10k 条 ~2ms, 50k 条 ~10ms

fn query(&self, since: i64, filters: &[Filter], limit: usize, cursor: Option<u64>,
    sort: Option<(&str, SortOrder)>)  // 新增 sort 参数
    -> (Vec<RequestTelemetry>, Option<u64>, bool)
{
    let head = self.head.load(Ordering::Relaxed);
    let cap = self.capacity as u64;
    let total = head.min(cap);  // 缓冲区尚未写满时的边界

    // 游标定位: 游标 = 上次最后一条的顺序号
    let start_idx = cursor.map(|c| c + 1).unwrap_or(head.saturating_sub(cap));
    if start_idx >= head { return (vec![], None, false); }

    // 如果 sort 不为 None:
    //   先收集所有匹配项 (不提前返回), M = 匹配项数
    //   按 sort.0 字段排序, sort.1 决定 asc/desc
    //   取前 limit 条
    //   复杂度: O(N) 扫描 + O(M log M) 排序
    if let Some((field, order)) = sort {
        let mut all: Vec<(u64, RequestTelemetry)> = Vec::new();
        for i in start_idx..head {
            let slot = &self.buffer[(i % cap) as usize];
            let item = unsafe { (*slot.get()).assume_init_ref() };
            if item.ts < since { continue; }
            if !filters.iter().all(|f| f.matches(item)) { continue; }
            all.push((i, item.clone()));
        }
        if all.is_empty() { return (vec![], None, false); }
        match order {
            SortOrder::Asc  => all.sort_by(|a, b| cmp_field(&a.1, &b.1, field)),
            SortOrder::Desc => all.sort_by(|a, b| cmp_field(&b.1, &a.1, field)),
        }
        all.truncate(limit.min(all.len()));
        let last_seq = all.last().map(|(seq, _)| *seq);
        let items = all.into_iter().map(|(_, t)| t).collect();
        return (items, last_seq, false);
    }

    let mut results = Vec::with_capacity(limit.min(100));
    let mut new_cursor = None;

    // 从旧到新正向扫描 (无排序, 可达提前返回)
    for i in start_idx..head {
        let slot = &self.buffer[(i % cap) as usize];
        let item = unsafe { (*slot.get()).assume_init_ref() };
        if item.ts < since { continue; }
        if !filters.iter().all(|f| f.matches(item)) { continue; }
        if results.len() >= limit {
            new_cursor = Some(i - 1);
            return (results, new_cursor, true);
        }
        results.push(item.clone());
        new_cursor = Some(i);
    }
    (results, new_cursor, false)
}
```

### 3.7 滚动窗口聚合 + 快照恢复

```rust
// 位置: collector/aggregator.rs
// 写入: ~1.5μs/请求 (5个 HashMap update + 1次边界检查)
// 查询预计算维度: O(1)
// 查询多维组合: 标记 fallback, 环形缓冲区扫描聚合

// 窗口边界对齐:
fn window_start(ts: i64) -> i64 {
    ts / 300_000 * 300_000  // 对齐到整 5 分钟
}

// 聚合结果拼接:
fn query(&self, since: i64, until: i64, group: GroupBy, aggregate: Aggregate) -> AggregatedResult {
    let current = self.current.lock();
    let completed = self.completed.lock();
    let mut windows: Vec<AggregatedWindow> = Vec::new();

    // 1. 从 completed 中取出范围的窗口
    for w in completed.iter().filter(|w| w.window_start >= since && w.window_start < until) {
        windows.push(w.to_window(group, aggregate));
    }
    // 2. 当前窗口中的部分
    if current.window_start >= since && current.window_start < until {
        windows.push(current.to_window_with_truncation(group, aggregate, until));
    }
    // 3. 如果 group 是预计算维度且时间范围在 completed 内 -> 精确返回
    //    如果超出 1h 或含多维组合 -> fallback_to_scan = true
    let truncated = since < completed.front().map(|w| w.window_start).unwrap_or(i64::MAX);
    AggregatedResult { windows, fallback_to_scan: truncated }
}
```

### 3.8 UpstreamHealth 退避 (保留原实现)

```rust
// 位置: health.rs

连续的 HTTP 429 计数:
  1~5 次: 退避 1s
  6:      退避 2s
  7:      退避 4s
  8:      退避 8s
  9:      退避 16s
  10+:    退避 32s (上限)

退避期间:
  /health 显示 upstream.backoff = true
  /admin/stats 显示当前退避状态
  不参与路由决策 (仅监控展示)
```

---

## 四、旧算法对照表

| 旧算法 (v2.0) | 新算法 (v3.0) | 变更 |
|-------------|-------------|------|
| TokenBucket AIMD (全局限流) | ActivePool::max_concurrent + UpstreamHealth | 节点级并发 + 全局429退避分层处理 |
| ProxySelector 3-pass RR | DispatchPool 加权随机 | 3-pass 不区分质量，加权随机更智能 |
| StickySession + PoolSelector | 删除 | 不再需要粘性会话 |
| compute_score() (固定公式) | PoolNode::score() (5维度加权) | 维度增加、权重可调 |
| 探活 TCP connect | probe logic 真实 HTTP 请求 | TCP 无法验证 SOCKS5 可用性 |
| 探活 6定时器 (L1/L3/WS/Dead/Dump/Purge) | ProbeScheduler 统一调度各池探活 | 429池/死池/探活期通过接口接入 |
| NodeDB 持久化 (写JSON) | DataCollector snapshot + StorageBackend | 统一数据输出，可换格式 |
| 裸 admin x-api-key | AuthGate + AuditSink | 认证与审计变成可测试榫头 |
| 完整 node URL 进指标 | Sanitizer + NodeIdentity | 指标/WAL/API 全链路脱敏 |
| smart_backoff 按状态码区分 | smart_backoff + 全局429退避 | 节点错误和全局限流分开 |
| pool_events.log 事件文件 | DataCollector 事件流 | 与请求明细共用 WAL/查询能力 |
| **环形缓冲区扫描 (新增)** | RingBuffer::query() | 固定内存 2MB, 10k 条 ~2ms |
| **滚动窗口聚合 (新增)** | RollingAggregator::query() | 5个预计算维度 O(1), 多维走扫描 |
| **WAL 回放恢复 (新增)** | WAL::refill_ring_buffer() | 崩溃恢复 <100ms |
| **游标分页 (新增)** | base64url(seq) 游标 | rid 只做请求 ID, cursor 只做分页定位 |
