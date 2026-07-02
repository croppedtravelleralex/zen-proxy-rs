# Roadmap

## Now

1. 当前暂停继续测试和修复；先以 `docs/reports/2026-06-13-claudecode-web-tool-handoff.md` 作为接手入口。
2. 若恢复工作，第一步不是改代码，而是重新确认当前 Windows ClaudeCode/cc-switch provider 是否仍为 `closedeepseek -> https://sub2api.closeapi.top`，还是已切回 panda NewAPI/channel 69。
3. WebSearch/WebFetch 后续排查必须先分清 ClaudeCode 内置 Web 工具、MCP/Playwright 工具、模型服务商原生搜索三条能力；本轮已确认 WebFetch 参数完整但安全验证链路失败。
4. `Failed to parse JSON` 后续排查必须先拿同一时间窗口的 ClaudeCode stream-json、cc-switch request log、panda NewAPI log、ZenProxy journal，不要凭终端报错直接归因。
5. 继续保留 V4.105/V4.106/V4.107/V4.110 的线上观察目标：cache hit、真实首字、工具参数完整率、reasoning-only/no-forwardable 重试、连接层 502/524。
6. 清理或归类未跟踪文件，尤其是 `north-mini-code`、`.codex_tmp/`、密钥、测试输出；默认不提交、不盲删。
7. 如果代码继续变化，保持根 README、维护文档、日志和真实部署状态同步。

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

1. 恢复工作前先重跑最小事实确认：`git status`、当前 cc-switch provider、panda channel 69 状态、ZenProxy `/health`、ClaudeCode 官方/日常入口版本。
2. 验证 V4.106 中等上下文 session 优化的线上效果：比较部署后 15-30 分钟和更长窗口的 cc-switch 口径 cache hit、10k-50k 输入桶命中率、真实首字和工具质量；不得用部署后前 5 分钟 warm-up 样本下结论，不得用降质换命中率。
2. 观察 V4.105 线上效果：确认 `buffer_reason` 日志只出现在窄场景、普通 ClaudeCode 长会话不因宽泛 exact-output 进入 buffered、DeepSeek `prompt_cache_hit_tokens/prompt_cache_miss_tokens` 能透传为 cache usage、cache hit 分桶报表能对齐 cc-switch/NewAPI/ZenProxy。
3. 按 `docs/06-panda-pressure-test-plan.md` 执行 policy-smoke / policy-dry；任一 policy gate 失败都不进入四客户端 dry/full，尤其要确认 panda 上 flash/free 没有输入墙、输出墙或隐藏 compactor。
4. 用真实 ClaudeCode 长会话观察 V4.104/V4.105/V4.106：`first_tool_call_ms` 与 NewAPI FRT 是否靠近、`first_tool_emit_ms` 长尾是否不再阻塞首字、`anthropic_buffered` 是否只在窄场景出现，以及 `Invalid tool parameters`、`summary is required when message is a string`、`provider_missing_reasoning_content`、重复 `Read/Edit/Bash`、`Agent` 初始化卡住和输出格式是否回归。
5. 继续补强 Windows ClaudeCode 和 WSL ClaudeCode 测试执行环境，避免从 WSL 非交互环境或 clawgod launcher 误报 `config_error`。
6. policy 通过后按 `docs/06-panda-pressure-test-plan.md` 执行四客户端 dry run，再决定是否进入 full run：Windows ClaudeCode、WSL ClaudeCode、WSL Hermes、WSL OpenClaw。
7. 加入更细粒度运行指标采集：protocol first byte、first content、first tool call、first downstream、upstream connect、upstream status、stream parse error、empty upstream。
8. 补足 API 覆盖文档：OpenAI、Anthropic、Models、Health、错误响应、认证头、请求体限制。
9. 如果代码继续变化，保持根 README 和维护文档同步。
10. NewAPI 使用日志导出器下一步是生产收紧：专用只读 DB 用户、审计日志、NewAPI `type` 精确错误分类、可选日趋势统计。

## Later

1. 与 ZenProxyRS 合包部署时，明确本仓库是 kernel、sidecar 还是 library crate。
2. 如果进入高并发生产，补 lane 调度、全局预算、节点分桶和 Redis/共享状态设计；本仓库当前没有完整池调度。
3. 长上下文 compactor 如要下沉到本仓库，需要单独设计语义保真测试，不能只按字节裁剪。
4. 建立 24h panda 稳定性验收：成功率、错误率、TTFT 分位、first_content 分位、工具调用成功率、短请求隔离。
