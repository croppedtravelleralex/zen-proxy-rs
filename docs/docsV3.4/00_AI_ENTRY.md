# ZenProxyRS_M v3.4 文档入口

## 版本定位

ZenProxyRS_M 中的 `M` 定义为：

- **Modular**：模块化
- **Model-aware**：模型感知
- **Manageable**：可管理

ZenProxyRS_M 不再只是一个"转发到 opencode 的代理"，而是升级为：

> 面向免费/低成本模型资源的 OpenAI-compatible 智能反代控制面。

## 当前代码基线 (v3.0)

当前代码（`feat/ledger-observability-integration` 分支）已实现：
- 五池状态机（Dispatch/Active/RateLimited/Dead/ProbePeriod）
- PoolManager trait + PoolManagerImpl 编排器
- DataCollector（RingBuffer + RollingAggregator + WAL + Prometheus）
- LedgerCounters + LedgerEvent JSONL 写入
- OpenCode headers 注入
- SSE frame-aware 缓冲
- 4 个 admin 端点（GET /admin/pools, /admin/fuse, /admin/health, /admin/nodes）
- 45 个单元测试

7 个文件为存根（1 字节）：admin.rs, bandwidth.rs, metrics.rs, node_db.rs, node_probe.rs, selector.rs, token_bucket.rs

详见[差距分析](./13_最优实施路线.md#phase-0-文档与契约收敛)的基线部分。

## v3.4 核心能力

1. 统一 OpenAI-compatible 入口（已有）
2. 可插拔上游 Provider（新增抽象）
3. 可插拔代理/出口节点（新增抽象）
4. 自动/手动模型探测（新增）
5. Provider 到代理节点的 Route 管理（新增）
6. 请求调用记录（已有，需统一）
7. 节点健康、熔断、重试、退避（已有，需增强）
8. Admin API 管理面（已有 4 端点，需扩展至全 CRUD）
9. 配置热加载（已有 SIGHUP，需增加 API）
10. 后续支持 Dashboard / SQLite / 成本统计 / 插件化

## 文档目录

| 编号 | 文档 | 说明 |
|---|---|---|
| 00 | 本文 | 版本定位与文档入口 |
| 01 | [总体方案与设计原则](01_总体方案与设计原则.md) | 不推倒重来、边界划分、分层目标 |
| 02 | [分层架构](02_分层架构.md) | L7-L0 八层定义 |
| 03 | [核心领域对象](03_核心领域对象.md) | Model / Provider / Node / Route / RequestRecord / EventRecord / FuseState |
| 04 | [核心 Trait 与接口](04_核心Trait与接口.md) | ProviderAdapter / TransportAdapter / Registry / Selector 等 |
| 05 | [请求全链路流程](05_请求全链路流程.md) | 从 Client 到上游返回的完整路径 |
| 06 | [Public API 契约](06_Public_API契约.md) | OpenAI-compatible 公开 API |
| 07 | [Admin API 总体契约](07_Admin_API总体契约.md) | 统一认证、响应格式 |
| 08 | [Admin API 资源契约](08_Admin_API资源契约.md) | Models / Providers / Nodes / Routes / Requests / Events / Fuse |
| 09 | [配置、热加载与热插拔](09_配置热加载与热插拔.md) | JSON 配置 / SIGHUP / RegistrySnapshot |
| 10 | [探测、路由与请求记录](10_探测路由与请求记录.md) | Probe / Route Selection / Request Logging |
| 11 | [旧 API 处理与迁移路线](11_旧API处理与迁移路线.md) | 保留/改造/废弃清单 |
| 12 | [测试策略与优先级](12_测试策略与优先级.md) | P0/P1/P2 测试覆盖 |
| 13 | [最优实施路线](13_最优实施路线.md) | Phase 0-9 实施路线图 |
| 14 | [OpenCode 深度逆向报告](14_OpenCode深度逆向报告.md) | 源码反编译、限流机制、社区研究 |

## 验证命令

```bash
cargo check
cargo fmt --check
cargo test   # 当前 45 passed
```
