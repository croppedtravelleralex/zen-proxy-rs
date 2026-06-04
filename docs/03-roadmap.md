# Roadmap

## Now

1. 部署并验收 V4.98 cache-friendly session：确认长会话不再因尾部增长导致上游 session 每轮变化，并用 `prefix_4k/32k/128k/256k` 与 cache tokens 对齐判断是否提升命中。
2. 继续跑真实 panda `policy-smoke` / `policy-dry`，确认输出限制取消、flash 输入只观测不压缩、provider usage/header/body 信号和 cache 四态在生产链路上闭环。
3. policy harness 通过后再跑修复后的四客户端 smoke/dry，确认 OpenClaw、Hermes、Windows ClaudeCode、WSL ClaudeCode 的真实客户端状态。
4. 针对 huge_context、`deepseek-v4-flash-lite` 长上下文语义漂移、Hermes 慢路径和输出限制取消后的 413/超时/空输出/成本风险做 lane/case 降级或隔离。
5. 清理或归类未跟踪文件，避免把 `.codex_tmp/`、密钥、测试输出混进提交。
6. 对压测前配置做安全确认：不污染 Hermes/OpenClaw/ClaudeCode 用户默认配置，不使用本机 `127.0.0.1:8081` 代替 panda。
7. 提交前复查 README、维护文档和 git 状态。

## Done This Phase

1. panda NewAPI 最小链路验收已通过：OpenAI 和 Anthropic 两类请求均返回 200。
2. Hermes 临时接入 panda NewAPI 的短回复和文件/终端工具小矩阵已完成；web 用例命令成功但返回 `WEB_FAIL`，保留观察。
3. OpenClaw 已通过隔离 Node 22.21.1、临时配置、models list、infer、agent 文件工具和 web_fetch。
4. 测试结果已纳入维护文档，不再只停留在聊天记录。
5. 根 README 曾同步请求体限制、非流式保护、空上游错误行为和当时测试数量；输出限制完全取消后，根 README 仍需后续单独同步，本轮只改 `docs/`。
6. panda-only 四客户端 500 次压测方案、采集字段、通过门槛和报告模板已落到 `docs/06-panda-pressure-test-plan.md`。
7. `.codex_tmp/` 和根目录 0 字节未跟踪文件已在 `docs/02-current-state.md` 归类，默认不提交、不盲删。
8. 客户端识别和策略隔离 90 分方案已落到 `docs/07-client-profile-policy-plan.md`。
9. 客户端识别和策略隔离 90 分代码已落地：`ClientProfile`、`x-fmc-client`、per-client thinking/whitespace/tool-history policy 和 kernel golden 回归测试。
10. OpenClaw body/profile 识别修复已落地并部署到 panda：OpenClaw-only smoke 5/5，WSL ClaudeCode/Hermes/OpenClaw smoke 15/15。
11. 输出限制已在当前源码/ZenProxy 侧完全取消：缺省 `max_tokens` 不再自动补值，显式值原样透传；真实 panda policy-smoke/policy-dry 尚未跑，不能写成生产已验证。
12. `policy-smoke` / `policy-dry` harness 已落地，可记录 input/output wall、provider header/body usage 和 cache `attempted/accepted/rejected/ignored` 四态。
13. `zen-proxy-rs` 外层 V4 context compactor 已完成模型分流：flash/free 只 warn/pass 不 compact/reject，lite 仍保留 compact 能力；本地 e2e 已覆盖。
14. V4.98 cache-friendly session 源码已落地：大请求 session 按稳定前缀而不是完整 messages hash 分组，并补 `prefix_4k/32k/128k/256k` 脱敏观测；真实 panda A/B 尚未跑。

## Next

1. 按 `docs/06-panda-pressure-test-plan.md` 执行 policy-smoke / policy-dry；任一 policy gate 失败都不进入四客户端 dry/full，尤其要确认 panda 上 flash/free 没有输入墙、输出墙或隐藏 compactor。
2. 继续补强 Windows ClaudeCode 和 WSL ClaudeCode 测试执行环境，避免从 WSL 非交互环境或 clawgod launcher 误报 `config_error`。
3. policy 通过后按 `docs/06-panda-pressure-test-plan.md` 执行四客户端 dry run，再决定是否进入 full run：Windows ClaudeCode、WSL ClaudeCode、WSL Hermes、WSL OpenClaw。
4. 加入更细粒度运行指标采集：protocol first byte、first content、first tool call、upstream connect、upstream status、stream parse error、empty upstream。
5. 补足 API 覆盖文档：OpenAI、Anthropic、Models、Health、错误响应、认证头、请求体限制。
6. 如果代码继续变化，保持根 README 和维护文档同步。

## Later

1. 与 ZenProxyRS 合包部署时，明确本仓库是 kernel、sidecar 还是 library crate。
2. 如果进入高并发生产，补 lane 调度、全局预算、节点分桶和 Redis/共享状态设计；本仓库当前没有完整池调度。
3. 长上下文 compactor 如要下沉到本仓库，需要单独设计语义保真测试，不能只按字节裁剪。
4. 建立 24h panda 稳定性验收：成功率、错误率、TTFT 分位、first_content 分位、工具调用成功率、短请求隔离。
