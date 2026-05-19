# Zen Proxy RS v3 AI 入口

> 本目录只记录 Rust v3 版本的设计、实现边界、迁移计划和维护规则。旧 Python 方案不再作为本文档集的事实来源。

## 项目定位

Zen Proxy RS v3 是一个面向 LLM API 转发场景的 Rust 单进程代理核心。目标是在低内存、低进程数、可观测、可灰度、可回滚的前提下，提供 OpenAI 兼容接口转发、SOCKS5 节点调度、节点状态机、限流隔离、失败探活、遥测采集和管理员观测能力。

核心实现采用 axum + tokio + reqwest，所有关键模块通过 trait 边界榫接。调用方只依赖接口，不直接依赖具体实现，从而让节点提供商、池实现、采集后端和导出格式可以逐步替换。

## 当前 Rust v3 主线

| 领域 | 当前设计 |
|---|---|
| 入口 | `src/main.rs` 负责配置加载、依赖组装、路由注册、后台任务和优雅关闭 |
| 配置 | `src/config.rs` 从环境变量加载，包含监听、上游、认证、池容量、超时、流式限制等字段 |
| 转发 | `src/proxy.rs` 处理 `/v1/*` 转发、模型映射、鉴权、重试、SSE 修补和遥测记录 |
| 服务端 | `src/server.rs` 提供 `/metrics`、`/admin/pools`、`/admin/fuse`、`/admin/health`、`/admin/stats` |
| 池系统 | `src/pool/` 定义 Pool、PoolManager、RateLimitedPool、DeadPool、NodeProvider 等接口与实现 |
| 采集系统 | `src/collector/` 定义 DataCollector、StorageBackend、请求遥测、环形缓冲、WAL、聚合、导出 |
| 节点来源 | `src/provider/webshare.rs` 提供 WebShareProvider，后续可替换为其他 provider |
| 测试 | `tests/e2e_integration.rs` 覆盖健康检查、指标、首页、管理员鉴权和模型列表 |

## 文档阅读顺序

1. `00_AI_ENTRY.md`：本文件，确认阅读顺序和维护边界。
2. `01_概览与架构.md`：Rust v3 总体架构、进程模型、调用链和模块分层。
3. `02_模块详细设计.md`：trait、结构体、模块责任和榫卯接口。
4. `03_增强功能.md`：v3 增强能力清单、状态、验收标准。
5. `05_项目结构_API_算法.md`：项目结构、API 合约、关键算法。
6. `06_性能_部署_实施_附录.md`：性能预算、部署方案、灰度和回滚。
7. `07_耦合分析与榫卯架构方案.md`：耦合风险、边界规则和演进决策。
8. `GAP_REPORT_FINAL.md`：当前缺口、验收门禁和下一步。

## v3 榫卯架构总图

```text
HTTP 请求
  -> axum Router
  -> proxy::proxy_handler
      -> Config
      -> PoolManager trait
          -> DispatchPool
          -> ActivePool
          -> RateLimitedPool
          -> DeadPool
          -> ProbePeriod
      -> reqwest Client over SOCKS5
      -> DataCollector trait
          -> RingBuffer
          -> RollingAggregator
          -> WAL
          -> StorageBackend
      -> UpstreamHealth
  -> HTTP/SSE 响应
```

## 关键设计原则

1. 单进程优先：默认一个 Rust 进程承载转发、调度、采集和管理端点。
2. 接口先行：`proxy.rs` 只认 `PoolManager` 和 `DataCollector`，不直接操作具体池。
3. 状态分层：调度、活跃、限流、死亡和探活分别建模，禁止用一个全局黑名单承载全部状态。
4. 采集旁路：请求主路径只做轻量记录，聚合、导出和落盘通过 collector 后端处理。
5. 安全默认：管理端点需要 `ADMIN_API_KEY`，公网监听必须显式配置代理鉴权。
6. 可灰度：Rust v3 先完成本地和 release 验证，再进入 shadow、灰度、替换。
7. 可回滚：任何生产替换都必须保留旧服务切回路径和配置恢复步骤。

## 当前完成状态

| 项目 | 状态 |
|---|---|
| Rust 项目骨架 | 已建立 |
| axum 路由 | 已建立 |
| 配置加载与校验 | 已建立 |
| Pool trait 边界 | 已建立 |
| PoolManager 组装 | 已建立 |
| DataCollector trait 边界 | 已建立 |
| Prometheus 文本导出 | 已建立 |
| 管理端点鉴权 | 已建立 |
| E2E 基础测试 | 已建立 |
| 全量生产替换 | 待执行 |
| 节点 provider 多来源 | 待扩展 |
| release 压测和真实灰度 | 待执行 |

## 后续 AI 接手规则

- 只以 Rust v3 代码、Cargo 配置、测试结果和本 docs 为事实来源。
- 不再把旧 Python 文件、旧 WSL 运维记录、旧代理脚本写入本目录正文。
- 若发现代码和文档冲突，以代码与最新验证结果为准，并同步修正文档。
- 声称“已完成”必须有对应代码路径、测试、构建或运行证据。
- 新增能力先写在 `03_增强功能.md`，落地后同步 `05_项目结构_API_算法.md` 和 `GAP_REPORT_FINAL.md`。
