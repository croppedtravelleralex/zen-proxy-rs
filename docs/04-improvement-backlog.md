# 改进 Backlog

## P0：必须优先处理

### panda NewAPI 真实链路验收

- 状态：已完成小矩阵。
- 原因：代码层测试通过，但用户当前关注的是 panda NewAPI 实际可用性。
- 验收结果：`/v1/models`、OpenAI `/v1/chat/completions`、Anthropic `/v1/messages` 均返回 200；响应摘要已写入 `docs/02-current-state.md`。
- 2026-06-04 更新：最新 V4.6 部署后，`/v1/models` 200、OpenAI 非流式 200、Anthropic 流式 200；Anthropic 极短非流式探针仍可能因上游持续空输出返回 502，需要单独补探针兜底，不影响真实流式小请求链路。
- 2026-06-04 最新残留：输出限制和 non-stream output guard 已取消，但真实 panda `policy-smoke/policy-dry` 尚未跑；不能把该策略写成生产已验证。
- 残留：尚未做 policy-smoke/policy-dry，也尚未做 4 客户端 x 500 次 panda-only 正式压测。

### Hermes 接入 panda NewAPI

- 状态：已完成小矩阵，保留 web 观察点。
- 已知：Hermes CLI 存在，历史版本为 `Hermes Agent v0.14.0`。
- 已执行：使用临时环境变量 `CUSTOM_BASE_URL=http://100.69.228.93:8081/v1` 和脱敏 key 方式测试，没有永久修改用户默认配置。
- 验收结果：短回复 `PONG` 通过；文件/终端工具写读通过；web 用例命令成功但返回 `WEB_FAIL`。
- 残留：正式压测前需要固定 Hermes web/search 的明确断言，避免把模型判断失败误算为链路失败。

### OpenClaw Node 运行环境

- 状态：Node 运行时已解决；OpenClaw agent/gateway 仍有阻塞。
- 已知：OpenClaw package 要求 Node `>=22.19.0`，当前系统 WSL Node 仍为 `v20.20.2`。
- 已执行：隔离安装 Node `v22.21.1` 到 `~/.local/opt/node-v22.21.1-linux-x64`，只在 OpenClaw 测试命令里 prepend PATH。
- 验收结果：`openclaw --help`、临时 config validate、models list、infer PONG、agent PONG、agent 文件工具、agent web_fetch 均通过 panda NewAPI。
- 最新结果：2026-06-04 smoke 中 OpenClaw API 5/5 通，但语义 0/5，输出固定 `HEARTBEAT_OK`，stderr 有 local secrets gateway `1006 abnormal closure`。
- 残留：如果后续直接运行 `openclaw` 而不显式使用隔离 Node，仍会命中系统 Node 20 并失败；即使 Node 22 正确，仍需修复 OpenClaw local gateway/agent harness，否则不能把 OpenClaw 纳入正式语义压测。

## P1：稳定性与观测

### 客户端识别与策略隔离

- 状态：90 分方案和代码均已落地；ClaudeCode/Hermes 当前路径可用，OpenClaw 需要先修本地 agent/gateway。
- 原因：Hermes/OpenClaw 适配目前通过共享路径生效，可能误伤 ClaudeCode 的 thinking、流式输出格式和工具历史语义。
- 方案入口：`docs/07-client-profile-policy-plan.md`。
- 目标：先用 `x-fmc-client` 和自动识别拆出 `claude-code`、`hermes`、`openclaw`、`unknown` 等 profile；按 profile 应用不同 thinking、空白保留、工具历史修复策略。
- 90 分验收：ClaudeCode tools 请求不再默认禁用 thinking；流式空格/换行/缩进不丢；Hermes/OpenClaw 小矩阵不回退；profile 维度测试通过。
- 已完成：`src/client_profile.rs`、`x-fmc-client`、OpenAI/Anthropic chat profile 传递、per-client thinking/whitespace/tool-history policy、kernel golden 回归测试。
- 2026-06-04 追加：已补模型维度有效策略，真实 `source_client` 继续进日志，但行为策略按模型收窄：`deepseek-v4-flash/deepseek-v4-flash-free` 只保留 ClaudeCode 深度适配，取消 Hermes/OpenClaw 适配并取消输入 token 墙；`deepseek-v4-flash-lite/big-pickle` 只保留 Hermes/OpenClaw 适配，取消 ClaudeCode 适配。
- 2026-06-04 外层同步：`zen-proxy-rs` V4 context compactor 也已按模型分流，flash/free 大输入只记录 `warn/pass`，不 compact、不按 token target reject；lite 仍作为 compactor 保护路径。
- 已验证：2026-06-04 Windows ClaudeCode 显式 panda NewAPI 5/5，Hermes 5/5；OpenClaw API 5/5 但语义 0/5，固定 `HEARTBEAT_OK`。
- 待完成：dry-run 级别 profile 维度运行数据、Hermes 慢路径拆解、OpenClaw local gateway/harness 修复、WSL ClaudeCode CLI 修复。
- 99+ 后续：拿真实数据后做动态 profile、per-client 指标、灰度和回滚。

### 运行指标细分

- 状态：待设计。
- 当前不足：代码层有错误结构化，但缺少完整阶段耗时暴露。
- 建议指标：请求入站、认证、解析、协议修复、上游连接、上游首包、first content、first tool call、stream decode、响应结束。

### 输出限制取消后的 panda 压测闸口

- 状态：源码/ZenProxy 侧策略已调整，真实 panda policy-smoke/policy-dry 未跑。
- 当前事实：
  - 缺省 `max_tokens` 不再补 1024/2048。
  - 显式 `max_tokens` 原样透传。
  - OpenAI/Anthropic 只有客户端显式传值时才向上游写 `max_tokens`。
  - ZenProxy 侧 non-stream output guard 已取消。
  - ZenProxy 侧 context compactor 对 `deepseek-v4-flash/deepseek-v4-flash-free` 已改为只观测/告警，不再压缩或按 token target 拒绝；`deepseek-v4-flash-lite/big-pickle` 仍保留压缩保护。
- 风险：
  - 上游 413、provider timeout、空输出、长尾延迟和成本风险不再由本仓库输出 cap 吸收。
  - flash/free 大输入现在可能真实打到 upstream；如果 upstream 无法承受，需要靠 panda policy 数据决定 lane/pool 隔离，而不是偷偷恢复输入墙。
  - 风险会回到 upstream、NewAPI、ZenProxy lane/pool 调度和客户端超时边界。
- 待办：
  1. 先跑 `policy-smoke`，确认 input/output wall、provider usage/header/body 信号和 cache 四态都有记录。
  2. 再跑 `policy-dry`，重点看大输出、无显式 `max_tokens`、长输入和 cache probe 是否出现 413/超时/空输出，并确认 flash/free 日志没有 `context_action=compact`。
  3. policy 失败时先做 lane/pool 或 case 隔离，不直接回滚成无证据的全局输出墙。
  4. policy 通过前，不启动四客户端 full run。

### ClaudeCode 输出被截断的责任层

- 状态：旧截断/finish_reason 问题已定位并修复；最新输出限制已完全取消，待真实 panda policy 和 ClaudeCode 长会话复验体感。
- 当前事实：
  - ClaudeCode 看到的 `… +N lines (ctrl+o to expand)` 是客户端对长工具输出的折叠展示，但它不能解释用户描述的“3 段只出 1.5 段”。
  - 近期 panda 样本显示，工程请求不全是 `>=50k` 大上下文；很多是 26k-40k prompt tokens，却只有 100-700 completion tokens。
  - 同窗口存在 `prompt=11906/completion=6985` 的长输出成功样本，证明链路和模型并非整体不能长输出。
  - 源码之前会吞掉上游 `finish_reason`，即使上游是 `length`，OpenAI 也会被写成 `stop`，Anthropic/ClaudeCode 也会被写成 `end_turn`。
  - 多条 ClaudeCode 工程请求的 `last_user_tokens=3`，同时存在 26k 级旧工具输出，说明当前任务指令被旧工具历史淹没；这类中等上下文没达到旧 compactor 的 80k 阈值。
  - 历史版本存在 `>=50k/100k/200k` 的流式输出 cap，会把显式大输出请求降到 1024/768/512；最新策略已经完全取消输出限制，缺省不补 `max_tokens`，显式值原样透传。
- 已完成：
  1. 透传上游 `finish_reason`，Anthropic 将 `length` 映射为 `max_tokens`。
  2. 增加 ClaudeCode 中等工具历史压缩，覆盖 `last_user_tokens` 很短、旧工具输出很大的场景。
  3. 新增 5 条回归：OpenAI/Anthropic stream/non-stream 的 `length` 透传，以及中等工具历史折叠。
  4. 完全取消输出限制：缺省 `max_tokens` 不自动补值，显式值不改写。
  5. 收窄 ClaudeCode Anthropic buffered stream：短输出和 exact-output 仍可 retry/fallback，20k/32k 长输出直接流式返回，降低首字和内存压力。
- 风险：
  - 用户会感知为回答短、收尾早、工具后总结不完整。
  - 输出限制取消后，空输出、上游 413、长尾延迟和成本风险需要靠 upstream 质量、lane/pool 调度和真实 panda 压测兜住。
- 待办：
  1. 用同一类 ClaudeCode 工程会话复验 completion tokens、stop_reason 和终端体感。
  2. 若仍短，再查真实上游是否稳定返回 `length/max_tokens`，并确认是上游截断、客户端展示截断还是模型自主停笔。
  3. 同时补脱敏观测：记录 `requested_max_tokens/effective_max_tokens/prompt_tokens_bucket/source_client/upstream_finish_reason`，避免继续把该问题误判为 NewAPI/CLI 渲染 bug。

### NewAPI cache 可见性

- 状态：cache usage 透传已在源码和 15:33 panda release 中落地；四态 cache 观测和 provider usage 信号已在 harness/观测侧补齐，待真实 panda policy-smoke/policy-dry 生成样本。
- 当前事实：
  - 2026-06-04 前，OpenAI 非流式正文/工具调用响应没有透传 `cache_*`；Anthropic 非流式正文/工具调用响应把 `cache_creation_input_tokens/cache_read_input_tokens` 固定写成 `0`。
  - 因此即使上游 usage 里有 cache 字段，NewAPI 也可能在非流式请求上完全看不到。
- 已完成：
  1. 非流式 OpenAI 正文/工具调用响应保留 `prompt_tokens_details.cached_tokens`、`cache_creation_input_tokens`、`cache_read_input_tokens`。
  2. 非流式 Anthropic 正文/工具调用响应保留真实 `cache_*`。
  3. 新增 kernel golden 回归覆盖 OpenAI/Anthropic 非流式正文和工具调用四种路径。
  4. cache 观测分类为 `attempted`、`accepted`、`rejected`、`ignored`，不伪造 cache 命中。
  5. policy harness 记录 provider response/header/body usage 信号，用来拆分上游返回、header 透传和 body usage 展示。
- 待办：
  1. 用真实 panda `policy-smoke/policy-dry` 调用记录确认 cache 四态分布。
  2. 若 body usage 有值但 NewAPI 展示仍不显示，再排查 NewAPI 自身对 OpenAI/Anthropic usage 字段的解析/展示层。
  3. 若 header usage 有值但 provider header 在 NewAPI 后消失，报告中必须写成“中间层剥离/未透传”，不能写成上游无 cache。

### ClaudeCode 大流式空 assistant 500

- 状态：已修复并部署，仍需线上观察。
- 现象：panda NewAPI channel 69 偶发 `status_code=500, upstream returned no assistant content or tool call`。
- 已确认事实：
  - 错误字符串来自 `free-model-client-rs` 的空上游保护，不是 NewAPI 自造错误。
  - 2026-06-04 近 4 小时内 `deepseek-v4-flash` 成功消费 133 条，该错误实际事件 4 次，发生在 10:43、10:58、11:03、11:04。
  - 失败样本主要是 ClaudeCode Anthropic `/v1/messages` 流式请求，`source_client=ClaudeCode`，`stream=true`，大上下文加工具 schema，`tools_tokens` 约 12.7k-12.9k。
  - 历史失败样本里，上游请求前会把 `max_tokens=32000` cap 到 768 或 1024；旧版 ClaudeCode huge buffered retry 只覆盖 `max_tokens <= 512`，所以这些请求绕过 buffered retry，普通流式分支在收到空正文/空工具调用后直接向客户端发 error。
  - 同一回合后续 NewAPI/客户端常发非流式 fallback，并有成功样本：约 48k-52k prompt tokens，905-1789 completion tokens。
- 已完成：
  1. 移除 `handle_stream` 内部多余的 `max_tokens <= 512` 二次门槛。
  2. 新增历史 1024 cap 桶回归：大 ClaudeCode 流式请求第一次上游空输出，第二次 buffered retry 成功；最新输出限制已完全取消，该回归主要防止旧路径回归。
  3. 2026-06-04 已部署到 panda stripped hash `7a8f4e5dc99e8ccf1aaf6562519d8353dc4ba5205e5e55f521c265b0760ed66e`。
- 风险：
  - 单次流式请求失败会被用户感知为无回复、卡顿或重试。
  - fallback 会拉长耗时，并可能继续放大长会话上下文。
- 待办：
  1. 观察新 hash 之后是否还出现同类空 assistant 500；如仍有，确认是否 buffered retry 三次都为空。
  2. 普通流式空输出分支补结构化日志：`source_client/prompt_hash/prompt_tokens/message_count/max_tokens/tool_count/tools_tokens`。
  3. 所有 buffered retry 为空时返回清晰分类；普通用户请求仍不得伪造答案。
  4. 部署后用 panda NewAPI 验收：同类错误事件应归零，短请求 P90 不被大 buffered retry 拖慢。

### Web search / web_fetch 能力边界

- 状态：源头已拆分验证；客户端执行器仍需分别处理。
- 当前事实：
  - DeepSeek/OpenAI-compatible chat 模型可以通过 function/tool calling 请求工具，但搜索动作本身不是模型内置能力；真正联网搜索必须由 ClaudeCode/Hermes/OpenClaw/MCP 或后端工具执行。
  - 本仓库只负责把工具定义、工具调用和工具结果在 OpenAI/Anthropic 协议间规范化转发，不自带搜索引擎、不自发访问公网。
  - OpenClaw panda 小矩阵中 `web_fetch` 曾通过；Hermes web 用例命令成功但返回 `WEB_FAIL`，需要独立归因。
  - `web_fetch/web_search` 不能作为 OpenClaw 强身份信号；该识别误伤已修复，避免 ClaudeCode 因带 web 工具而套用 OpenClaw/Hermes 策略。
  - 2026-06-04 清空 WSL proxy env 后，直连 panda NewAPI 的 Anthropic `/v1/messages` 和 OpenAI `/v1/chat/completions` 均能返回 `web_search` tool call。
  - 用户后续截图证明：Windows ClaudeCode 在官方 Claude 模型路径下可以真实执行 `WebSearch/WebFetch`。此前受控样本只能说明当时 ZenProxy 路径没有形成可执行 tool_use，不能说明 ClaudeCode 本身不支持 web 工具。
  - 2026-06-04 源码修复：free-model-client-rs 会把上游返回的 `web_search/task` 等工具名按原始请求工具表 canonicalize 回 `WebSearch/Task`，避免 ClaudeCode 因名称不匹配而不执行工具或 subagent。
  - OpenClaw 请求能带 `web_fetch/web_search` tool schema，但当前 OpenClaw agent 输出固定 `HEARTBEAT_OK`，不是 ZenProxy web 转发问题。
- 可能原因：
  1. 客户端没有把 web 工具定义传进请求，模型就无法调用。
  2. 模型收到工具定义但没有选择 tool call，属于模型工具服从问题。
  3. 模型发起 tool call，但工具名大小写/别名与客户端注册名不一致，例如 `web_search` vs `WebSearch`、`task` vs `Task`。
  4. 模型发起 tool call，但客户端/工具执行器没有真正联网或返回失败。
  5. 工具结果返回后被协议转换、compactor 或工具历史修复误处理，导致模型看不到搜索结果。
- 待办：
  1. ClaudeCode 若需要真实 WebSearch，先确认请求里存在 `WebSearch/WebFetch` 工具定义，再确认响应里返回的是同名 `tool_use`；不能指望 ZenProxy 自行执行搜索。
  2. Hermes 的 `WEB_FAIL` 不再直接算链路失败，必须标注为“未触发工具 / 工具执行失败 / 工具结果未被使用 / 模型判断错误”之一。
  3. OpenClaw 先修 local gateway/harness，再重新验证 `web_fetch/web_search` 工具执行。
  4. 保持 ZenProxy 脱敏 `tool_name_classes` 观测，不记录原始查询内容。
  5. 若用户需要“模型自带联网搜索”，不能只靠 `deepseek-v4-flash-free`；需要接入带搜索执行器的客户端/MCP 或在 ZenProxy 外侧增加受控搜索工具服务。

### ClaudeCode 请求体来源归因

- 状态：第二阶段已在 `free-model-client-rs` 源码落地，并已随 `zen-proxy-rs` 部署到 panda。
- 背景：2026-06-03 panda 日志显示 ClaudeCode 表面短 prompt 也可能产生 291KB-474KB 的 Anthropic `/v1/messages` 请求体；NewAPI/cc-switch 使用日志常见 40k-90k input tokens。
- 已确认事实：
  - ZenProxy `body_size` 是 HTTP JSON 字节数，不是 tokens。
  - 21:44 的 ClaudeCode 请求 `body_size=472161/474175`，`context_action=pass`，未触发 ZenProxy 外层大体积 compactor。
  - 历史版本里，free-model-client-rs 对这两条只做了流式输出 cap：`prompt_tokens=72826/73017`，`max_tokens=32000 -> 1024`；最新策略已取消输出限制，不能把该句当作当前行为。
  - Windows ClaudeCode 当前 `ANTHROPIC_BASE_URL=http://127.0.0.1:15721`，实际先走 cc-switch；Windows 设置启用 `CLAUDE_CODE_EFFORT_LEVEL=max`、agent teams、tool search 和多个插件。
  - cc-switch 最近 Claude 日志的 provider 为 `closedeepseek -> https://sub2api.closeapi.top`，不是 `LocalNewapi -> http://127.0.0.1:4000/v1`；因此 Windows ClaudeCode 最近使用记录和 panda NewAPI channel 69 记录不能直接混为同一条链路。
  - 2026-06-03 23:01-23:46 panda channel 69 真实 ClaudeCode 请求中，流式 body 从约 674KB 增长到 788KB，`message_count` 从 600 多增长到 705，`last_user_tokens` 多数只有 36-96。
  - 同一窗口 NewAPI channel 69 共 61 条：57 条流式、4 条非流式；53 条 prompt tokens 落在 70k-90k，2 条非流式超过 200k，2 条流式 prompt tokens 记 0 且内容为 `upstream returned no assistant content or tool call`。
  - `prompt_tokens>=200k` 的两条是非流式大请求/fallback：id `109370` 为 213248 prompt tokens，id `109461` 为 225416 prompt tokens。对应当时 ZenProxy 非流式路径只做输出 cap，未做输入 compactor；最新策略已改为输出不设墙、flash 输入只观测不压缩。
  - ClaudeCode huge-context 流式 compactor 的 `target_tokens=12k` 没在真实会话达到，根因不是 profile 误识别，而是 700+ 旧短消息和工具 schema 残留；当前 compactor 主要处理单条大文本，短轮次历史不进候选。
- 已完成：
  1. `src/protocol/translate.rs` 新增 `RequestShape`，只记录 token/数量/hash，不保存原始 prompt、请求体或 key。
  2. OpenAI/Anthropic 入口统一输出脱敏字段：`system_tokens/messages_tokens/tools_tokens/tool_count/message_count/largest_message_tokens/last_user_tokens/estimated_total_tokens/stream/max_tokens/tool_choice_present/prompt_hash/source_client/profile_source`。
  3. shape 单元测试覆盖“不泄露原文”和“工具 schema 计入 tools_tokens”。
  4. 新增 ClaudeCode huge-session compactor：折叠旧短轮次历史，保留系统消息、最近 48 条消息、最新用户目标和少量脱敏状态信号。
  5. 历史上 Anthropic/OpenAI 两个入口曾接入 ClaudeCode huge-session compactor，用于避免大非流式 fallback 只 cap 输出；最新 flash 策略已取消输入墙，只观测不压缩。
  6. 非流式输出 cap 的压缩前口径属于历史保护逻辑；最新输出限制已完全取消，后续用 policy harness 观察真实上游风险。
- 待办：
  1. 记录并统计 `stream empty -> non-stream fallback` 链路，避免上游一次空流导致 ClaudeCode 把同一大历史重发并继续膨胀。
  2. 继续观察真实 ClaudeCode/Hermes/OpenClaw 请求，确认 shape 字段在长时间运行中可被稳定采集。
  3. 如仍需要更细拆分，再补 `claude_code_shape` 二级指标，进一步拆插件/skills、历史消息和最后用户消息占比。
  4. 对 Windows cc-switch 链路单独建验收入口：`ClaudeCode -> cc-switch(15721) -> provider`，不要把它和 panda NewAPI channel 69 直接合并统计。
  5. 重新跑 Windows ClaudeCode raw/CLI 对照时，必须从 Windows 本地目录启动，不能从 `\\wsl.localhost` UNC cwd 启动。
  6. 用真实 ClaudeCode 长会话复测：导出/扫码/等待外部动作这类任务不得再反复提交任务、反复重启或杀掉已登录进程。

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
  5. 2026-06-04 新增显式 smoke 探针兜底：`strict smoke`、`reply PASS`、`answer OK` 等无工具短测在上游空输出后返回本地测试文本；普通 ClaudeCode 短输入仍不兜底。
- 待办：
  1. 继续用真实小非流式样本确认分类是否稳定命中 `internal_claude_code_probe`，并记录最终是否 retry 成功。
  2. 只有确认是 ClaudeCode 内部探测后，才评估本地安全 fallback 或短冷却；当前源码没有新增普通请求短路。
  3. 如日志仍无法区分用途，再补不含原文的 `last_user_prefix_class`。
  4. Windows ClaudeCode/cc-switch 若访问 panda Tailscale IP，需确认进程是否继承 `HTTP_PROXY=http://127.0.0.1:7897`；若继承，必须为 `100.69.228.93` 配置 no-proxy，否则 Windows HTTP 客户端可能走代理返回 502。

### V4.99 reasoning-only 空输出保护

- 状态：源码已落地，本地验证通过；2026-06-05 10:47 已部署 panda，最小 NewAPI smoke 通过，仍需长窗口生产观察和 policy-smoke/policy-dry。
- 背景：V4.98 后 cache 前缀观测正常，但短/中非流式和低输出预算请求仍可能遇到上游只返回 `reasoning_content`、正文为空，最终被判为 `upstream returned no assistant content or tool call`。
- 已完成：
  1. 新增共享输出分类：`valid/empty_output/reasoning_only/reasoning_only_length`。
  2. OpenAI/Anthropic 非流式遇到 `reasoning_only_length` 时只重试一次 `thinking: disabled`，不全局禁用 thinking。
  3. 大流式 ClaudeCode 主会话、工具请求和长上下文仍保留默认 thinking 策略；低预算探针/ClaudeCode 小流式才做初始 disabled。
  4. Anthropic ClaudeCode buffered stream 触发条件收窄：不再仅因 `max_tokens<=512` 进入 huge buffered。
  5. 空输出错误和日志增加 `class=`、`reasoning_chars/content_chars/finish_reason/tool_call_count/short_request_kind`。
  6. 新增 golden tests：OpenAI/Anthropic 非流式 reasoning-only-length disabled retry、小流式低预算不走 buffered retry、普通小非流式非探针仍不被本地 ok 误短路。
- 待办：
  1. 继续观察 NewAPI 短非流式/小流式是否还出现高发 `reasoning_only_length` 502；若出现，确认是否 disabled retry 后仍空。
  2. 用真实 ClaudeCode 长会话确认大流式主会话没有被 `thinking: disabled` 误伤，Task/subagent 和 Markdown 格式不回退。
  3. 观察 `class=empty_output` 是否仍高发；若高发且不是 reasoning-only，再回到节点质量、上游空输出或 ZenProxy lane/pool 排查。
  4. 如后续需要 99+ 观测，把本仓库分类同步到 ZenProxy `/metrics` 或新增轻量诊断出口。

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

- 状态：历史同步已完成；本轮只改 `docs/`，根 README 尚未同步最新“输出限制完全取消”事实。
- 已改历史：根目录 README 曾补 `FREE_MODEL_REQUEST_BODY_LIMIT_MB`、`ZEN_UPSTREAM_SESSION_TTL_SECS`、非流式输出保护、空上游错误行为、脱敏 request-shape 日志说明，并把测试数量更新到当时版本。
- 残留：后续允许改 README 时，需要删除旧输出保护表述，改成“缺省 `max_tokens` 不自动补值、显式值原样透传、真实 panda policy-smoke/policy-dry 待跑”。

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
- 注意：当前 `deepseek-v4-flash/deepseek-v4-flash-free` 已取消输入 token 墙，`free-model-client-rs` 侧只观测不压缩。若未来重新引入 compactor，必须先做语义保真设计，保护最后用户目标、最近错误、工具结果摘要、文件路径、subagent 指令和验收标准。

### V4.98 cache 命中优化

- 状态：源码已落地并部署 panda，长会话 cache A/B 待执行。
- 背景：真实 ClaudeCode 长会话已达到约 330k prompt tokens，NewAPI 总耗时爆红；部署后日志显示 flash/free 没有输入墙或 compactor，当前瓶颈更像是长输入每轮未命中 provider cache。
- 线上事实：
  - 最近 70 分钟同一长会话 `/v1/messages` 77/77 流式，cache hits 0，prompt P50 约 326k、P90 约 331k。
  - 8 小时窗口内仍存在大 cache 命中样本，说明 provider/NewAPI 并非完全不记录 cache。
  - 完全相同 `prompt_hash` 的重试样本能命中 cache；尾部不断增长的长会话基本不命中。
- 已做：
  1. 大请求上游 session 从完整 `messages` hash 改为稳定前缀 hash + tools hash + tool_choice hash。
  2. 新增 `prefix_4k_hash/prefix_32k_hash/prefix_128k_hash/prefix_256k_hash/cache_material_bytes` 脱敏观测。
  3. 增加回归：大前缀稳定、只追加尾部时，session 和 prefix hash 保持稳定；前缀变化时 prefix hash 必须变化。
  4. 2026-06-05 已部署 panda 三实例，线上 stripped SHA256 为 `566e1c519056a4d2ee95697803d0e8bff9db40dc706c81ab753d70405edfb224`，备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-v498-20260605-091813-9942460`。
- 非目标：
  - 不降低 330k 输入，不做摘要替换，不裁剪工具历史，不改用户消息顺序。
  - 不注入隐藏提示词，不伪造 `cache_tokens`，不把 NewAPI 显示问题误写成真实 cache 命中。
- 待验收：
  1. 用同一 ClaudeCode 长会话做 A/B：V47/V4.98 或部署前后窗口比较。
  2. 观察 `cache_tokens/cache_observation/frt/use_time/client_gone/empty output/tool errors`。
  3. 如果 prefix hash 稳定但 cache 仍为 0，再检查上游是否按 session、代理节点、账号或 body prefix 粒度隔离 cache。
  4. NewAPI 真实短问答仍偶发/可复现 `upstream returned no assistant content or tool call`；V47 备份临时实例同 prompt 也失败，当前按既有上游空输出/节点质量问题继续排查，不作为 V4.98 回滚条件。
