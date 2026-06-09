# Roadmap

## Now

1. V4.105 ClaudeCode true-stream/cache-hit 源码已落地并本地验证通过；下一步部署到 panda，并用真实 ClaudeCode 长会话确认 `anthropic_buffered` 误触发、cc-switch 真实首字和 cache hit 统计是否改善。
2. 继续验收 V4.98 cache-friendly session：代码和 panda 部署已完成，下一步用同一 ClaudeCode 长会话确认 `prefix_4k/32k/128k/256k` 是否稳定，并与 cache tokens、`frt`、总耗时对齐判断是否提升命中。
3. 观察已部署的 V4.104 ClaudeCode progressive tool streaming：确认大 Write/Edit/Agent 工具参数不再等完整 JSON 才出现真实 tool_use 首字，同时 `Invalid tool parameters`、半截工具 JSON 和重复工具风暴不回潮。
4. 观察已部署的 ClaudeCode 低预算工具探针保护：确认真实 `/context` 等非流式小工具探针不再因 `reasoning_only_length` 裸 502，同时普通 ClaudeCode 工具调用仍不默认禁用 thinking。
5. 观察已部署的 ClaudeCode Anthropic stream idle ping：确认 50k+ 流式请求 `client_gone/use_time≈64s/completion=0` 红行是否下降；注意该 ping 只保活，不代表 first content 变快。
6. 观察 V4.99 reasoning-aware output guard 生产效果：确认短/中非流式 `reasoning_only_length` 不再裸 502，低预算小流式不再误进 huge buffered，且 ClaudeCode 大流式主会话不被默认禁用 thinking。
7. 继续跑真实 panda `policy-smoke` / `policy-dry`，确认输出限制取消、flash 输入只观测不压缩、provider usage/header/body 信号、cache 四态和 V4.99 空输出分类在生产链路上闭环；同时记录既有上游空输出/节点质量问题，避免误归因到 V4.98/V4.99。
8. policy harness 通过后再跑修复后的四客户端 smoke/dry，确认 OpenClaw、Hermes、Windows ClaudeCode、WSL ClaudeCode 的真实客户端状态。
9. 针对 huge_context、`deepseek-v4-flash-lite` 长上下文语义漂移、Hermes 慢路径和输出限制取消后的 413/超时/空输出/成本风险做 lane/case 降级或隔离。
10. 清理或归类未跟踪文件，避免把 `.codex_tmp/`、密钥、测试输出混进提交。
11. 对压测前配置做安全确认：不污染 Hermes/OpenClaw/ClaudeCode 用户默认配置，不使用本机 `127.0.0.1:8081` 代替 panda。
12. 提交前复查 README、维护文档和 git 状态。
13. NewAPI 使用日志导出器已在 panda 以 systemd 服务上线；后续补专用只读 DB 用户和 NewAPI `type` 精确映射。

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
14. V4.98 cache-friendly session 已落地并部署 panda：大请求 session 按稳定前缀而不是完整 messages hash 分组，并补 `prefix_4k/32k/128k/256k` 脱敏观测；真实 panda 长会话 A/B 尚未跑。
15. V4.99 reasoning-aware output guard 已在源码落地并部署 panda：OpenAI/Anthropic 非流式 `reasoning_only_length` 只重试一次 `thinking: disabled`，小流式低预算不再仅因 `max_tokens<=512` 进入 ClaudeCode huge buffered，空输出错误带分类；本地 fmt/clippy/test 已通过，panda 三实例健康，NewAPI OpenAI 非流式和 Anthropic 流式 smoke 通过。
16. 独立 `tools/newapi-usage-exporter` 已落地并部署 panda：Rust CLI/HTTP sidecar，可按 `user_id + time range` 或一句话 instruction 从 NewAPI SQLite/Postgres 日志只读导出脱敏分析包；本地 fmt/clippy/test、panda Postgres 直连 smoke、helper 和 HTTP instruction 均通过。
17. ClaudeCode 低预算工具探针保护已在源码落地并部署 panda：OpenAI/Anthropic 非流式小工具探针会初始禁用 thinking，并将 `max_tokens<=32` 的上游预算最小抬到 64；panda NewAPI `max_tokens=1/16` 工具探针均返回 200 且 `stop_reason=tool_use`。
18. ClaudeCode Anthropic stream idle ping 已在源码落地并部署 panda：ClaudeCode Anthropic SSE 下游 15 秒无可转发事件时发送协议级 `ping`，本地 delayed-stream golden 通过，panda 三实例健康，NewAPI Anthropic smoke 200。
19. V4.102 ClaudeCode 工具参数完整性门控已落地并部署 panda：缺必填参数、空 `{}`、重复补参循环和明显坏文件路径均在源头拦截或窄修复；Windows ClaudeCode 真实 Bash/Write/Read、ToolSearch、Task/Agent、Markdown 小矩阵通过，NewAPI 近窗口无错误类型日志。
20. V4.103 ClaudeCode 工具门控续修已随 V4.104 部署 panda：补 `SendMessage` 字符串消息缺 `summary`、空 `command/query`、同一 assistant response 内重复同参工具调用，以及流式 `provider_missing_reasoning_content` 工具历史降级重试漏口。
21. V4.104 ClaudeCode 低延迟工具流与质量回退已落地并部署 panda：ClaudeCode Anthropic 工具调用改为先发真实 `tool_use` start，再按 `input_json_delta` 增量流参数，最终完整 JSON 校验通过才 stop；同时取消从最新 user 文本推断 Write/Edit/Bash/Task/ToolSearch 参数，只保留 `SendMessage.summary` 这种确定性窄修复。本地 WSL 原生 `fmt/clippy/test` 通过：lib/main 114 条、kernel golden 112 条；panda 三实例健康，ZenProxy/NewAPI OpenAI+Anthropic PONG smoke 通过，forced Bash tool stream 输出完整 `tool_use` 增量流。

## Next

1. 部署 V4.105 并验收：确认 `buffer_reason` 日志出现、普通 ClaudeCode 长会话不因宽泛 exact-output 进入 buffered、DeepSeek `prompt_cache_hit_tokens/prompt_cache_miss_tokens` 能透传为 cache usage、cache hit 分桶报表能对齐 cc-switch/NewAPI/ZenProxy。
2. 按 `docs/06-panda-pressure-test-plan.md` 执行 policy-smoke / policy-dry；任一 policy gate 失败都不进入四客户端 dry/full，尤其要确认 panda 上 flash/free 没有输入墙、输出墙或隐藏 compactor。
3. 用真实 ClaudeCode 长会话观察 V4.104/V4.105：`first_tool_call_ms` 与 NewAPI FRT 是否靠近、`first_tool_emit_ms` 长尾是否不再阻塞首字、`anthropic_buffered` 是否只在窄场景出现，以及 `Invalid tool parameters`、`summary is required when message is a string`、`provider_missing_reasoning_content`、重复 `Read/Edit/Bash`、`Agent` 初始化卡住和输出格式是否回归。
4. 继续补强 Windows ClaudeCode 和 WSL ClaudeCode 测试执行环境，避免从 WSL 非交互环境或 clawgod launcher 误报 `config_error`。
5. policy 通过后按 `docs/06-panda-pressure-test-plan.md` 执行四客户端 dry run，再决定是否进入 full run：Windows ClaudeCode、WSL ClaudeCode、WSL Hermes、WSL OpenClaw。
6. 加入更细粒度运行指标采集：protocol first byte、first content、first tool call、first downstream、upstream connect、upstream status、stream parse error、empty upstream。
7. 补足 API 覆盖文档：OpenAI、Anthropic、Models、Health、错误响应、认证头、请求体限制。
8. 如果代码继续变化，保持根 README 和维护文档同步。
9. NewAPI 使用日志导出器下一步是生产收紧：专用只读 DB 用户、审计日志、NewAPI `type` 精确错误分类、可选日趋势统计。

## Later

1. 与 ZenProxyRS 合包部署时，明确本仓库是 kernel、sidecar 还是 library crate。
2. 如果进入高并发生产，补 lane 调度、全局预算、节点分桶和 Redis/共享状态设计；本仓库当前没有完整池调度。
3. 长上下文 compactor 如要下沉到本仓库，需要单独设计语义保真测试，不能只按字节裁剪。
4. 建立 24h panda 稳定性验收：成功率、错误率、TTFT 分位、first_content 分位、工具调用成功率、短请求隔离。
