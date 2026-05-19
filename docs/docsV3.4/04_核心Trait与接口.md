# ZenProxyRS_M v3.4 核心 Trait 与接口

> 注意：这是架构设计，不是实际代码。实际实现时可根据 Rust 惯例简化。

## 1. ProviderAdapter

职责：适配不同上游服务（OpenCode / OpenAI / OpenRouter / Ollama 等）。

```rust
#[async_trait]
trait ProviderAdapter: Send + Sync {
    fn provider_id(&self) -> &str;
    async fn list_models(&self) -> Result<Vec<Model>, ProviderError>;
    async fn probe_model(&self, model_id: &str) -> Result<ProbeStatus, ProviderError>;
    async fn build_request(&self, req: UpstreamRequest) -> Result<reqwest::RequestBuilder, ProviderError>;
    async fn parse_response(&self, response: reqwest::Response) -> Result<ParsedResponse, ProviderError>;
    async fn parse_error(&self, response: reqwest::Response) -> Result<ProviderErrorDetail, ProviderError>;
    async fn parse_stream(&self, line: &str) -> Result<Option<StreamChunk>, ProviderError>;
}
```

### 当前基线

- ✅ 只有 `trait NodeProvider`（3 方法：`all_urls` / `id_for_url` / `name`）
- ✅ `WebShareProvider` 实现了 `NodeProvider`
- ❌ 无 `ProviderAdapter` trait

---

## 2. TransportAdapter

职责：适配不同网络出口（Direct / HTTP Proxy / SOCKS5 / WebShare）。

```rust
#[async_trait]
trait TransportAdapter: Send + Sync {
    async fn send(&self, request: TransportRequest) -> Result<TransportResponse, TransportError>;
    async fn probe_node(&self, config: &ProbeConfig) -> Result<ProbeResult, TransportError>;
    async fn resolve_egress_ip(&self) -> Result<String, TransportError>;
    async fn measure_latency(&self) -> Result<Duration, TransportError>;
}
```

### 当前基线

- SOCKS5 客户端在 `DispatchPool::add()` 和 `PoolManagerImpl::make_client()` 中内联创建
- 无 `TransportAdapter` trait

---

## 3. RouteSelector

职责：从候选 routes 中根据策略选一个。

```rust
#[async_trait]
trait RouteSelector: Send + Sync {
    async fn select(&self, routes: &[Route], context: &RouteContext) -> Result<&Route, SelectorError>;
}
```

策略：Priority / WeightedRoundRobin / LeastLatency / LeastErrorRate / ManualPinned / FreeModelPreferred / HealthyOnly / StickyByClient / StickyByModel

### 当前基线

- ❌ 无 RouteSelector trait
- `DispatchPool::acquire()` 通过加权评分选择节点，可适配

---

## 4. ModelRegistry

职责：存储、查询、更新、删除模型。

```rust
#[async_trait]
trait ModelRegistry: Send + Sync {
    async fn list(&self, filter: &ModelFilter) -> Result<Vec<Model>, RegistryError>;
    async fn get(&self, provider_id: &str, model_id: &str) -> Result<Model, RegistryError>;
    async fn upsert(&self, model: Model) -> Result<Model, RegistryError>;
    async fn delete(&self, provider_id: &str, model_id: &str) -> Result<(), RegistryError>;
    async fn enable(&self, provider_id: &str, model_id: &str) -> Result<(), RegistryError>;
    async fn disable(&self, provider_id: &str, model_id: &str) -> Result<(), RegistryError>;
    async fn mark_probe_result(&self, provider_id: &str, model_id: &str, status: ProbeStatus) -> Result<(), RegistryError>;
}
```

### 当前基线

- ✅ `/v1/models` 返回硬编码列表
- ❌ 无 `ModelRegistry` trait

---

## 5. ProviderRegistry

职责：管理上游 provider。

```rust
#[async_trait]
trait ProviderRegistry: Send + Sync {
    async fn list(&self, filter: &ProviderFilter) -> Result<Vec<Provider>, RegistryError>;
    async fn get(&self, provider_id: &str) -> Result<Provider, RegistryError>;
    async fn upsert(&self, provider: Provider) -> Result<Provider, RegistryError>;
    async fn delete(&self, provider_id: &str) -> Result<(), RegistryError>;
    async fn enable(&self, provider_id: &str) -> Result<(), RegistryError>;
    async fn disable(&self, provider_id: &str) -> Result<(), RegistryError>;
    async fn set_active_route(&self, provider_id: &str, route_id: &str) -> Result<(), RegistryError>;
}
```

### 当前基线

- ❌ 无 `ProviderRegistry` trait
- 当前只有 1 个上游，硬编码在 `config.upstream_base`

---

## 6. NodeManager

职责：管理代理节点。

```rust
#[async_trait]
trait NodeManager: Send + Sync {
    async fn list(&self, filter: &NodeFilter) -> Result<Vec<Node>, ManagerError>;
    async fn get(&self, node_id: &str) -> Result<Node, ManagerError>;
    async fn add(&self, node: Node) -> Result<Node, ManagerError>;
    async fn update(&self, node: Node) -> Result<Node, ManagerError>;
    async fn delete(&self, node_id: &str) -> Result<(), ManagerError>;
    async fn enable(&self, node_id: &str) -> Result<(), ManagerError>;
    async fn disable(&self, node_id: &str) -> Result<(), ManagerError>;
    async fn probe(&self, node_id: &str) -> Result<ProbeResult, ManagerError>;
    async fn mark_health(&self, node_id: &str, health: NodeHealth) -> Result<(), ManagerError>;
}
```

### 当前基线

- ✅ `PoolManager` trait + `PoolManagerImpl` 编排五池状态机
- ❌ 无 `NodeManager` trait（现有 `PoolManager` 偏底层）
- ❌ 无 region/tags/limits 元数据

---

## 7. RouteManager

职责：管理 provider→node 的链路。

```rust
#[async_trait]
trait RouteManager: Send + Sync {
    async fn list_routes(&self, filter: &RouteFilter) -> Result<Vec<Route>, ManagerError>;
    async fn get_route(&self, route_id: &str) -> Result<Route, ManagerError>;
    async fn add_route(&self, route: Route) -> Result<Route, ManagerError>;
    async fn update_route(&self, route: Route) -> Result<Route, ManagerError>;
    async fn delete_route(&self, route_id: &str) -> Result<(), ManagerError>;
    async fn enable_route(&self, route_id: &str) -> Result<(), ManagerError>;
    async fn disable_route(&self, route_id: &str) -> Result<(), ManagerError>;
    async fn switch_provider_route(&self, provider_id: &str, route_id: &str) -> Result<(), ManagerError>;
}
```

### 当前基线

- ❌ 完全不存在

---

## 8. RequestLedger

职责：记录和查询请求调用记录。

```rust
#[async_trait]
trait RequestLedger: Send + Sync {
    async fn record(&self, record: RequestRecord) -> Result<(), LedgerError>;
    async fn query(&self, filter: &RequestFilter) -> Result<Vec<RequestRecord>, LedgerError>;
    async fn get(&self, request_id: &str) -> Result<RequestRecord, LedgerError>;
    async fn aggregate(&self, filter: &AggregationFilter) -> Result<AggregationResult, LedgerError>;
    async fn export(&self, format: ExportFormat) -> Result<String, LedgerError>;
}
```

### 当前基线

- ✅ `LedgerCounters`：内存维聚合 + JSONL 错误事件写入
- ✅ `DataCollector`：7 层数据采集 + RingBuffer + RollingAggregator + WAL + Prometheus
- ❌ 两系统未统一
- ❌ 无 `RequestLedger` trait
- ❌ 无查询接口

---

## 9. EventStore

职责：记录和查询系统事件。

```rust
#[async_trait]
trait EventStore: Send + Sync {
    async fn emit(&self, event: EventRecord) -> Result<(), StoreError>;
    async fn query(&self, filter: &EventFilter) -> Result<Vec<EventRecord>, StoreError>;
    async fn stream(&self, filter: &EventFilter) -> Result<Pin<Box<dyn Stream<Item = EventRecord> + Send>>, StoreError>;
}
```

### 当前基线

- ✅ `LedgerEvent` JSONL 写入（仅错误/转换事件）
- ✅ collector 中有 `PoolEvent` / `ProbeEvent` / `SystemEvent`
- ❌ 无 `EventStore` trait
- ❌ 无统一查询接口

---

## 10. ProbeScheduler

职责：探测模型、Provider、Node、Route。

```rust
#[async_trait]
trait ProbeScheduler: Send + Sync {
    async fn probe_model(&self, provider_id: &str, model_id: &str) -> Result<ProbeResult, ProbeError>;
    async fn probe_provider(&self, provider_id: &str) -> Result<ProbeResult, ProbeError>;
    async fn probe_node(&self, node_id: &str) -> Result<ProbeResult, ProbeError>;
    async fn probe_route(&self, route_id: &str) -> Result<ProbeResult, ProbeError>;
    async fn schedule_periodic_probe(&self, interval: Duration, target: ProbeTarget) -> Result<(), ProbeError>;
    async fn query_probe_result(&self, probe_id: &str) -> Result<ProbeResult, ProbeError>;
}
```

### 当前基线

- ✅ `ProbePeriod::probe_node()`：向上游发测试请求，最多 3 次重试
- ✅ `PoolManagerImpl` 内联 tokio::spawn 后台探活
- ❌ 无 `ProbeScheduler` trait
- ❌ 无模型探测
