# 文档索引

|文档|日期|说明|
|[2026-07-31 综合：thinking 透出方案 + 渠道69 报错诊断 + 架构梳理](diagnosis-2026-07-31-thinking-passthrough-channel69-errors-arch-review.md)|2026-07-31|三合一报告。thinking 为何不透出（kernel 只累加不发射）+ 渠道69 报错全量统计（reasoning_only/上下文超限/记账污染）+ 架构可优化空间 O1-O15|
|---|---|---|
|[渠道 69 全量诊断报告](diagnosis-2026-07-27-channel69-comprehensive.md)|2026-07-27|**主报告**。基于 NewAPI + zen-proxy-rs audit per-request 数据，含按模型拆分的延迟分解、EO 分析、缓存审计、节点行为、根因矩阵、实施路线|
|[渠道 69 待办与发布清单](plan-2026-07-22-zenproxy-reasoning-only-retry.md)|2026-07-22|运维发布清单。**部分结论已被 07-27 诊断报告证伪**（见报告 §0 勘误）|
|[项目交接](PROJECT_HANDOFF.md)|2026-07-27|项目定位、链路拓扑、生产事实、工程反思、替换顺序|
|[Cache 99+ 架构方案](cache-95plus-architecture.md)|2026-07-06|CCP × ICP × 五层协同设计。含逐次排障更新（2026-07-03 至 07-06 共七次）|
|[ClaudeCode 稳定性交接](CLAUDECODE_STABILITY_HANDOFF_2026-07-15.md)|2026-07-15|三模型矩阵验收与缓存状态|
|[运维规则](OPERATING_RULES.md)|2026-07-15|硬约束、测试链路、部署规则、文档规则、monorepo 规则|
|[清理与结构](CLEANUP_AND_STRUCTURE.md)|2026-07-02|monorepo 聚合结构与清理|

## 外部入口

- [根目录 plan.md](../plan.md)
- `ops/panda/` — pandas 运维脚本
