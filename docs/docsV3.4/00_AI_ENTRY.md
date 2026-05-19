# ZenProxyRS_M v3.4 文档入口

## 版本定位

ZenProxyRS_M 中的 `M` 定义为：

- **Modular**：模块化
- **Model-aware**：模型感知
- **Manageable**：可管理

ZenProxyRS_M 不再只是一个“转发到 opencode 的代理”，而是升级为：

> 面向免费/低成本模型资源的 OpenAI-compatible 智能反代控制面。

## 核心能力

1. 统一 OpenAI-compatible 入口
2. 可插拔上游 Provider
3. 可插拔代理/出口节点
4. 自动/手动模型探测
5. Provider 到代理节点的 Route 管理
6. 请求调用记录
7. 节点健康、熔断、重试、退避
8. Admin API 管理面
9. 配置热加载
10. 后续支持 Dashboard / SQLite / 成本统计 / 插件化

## 文档目录

- [01 总体方案与设计原则](01_总体方案与设计原则.md)
- [02 分层架构](02_分层架构.md)
- [03 核心领域对象](03_核心领域对象.md)
- [04 核心 Trait 与接口](04_核心Trait与接口.md)
- [05 请求全链路流程](05_请求全链路流程.md)
- [06 Public API 契约](06_Public_API契约.md)
- [07 Admin API 总体契约](07_Admin_API总体契约.md)
- [08 Admin API 资源契约](08_Admin_API资源契约.md)
- [09 配置、热加载与热插拔](09_配置热加载与热插拔.md)
- [10 探测、路由与请求记录](10_探测路由与请求记录.md)
- [11 旧 API 处理与迁移路线](11_旧API处理与迁移路线.md)
- [12 测试策略与优先级](12_测试策略与优先级.md)
- [13 最优实施路线](13_最优实施路线.md)
