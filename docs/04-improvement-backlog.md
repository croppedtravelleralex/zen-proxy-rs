# 改进 Backlog

## P0：必须优先处理

### panda NewAPI 真实链路验收

- 状态：已完成小矩阵。
- 原因：代码层测试通过，但用户当前关注的是 panda NewAPI 实际可用性。
- 验收结果：`/v1/models`、OpenAI `/v1/chat/completions`、Anthropic `/v1/messages` 均返回 200；响应摘要已写入 `docs/02-current-state.md`。
- 残留：尚未做 4 客户端 x 500 次 panda-only 正式压测。

### Hermes 接入 panda NewAPI

- 状态：已完成小矩阵，保留 web 观察点。
- 已知：Hermes CLI 存在，历史版本为 `Hermes Agent v0.14.0`。
- 已执行：使用临时环境变量 `CUSTOM_BASE_URL=http://100.69.228.93:8081/v1` 和脱敏 key 方式测试，没有永久修改用户默认配置。
- 验收结果：短回复 `PONG` 通过；文件/终端工具写读通过；web 用例命令成功但返回 `WEB_FAIL`。
- 残留：正式压测前需要固定 Hermes web/search 的明确断言，避免把模型判断失败误算为链路失败。

### OpenClaw Node 运行环境

- 状态：已完成小矩阵。
- 已知：OpenClaw package 要求 Node `>=22.19.0`，当前系统 WSL Node 仍为 `v20.20.2`。
- 已执行：隔离安装 Node `v22.21.1` 到 `~/.local/opt/node-v22.21.1-linux-x64`，只在 OpenClaw 测试命令里 prepend PATH。
- 验收结果：`openclaw --help`、临时 config validate、models list、infer PONG、agent PONG、agent 文件工具、agent web_fetch 均通过 panda NewAPI。
- 残留：如果后续直接运行 `openclaw` 而不显式使用隔离 Node，仍会命中系统 Node 20 并失败。

## P1：稳定性与观测

### 客户端识别与策略隔离

- 状态：90 分方案和代码均已落地；OpenClaw body/profile 修复已部署到 panda，WSL 三客户端 smoke 已通过。
- 原因：Hermes/OpenClaw 适配目前通过共享路径生效，可能误伤 ClaudeCode 的 thinking、流式输出格式和工具历史语义。
- 方案入口：`docs/07-client-profile-policy-plan.md`。
- 目标：先用 `x-fmc-client` 和自动识别拆出 `claude-code`、`hermes`、`openclaw`、`unknown` 等 profile；按 profile 应用不同 thinking、空白保留、工具历史修复策略。
- 90 分验收：ClaudeCode tools 请求不再默认禁用 thinking；流式空格/换行/缩进不丢；Hermes/OpenClaw 小矩阵不回退；profile 维度测试通过。
- 已完成：`src/client_profile.rs`、`x-fmc-client`、OpenAI/Anthropic chat profile 传递、per-client thinking/whitespace/tool-history policy、kernel golden 回归测试。
- 已验证：OpenClaw-only smoke 5/5；WSL ClaudeCode/Hermes/OpenClaw smoke 15/15；OpenClaw subagent 用例从历史 328s timeout 降到约 20.1s 成功。
- 待完成：dry-run 级别 profile 维度运行数据、Hermes 慢路径拆解、Windows ClaudeCode 原生执行验证。
- 99+ 后续：拿真实数据后做动态 profile、per-client 指标、灰度和回滚。

### 运行指标细分

- 状态：待设计。
- 当前不足：代码层有错误结构化，但缺少完整阶段耗时暴露。
- 建议指标：请求入站、认证、解析、协议修复、上游连接、上游首包、first content、first tool call、stream decode、响应结束。

### ClaudeCode 请求体来源归因

- 状态：第一阶段已在 `free-model-client-rs` 源码落地，并已随 `zen-proxy-rs` 部署到 panda。
- 背景：2026-06-03 panda 日志显示 ClaudeCode 表面短 prompt 也可能产生 291KB-474KB 的 Anthropic `/v1/messages` 请求体；NewAPI/cc-switch 使用日志常见 40k-90k input tokens。
- 已确认事实：
  - ZenProxy `body_size` 是 HTTP JSON 字节数，不是 tokens。
  - 21:44 的 ClaudeCode 请求 `body_size=472161/474175`，`context_action=pass`，未触发 ZenProxy 外层大体积 compactor。
  - free-model-client-rs 对这两条只做了流式输出 cap：`prompt_tokens=72826/73017`，`max_tokens=32000 -> 1024`。
  - Windows ClaudeCode 当前 `ANTHROPIC_BASE_URL=http://127.0.0.1:15721`，实际先走 cc-switch；Windows 设置启用 `CLAUDE_CODE_EFFORT_LEVEL=max`、agent teams、tool search 和多个插件。
  - cc-switch 最近 Claude 日志的 provider 为 `closedeepseek -> https://sub2api.closeapi.top`，不是 `LocalNewapi -> http://127.0.0.1:4000/v1`；因此 Windows ClaudeCode 最近使用记录和 panda NewAPI channel 69 记录不能直接混为同一条链路。
- 已完成：
  1. `src/protocol/translate.rs` 新增 `RequestShape`，只记录 token/数量/hash，不保存原始 prompt、请求体或 key。
  2. OpenAI/Anthropic 入口统一输出脱敏字段：`system_tokens/messages_tokens/tools_tokens/tool_count/message_count/largest_message_tokens/last_user_tokens/estimated_total_tokens/stream/max_tokens/tool_choice_present/prompt_hash/source_client/profile_source`。
  3. shape 单元测试覆盖“不泄露原文”和“工具 schema 计入 tools_tokens”。
- 待办：
  1. 继续观察真实 ClaudeCode/Hermes/OpenClaw 请求，确认 shape 字段在长时间运行中可被稳定采集。
  2. 如仍需要更细拆分，再补 `claude_code_shape` 二级指标，进一步拆插件/skills、历史消息和最后用户消息占比。
  3. 对 Windows cc-switch 链路单独建验收入口：`ClaudeCode -> cc-switch(15721) -> provider`，不要把它和 panda NewAPI channel 69 直接合并统计。
  4. 重新跑 Windows ClaudeCode raw/CLI 对照时，必须从 Windows 本地目录启动，不能从 `\\wsl.localhost` UNC cwd 启动。

### ClaudeCode 小非流式空输出请求分类

- 状态：第一阶段已在 `free-model-client-rs` 源码落地，并已随 `zen-proxy-rs` 部署到 panda。
- 背景：2026-06-03 21:28 panda 日志出现 `source_client=claude-code`、`stream=false`、`body_size=342` 的 `/v1/messages` 请求，随后多次 `non-stream upstream returned empty output; retrying`。
- 当前判断：该请求形态不像用户主对话，更像 ClaudeCode 内部非流式探测、摘要、标题、能力检查或小模型辅助请求；当前日志没有保存脱敏 request shape，无法确认具体用途。
- 已有保护：`hi/hello/test/echo hi` 类极短无工具 channel-test probe 在上游连续空输出后会返回本地 `ok`；普通请求仍返回结构化空输出错误。
- 已完成：
  1. 新增分类：`health_probe/channel_test/internal_claude_code_probe/user_short_request/unknown_short_nonstream/not_short_nonstream`。
  2. 非流式空输出 retry 日志增加 `short_request_kind/prompt_hash/prompt_tokens/message_count/max_tokens/source_client`。
  3. `echo hi` 仍是 channel-test probe；普通短请求不是 channel-test。
  4. 新增 kernel golden：ClaudeCode 小非流式、非探针、上游空输出时仍返回 `upstream returned no assistant content or tool call`，不会被本地 `ok` 误短路。
- 待办：
  1. 继续用真实小非流式样本确认分类是否稳定命中 `internal_claude_code_probe`，并记录最终是否 retry 成功。
  2. 只有确认是 ClaudeCode 内部探测后，才评估本地安全 fallback 或短冷却；当前源码没有新增普通请求短路。
  3. 如日志仍无法区分用途，再补不含原文的 `last_user_prefix_class`。

### 压测矩阵

- 状态：方案和报告模板已落地，执行器与真实压测待落地。
- 当前资料：`.codex_tmp/client-matrix` 有历史脚本和输出，但未清洗、未文档化、可能含敏感信息。
- 目标：只在 panda 侧执行，不再把本机 WSL NewAPI 当成生产链路。
- 客户端：Windows ClaudeCode、WSL ClaudeCode、WSL Hermes、WSL OpenClaw。
- 规模：每客户端 500 次混合压力测试。
- 指标：stream/non-stream、prompt tokens 桶、输出 tokens、TTFT、first_content、总耗时、错误分类、工具成功率、subagent/Task 成功率。
- 已落地：`docs/06-panda-pressure-test-plan.md` 记录执行阶段、采集字段、错误分类、通过门槛和报告模板。
- 建议：迁移脚本骨架到 `scripts/` 或 `tests/manual/`，结果默认输出到 `.codex_tmp/`，报告只写摘要和脱敏样本。

### README 同步

- 状态：已完成本轮同步。
- 已改：根目录 README 已补 `FREE_MODEL_REQUEST_BODY_LIMIT_MB`、`ZEN_UPSTREAM_SESSION_TTL_SECS`、非流式输出保护、空上游错误行为、脱敏 request-shape 日志说明，并把测试数量更新为库测试 69 条、kernel golden 71 条。
- 残留：后续代码继续变化时，需要同步 README 和维护文档。

### 临时产物归类

- 状态：已文档归类，未删除。
- 当前事实：`.codex_tmp/` 下有 `client-matrix`、Hermes/OpenClaw panda 临时配置和测试输出；根目录有 0 字节未跟踪文件 `configured`、`panda`，以及异常字符文件。
- 要求：默认不提交 `.codex_tmp/` 和任何可能含密钥或大输出的文件；未跟踪根目录文件不盲删，提交前单独确认来源或保持未跟踪。
- 已记录：归类表位于 `docs/02-current-state.md`。

## P2：架构后续

### ZenProxyRS 合包边界

- 状态：待决策。
- 选项：library crate、sidecar、kernel worker pool。
- 判断点：性能、部署复杂度、ZenProxyRS 生命周期管理、故障隔离、观测统一程度。

### 长上下文质量保护

- 状态：待设计。
- 注意：本仓库目前只有非流式输出 cap，不等于完整 compactor。若做 compactor，必须保护最后用户目标、最近错误、工具结果摘要、文件路径、subagent 指令和验收标准。
