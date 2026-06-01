# Roadmap

## Now

1. 跑修复后的 WSL 三客户端 dry run，确认 OpenClaw subagent 修复在 50 次级别仍成立。
2. 针对 huge_context、`deepseek-v4-flash-lite` 长上下文语义漂移和 Hermes 慢路径做 lane/case 降级或隔离。
3. 清理或归类未跟踪文件，避免把 `.codex_tmp/`、密钥、测试输出混进提交。
4. 对压测前配置做安全确认：不污染 Hermes/OpenClaw/ClaudeCode 用户默认配置，不使用本机 `127.0.0.1:8081` 代替 panda。
5. 提交前复查 README、维护文档和 git 状态。

## Done This Phase

1. panda NewAPI 最小链路验收已通过：OpenAI 和 Anthropic 两类请求均返回 200。
2. Hermes 临时接入 panda NewAPI 的短回复和文件/终端工具小矩阵已完成；web 用例命令成功但返回 `WEB_FAIL`，保留观察。
3. OpenClaw 已通过隔离 Node 22.21.1、临时配置、models list、infer、agent 文件工具和 web_fetch。
4. 测试结果已纳入维护文档，不再只停留在聊天记录。
5. 根 README 已同步请求体限制、非流式保护、空上游错误行为和当前测试数量。
6. panda-only 四客户端 500 次压测方案、采集字段、通过门槛和报告模板已落到 `docs/06-panda-pressure-test-plan.md`。
7. `.codex_tmp/` 和根目录 0 字节未跟踪文件已在 `docs/02-current-state.md` 归类，默认不提交、不盲删。
8. 客户端识别和策略隔离 90 分方案已落到 `docs/07-client-profile-policy-plan.md`。
9. 客户端识别和策略隔离 90 分代码已落地：`ClientProfile`、`x-fmc-client`、per-client thinking/whitespace/tool-history policy 和 kernel golden 回归测试。
10. OpenClaw body/profile 识别修复已落地并部署到 panda：OpenClaw-only smoke 5/5，WSL ClaudeCode/Hermes/OpenClaw smoke 15/15。

## Next

1. 按 `docs/06-panda-pressure-test-plan.md` 执行修复后的 dry run；通过后再进入 Windows ClaudeCode 与 full run。
2. 继续补强 Windows ClaudeCode 测试执行环境，避免从 WSL 非交互环境误报 `config_error`。
3. 按 `docs/06-panda-pressure-test-plan.md` 执行 full run：Windows ClaudeCode、WSL ClaudeCode、WSL Hermes、WSL OpenClaw。
4. 加入更细粒度运行指标采集：protocol first byte、first content、first tool call、upstream connect、upstream status、stream parse error、empty upstream。
5. 补足 API 覆盖文档：OpenAI、Anthropic、Models、Health、错误响应、认证头、请求体限制。
6. 如果代码继续变化，保持根 README 和维护文档同步。

## Later

1. 与 ZenProxyRS 合包部署时，明确本仓库是 kernel、sidecar 还是 library crate。
2. 如果进入高并发生产，补 lane 调度、全局预算、节点分桶和 Redis/共享状态设计；本仓库当前没有完整池调度。
3. 长上下文 compactor 如要下沉到本仓库，需要单独设计语义保真测试，不能只按字节裁剪。
4. 建立 24h panda 稳定性验收：成功率、错误率、TTFT 分位、first_content 分位、工具调用成功率、短请求隔离。
