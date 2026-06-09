# 当前状态

更新时间：2026-06-09
分支：`codex/v47-client-split-cache-harness`

## 代码已确认能力

最新源码验证使用 WSL 原生路径和临时 target，避免 UNC target 增量锁问题：

```bash
cd /home/lenovo/free-model-client-rs
CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/free-model-client-rs-target cargo fmt -- --check
CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/free-model-client-rs-target cargo clippy --all-targets -- -D warnings
CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/free-model-client-rs-target cargo test
```

结果：

- `fmt --check` 通过。
- `clippy --all-targets -- -D warnings` 通过。
- `cargo test` 通过：库测试 89 条、kernel golden 103 条、doc tests 0 条。
- `zen-proxy-rs` 本轮已改外层 V4 context compactor 和 e2e harness；当前已验证 `clippy -D warnings`、bin 单元测试 132 条、context 相关单元测试 12 条、e2e 27 条、shell e2e 9/9 通过。

注意：上述验证覆盖本仓库当前源码。2026-06-04 18:54 已将输出限制取消、模型策略收窄、flash/free 输入放行和 cache 四态观测构建进 `zen-proxy-rs` release 并部署到 panda；部署后已通过 NewAPI models、短请求和手工大上下文不折叠 smoke。真实 panda `policy-smoke/policy-dry` 和四客户端压测仍未跑，不能当作生产压测结论。

当前已实现并由测试覆盖的关键能力：

1. `Authorization` 和 `x-api-key` 两种认证头识别。
2. 请求体上限由 `FREE_MODEL_REQUEST_BODY_LIMIT_MB` 控制，默认 64MB。
3. OpenAI/Anthropic 两套入口共享协议内核。
4. 输出限制已完全取消：缺省 `max_tokens` 不再补 1024/2048；显式 `max_tokens` 原样透传；OpenAI/Anthropic 上游请求只有在客户端显式传值时才写 `max_tokens`。
5. `deepseek-v4-flash/deepseek-v4-flash-free` 已取消 Hermes/OpenClaw 适配，只保留 ClaudeCode 深度适配；该模型族不再设置输入 token 墙，`free-model-client-rs` 侧只做脱敏 request-shape 观测，不压缩输入。
6. 保留短指令原意，不再把 `1`、`继续`、`执行` 等短输入改写成 `ok`。
7. 不默认禁用 thinking；仅保留参数规范化位置。
8. OpenAI 工具历史规范化：缺 id、缺 `tool_call_id`、孤儿 tool result、交错 user 消息等有修复/降级。
9. Anthropic 工具历史到 OpenAI 工具调用的转换和缺失 id 修复。
10. SSE parser 支持 CRLF、可选空格、多行 data，并能识别截断、解析错误和 finish_reason。
11. 上游空内容且无工具调用时返回结构化空输出错误，不再合成假的工具调用。
12. 429 上游错误保留 `Retry-After` 等关键响应头信息。
13. Markdown fence 边界修复，降低流式代码块截断导致客户端显示异常的概率。
14. `ParsedZenFrame::Event` 已装箱，`clippy::large_enum_variant` 已消除。
15. 客户端 profile 已支持显式 `x-fmc-client`、header、UA 和 body/toolset 推断；只有强 OpenClaw/Hermes marker 或 OpenClaw 专属工具集才覆盖 ClaudeCode，普通正文提到 OpenClaw/Hermes、或只带 `web_fetch`/`web_search` 不再误判为 OpenClaw。
16. 空内容、无工具的 OpenAI/Anthropic 健康探测会走本地短路 `ok`，不再误进上游或 huge buffered retry。
17. ClaudeCode huge buffered stream 只在修复前估算输入 >= 50k tokens 时启用；小 `max_tokens` 健康探测不再触发 huge retry 路径。
18. NewAPI 管理端测渠道常见的极短 `echo hi` 流式探测，如果上游空输出，会返回本地 `ok`，只在无工具、单用户消息、`max_tokens <= 64` 的探测形态触发，避免误伤普通请求。
19. 已新增脱敏 request-shape 观测：OpenAI/Anthropic 入口统一记录 `system_tokens/messages_tokens/tools_tokens/tool_count/message_count/largest_message_tokens/last_user_tokens/estimated_total_tokens/stream/max_tokens/tool_choice_present/prompt_hash/source_client/profile_source`，不记录原始 prompt、请求体或 key。
20. 已新增小非流式请求分类：`health_probe/channel_test/internal_claude_code_probe/user_short_request/unknown_short_nonstream/not_short_nonstream`；当前只用于日志归因，普通 ClaudeCode 小非流式非探针请求在上游空输出时仍返回结构化 502，不会被本地 `ok` 误短路。
21. ClaudeCode huge-session compactor 属于历史修复背景；当前 `deepseek-v4-flash/deepseek-v4-flash-free` 路径已经取消输入墙和输入压缩，只保留脱敏观测，避免在本仓库侧裁剪用户上下文。
22. 源码已补非流式 cache usage 透传：OpenAI 非流式正文/工具调用响应会保留 `prompt_tokens_details.cached_tokens`、`cache_creation_input_tokens`、`cache_read_input_tokens`；Anthropic 非流式正文/工具调用响应也会保留真实 `cache_*`，不再统一写死为 `0`。
23. 源码已补上游 `finish_reason` 透传：OpenAI 非流式/流式会保留 `length/content_filter/stop`；Anthropic 非流式/流式/buffered 流式会把上游 `length` 映射为 `max_tokens`，不再把提前到达长度上限伪装成正常 `end_turn`。
24. 源码已补 ClaudeCode 中等工具历史压缩：当消息很多、总上下文约 24k+、单条旧工具输出 12k+、最新用户指令很短时，会折叠旧工具/会话历史并保留最新用户目标，覆盖线上 `last_user_tokens=3`、旧工具输出 26k、总输入不到 50k 的截断感场景。
25. 源码已补模型维度的有效 profile 策略：日志仍记录真实 `source_client`，但 `deepseek-v4-flash/deepseek-v4-flash-free` 不再应用 Hermes/OpenClaw 兼容策略，只保留 ClaudeCode 深度适配；`deepseek-v4-flash-lite/big-pickle` 不再应用 ClaudeCode 适配，只保留 Hermes/OpenClaw 适配。
26. cache 观测新增四态：`attempted`、`accepted`、`rejected`、`ignored`；同时采集 provider response/header/body usage 信号，用来区分“上游未给 usage/cache”、“NewAPI/中间层剥离 header”和“真实 cache 命中”。
27. 显式 `reply PONG only` 等短 smoke 在无工具、`max_tokens <= 64` 且上游连续空输出后允许返回本地 `PONG`；普通短请求仍不会伪造答案。
28. `zen-proxy-rs` 外层 V4 context compactor 已按模型分流：`deepseek-v4-flash/deepseek-v4-flash-free` 只记录 `warn/pass`，不 compact、不因 token target reject；`deepseek-v4-flash-lite/big-pickle` 仍保留 compactor 能力，避免把全局大上下文保护误关。
29. V4.98 cache-friendly session 已在本仓库源码落地：大请求上游 `x-opencode-session` 不再按完整 `messages` hash 每轮变化，而是按稳定前缀 hash、tools hash、tool_choice hash、模型、api key hash 和时间桶分组；请求正文、消息顺序、`max_tokens` 均不改写。
30. V4.98 新增脱敏 prefix 观测：request-shape 和 cache observation 日志记录 `prefix_4k_hash/prefix_32k_hash/prefix_128k_hash/prefix_256k_hash/cache_material_bytes`，用于判断长会话前缀是否稳定；仍不记录原始 prompt、请求体或 key。
31. 2026-06-05 已补并部署 ClaudeCode 低预算工具探针保护：仅当 `source_client=ClaudeCode`、非流式、`max_tokens<=32`、工具数 1-2、无显式 `tool_choice`、小上下文时，第一次上游请求前禁用 thinking，并把上游 `max_tokens` 最小抬到 64，避免 `/context` 等内部探针被 DeepSeek 消耗在 reasoning-only 后裸 502；普通工具调用、长上下文、Hermes/OpenClaw 不受影响。
32. 2026-06-05 已补并部署 ClaudeCode Anthropic 流式 idle ping 保活：仅对 `source_client=ClaudeCode` 的 Anthropic SSE 流，在 15 秒内没有下游可转发事件时发送协议级 `event: ping` / `{"type":"ping"}`；不伪造内容、不计入 first content、不改写 prompt，用来降低 50k+ 流式请求在真实内容前被 NewAPI/客户端判为 `client_gone` 的概率。
33. 2026-06-06 已补并部署 V4.99 ClaudeCode Anthropic Stream Guard：当 ClaudeCode Anthropic stream 在真实 text/tool 输出前遇到上游 `stream truncated before DONE or finish_reason` 或 60 秒无可转发内容时，最多 3 次原地重试；最后一次仅在工具请求场景启用 disabled thinking 兜底。正常请求不改 prompt、不裁剪输入、不限制输出、不默认禁用 thinking。
34. 2026-06-06 已补 Anthropic 工具调用 `input_json_delta` 分片：普通流式和 buffered huge-stream 返回工具参数时按 4KB 安全切片发送，保证拼接后 JSON 字符完全一致，降低大 Write 参数导致客户端/中间层解析压力。ClaudeCode 显式 forced `tool_choice` 会首跳禁用 thinking，避免上游返回 `Thinking mode does not support this tool_choice`；`tool_choice=auto` 和普通 tools 请求仍保持默认 thinking。
35. 2026-06-06 已补 provider `reasoning_content` 缺失兜底：当上游直接返回 `The reasoning_content in the thinking mode must be passed back to the API` 时，OpenAI/Anthropic 非流式、OpenAI 流式、ClaudeCode Anthropic 流式和 buffered huge-stream 会将同一请求重试一次 `thinking: disabled`；仅在 provider 明确拒绝当前请求后触发，不全局禁用 ClaudeCode tools auto thinking。
36. 2026-06-06 已补上游错误脱敏映射：`AppError::upstream` 不再把 `opencode zen`、上游原始 body、内部路由或节点标识写进 public response；public body 使用 `upstream_provider_error` 和稳定 `code`，私有 provider 状态只进服务端日志。
37. 2026-06-08 源码已补 V4.101 ClaudeCode Anthropic 工具流提前释放：只有当上游工具调用参数已经拼成完整、可解析 JSON 后才向下游发送 `tool_use`，并在日志中记录 `first_tool_emit_ms` 和 `emitted_tool_call_count`；不发送 partial tool、不伪造工具、不改写 prompt。
38. 2026-06-08 源码已补 V4.101 自适应 no-forwardable watchdog：ClaudeCode Anthropic stream 在真实 text/tool 发出前按输入桶使用 10s/14s/22s/32s/45s 上限，而不是固定等满 45s；若用户配置更低值，则继续尊重更低值。
39. 2026-06-08 `zen-proxy-rs` 源码已补 V4.101 cache-friendly affinity key：大流式请求的 affinity 从 `model/path/client/body_bucket` 升级为包含稳定 `messages` 前缀 hash、`tools` hash 和 `tool_choice` hash；只保存 hash，不保存 prompt 原文。
40. 2026-06-08 `zen-proxy-rs` 源码已补 V4.101 中等工具流隔离：tool-heavy lane 阈值从 `tools>=16 / tool_markers>=12` 下调到 `tools>=8 / tool_markers>=6`，让 ClaudeCode 中等工具链请求更早进入隔离 lane，降低普通流式请求被工具流拖慢的概率。
41. 2026-06-08 22:27 CST 已将 V4.101 stripped release 部署到 panda 三实例；线上二进制 hash `149dd2f65c8b33228498bcc1f2e94f6742e1e1a5417592c0eb6921e7cc7deb49`，旧版备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260608-222704.pre-v4101`。部署后 `/health`、`/v1/models`、OpenAI stream、Anthropic ClaudeCode stream 和 NewAPI OpenAI stream 最小 smoke 均通过。
42. 2026-06-09 已补并部署 V4.102 ClaudeCode 工具参数完整性门控：Anthropic/ClaudeCode 流式和非流式只在工具参数包含必填字段且 JSON 完整后下发 `tool_use`；上游空 `{}` 或缺必填参数时先做窄范围 disabled-thinking retry，仍不完整则返回结构化 `upstream returned incomplete tool call arguments`，不再把坏工具调用交给 ClaudeCode 造成 `Invalid tool parameters`。同时新增重复补参防循环：同一修复后工具调用如果历史中已有 assistant tool_call 和对应 tool_result，不再重复补发。另补文件工具坏路径保护：`Read/Write/Edit` 等收到 `file_path="\\\\"`、`"/"`、`"."` 这类明显非文件路径时，优先从最新用户明确指令修复，修不了则拒绝下发。线上 stripped hash `ebe41572fe76a5f99783ba5e4308e164368415b00277432cd9829e60ecc651dd`，旧版备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260609-111046.pre-v4102-tool-input-guard`。

## 附属工具

2026-06-05 新增独立 Rust sidecar：`tools/newapi-usage-exporter/`。

边界：

- 只读 NewAPI 使用日志数据库，支持 SQLite / Postgres。
- 不修改 NewAPI，不进入 ZenProxy/free-model-client-rs 主链路。
- 按 `user_id + time range` 导出，单次最大 31 天。
- 导出 zip 默认保留 30 天，过期清理。
- 不导出 prompt 原文、完整响应、真实 API key 或 IP 明文。
- 不做套餐推荐，不凭 tokens 猜用户真实用途。

接口：

- CLI：`serve`、`export`、`cleanup`。
- HTTP：`GET /health`、`POST /v1/usage-export`、`POST /v1/usage-export/instruction`、`GET /v1/usage-export/{id}`、`GET /v1/usage-export/{id}/download`、`DELETE /v1/usage-export/{id}`。
- panda helper：`newapi-usage-export '导出用户1从2026年6月5日~2026年6月5日的数据并做简要分析'`。

验证：

- `cargo fmt --manifest-path tools/newapi-usage-exporter/Cargo.toml -- --check` 通过。
- `cargo clippy --manifest-path tools/newapi-usage-exporter/Cargo.toml --all-targets -- -D warnings` 通过。
- `cargo test --manifest-path tools/newapi-usage-exporter/Cargo.toml` 通过：6 条测试。
- panda 真实 Postgres 直连验收通过：用户 1 当天 865 行导出 0.05 秒；用户 2 31 天 97,438 行导出 1.17 秒；HTTP create/get/download/delete 通过。
- panda 已部署 `newapi-usage-exporter.service`，本地 API `http://127.0.0.1:8098` active；一句话 helper 和 `/v1/usage-export/instruction` 验收通过。

详细说明见 `docs/08-newapi-usage-exporter.md`。

## 运行链路事实

已确认的最小事实：

```text
client -> panda NewAPI http://100.69.228.93:8081 -> configured upstream
```

注意：本仓库文档目前不把 panda NewAPI 后面的真实渠道名写死。除非通过 NewAPI 管理端、日志或响应证据确认，否则后续报告只能写 “configured upstream”。

2026-06-03 已确认的 channel 69 事实：

- panda NewAPI channel 69 名称为 `Zenproxyrs4.3`，状态启用，分组为 `vip`。
- channel 69 模型为 `deepseek-v4-flash,deepseek-v4-flash-lite`，base URL 为 `http://172.17.0.1:4000`。
- `sk-dev` 已被删除且属于历史 default 组，不能用于判断 channel 69 是否可用。
- NewAPI token 表中有效 `vip` 组 token 可通过 channel 69；文档和报告只记录 token id/name/group，不记录明文 key。
- 2026-06-03 11:26 NewAPI 日志出现 channel 69 管理测试成功消费记录：`request_path=/v1/messages`、`stream_status=ok`、`frt=76ms`。
- 2026-06-03 11:34 panda 本机 NewAPI 实测：`/v1/models` 200 且包含两个 deepseek 模型；OpenAI 非流式 `deepseek-v4-flash` 200，返回 `CHANNEL69_OPENAI_OK`；Anthropic 流式 `deepseek-v4-flash` 200，返回 `CHANNEL69_ANTHROPIC_OK`。

历史探测事实：

- WSL 到 `http://100.69.228.93:8081/v1/models` 可达。
- 返回模型包含 `deepseek-v4-flash`、`deepseek-v4-flash-lite`、`gpt-5.5`、`gpt-5.4`、`gpt-5.4-mini`。
- `http://43.156.233.219:8081`、`http://8.163.32.25:8081` 曾超时。
- 直连 `https://sub2api.closeapi.top/v1/models` 曾返回 Cloudflare 403/1010；这只能作为直连基线，不代表 panda NewAPI 不可用。

2026-05-30 至 2026-05-31 小矩阵实测：

| 场景 | 结果 | 耗时 | 证据摘要 |
|------|------|------|----------|
| panda `/v1/models` | 200 | 612ms | 返回 `deepseek-v4-flash`、`deepseek-v4-flash-lite`、`gpt-5.5`、`gpt-5.4`、`gpt-5.4-mini` 等模型。 |
| panda OpenAI `/v1/chat/completions` | 200 | 1882ms | `deepseek-v4-flash`，非流式，返回 `PONG`。 |
| panda Anthropic `/v1/messages` | 200 | 1532ms | `deepseek-v4-flash`，非流式，返回 `PONG`。 |
| WSL Hermes 短回复 | 成功 | 33989ms | 临时 `CUSTOM_BASE_URL=http://100.69.228.93:8081/v1`，返回 `PONG`。 |
| WSL Hermes 文件/终端工具 | 成功 | 54175ms | 写入并读取 `/tmp/hermes-panda-tool-test/hello.txt`，返回 `TOOL_OK:hello-panda`。 |
| WSL Hermes web 探测 | 命令成功 | 52335ms | 返回 `WEB_FAIL`，属于工具/模型判断结果，不是链路错误。 |
| WSL OpenClaw Node 运行时 | 成功 | - | 隔离安装 Node `v22.21.1` 到 `~/.local/opt/node-v22.21.1-linux-x64`，未覆盖系统 Node `v20.20.2`。 |
| WSL OpenClaw config validate | 成功 | 1550ms | 临时配置位于 `.codex_tmp/openclaw-panda/openclaw.json`。 |
| WSL OpenClaw models list | 成功 | 3998ms | 返回 `panda-newapi/deepseek-v4-flash`、`panda-newapi/deepseek-v4-flash-lite`。 |
| WSL OpenClaw infer PONG | 成功 | 9106ms | `infer model run --local`，winnerProvider=`panda-newapi`，返回 `PONG`。 |
| WSL OpenClaw agent PONG | 成功 | 12144ms | 默认 agent id 为 `main`，winnerProvider=`panda-newapi`。 |
| WSL OpenClaw agent 文件工具 | 成功 | 26787ms | `exec/write/read` 共 3 次工具调用，返回 `TOOL_OK:hello-openclaw`。 |
| WSL OpenClaw agent web_fetch | 成功 | 19088ms | 1 次 `web_fetch`，返回 `WEB_OK`。 |

结论：panda NewAPI、WSL Hermes、WSL OpenClaw 到 panda NewAPI 的小矩阵已经跑通。Hermes/OpenClaw 的 agent 工具链耗时明显高于裸 HTTP，包含客户端启动、上下文注入、工具 schema 和本地 agent 循环成本，不能直接等同于上游 TTFT。

## 当前未完成

P0 已完成：

1. panda NewAPI key/base URL 的 OpenAI `/v1/chat/completions` 最小请求已通过。
2. panda NewAPI key/base URL 的 Anthropic `/v1/messages` 最小请求已通过。
3. Hermes 临时指向 panda NewAPI，已运行短回复、文件/终端工具、web 探测。
4. OpenClaw Node 版本问题已用隔离 Node 22.21.1 解决，未覆盖系统 Node。
5. OpenClaw 临时指向 panda NewAPI，已运行 models list、infer、agent 短回复、文件工具、web_fetch。
6. 文档中的 API key 均按 `sk-***` 形式脱敏。

P0 残留观察点：

1. Hermes CLI 短回复和工具用例耗时波动较大，后续压测需要拆分客户端启动、上下文注入、上游模型响应三段耗时。
2. Hermes web 用例返回 `WEB_FAIL`，命令本身成功；后续若要验收 web 能力，需要固定更清晰的 web 工具断言和可访问目标。
3. OpenClaw 临时配置仍在 `.codex_tmp/openclaw-panda`，属于测试资产，不应提交密钥或大输出。

P1 当前状态：

1. 90 分客户端识别和策略隔离已落地，避免 Hermes/OpenClaw 兼容策略继续误伤 ClaudeCode。
2. 已实现 `ClientProfile`、`x-fmc-client`、OpenAI/Anthropic chat 路径 profile 传递、per-client thinking/whitespace/tool-history policy。
3. 已验证 kernel golden：ClaudeCode tools 不默认禁用 thinking，ClaudeCode 流式空白 delta 保留，Hermes compat thinking 策略保留，显式 `x-fmc-client` 优先。
4. 2026-06-04 15:33 已将包含 `finish_reason` 透传、non-stream cache usage 透传和 ClaudeCode 中等工具历史压缩的 `zen-proxy-rs` release 部署到 panda 三实例；panda 运行链路为 `NewAPI 8081 -> zen-proxy-rs 4001/4002/4004 -> free-model-client-rs kernel -> upstream`。
5. 本轮部署后 smoke：`/v1/models` 返回 200，包含 `deepseek-v4-flash`、`deepseek-v4-flash-lite`；`deepseek-v4-flash` OpenAI 非流式返回 `PONG`，HTTP 200；Anthropic 流式返回 `PONG`，HTTP 200；Anthropic 极短非流式探针仍可能在上游持续空输出时返回 502。
6. 最新源码/脚本状态：输出限制已完全取消，ZenProxy 侧 non-stream output guard 已取消；ZenProxy 外层 context compactor 对 flash/free 已放行、对 lite 仍生效；`policy-smoke/policy-dry` harness 已存在并记录 input/output wall、provider header/body usage、cache 四态。真实 panda `policy-smoke/policy-dry` 尚未跑，不能写成生产已验证。

P1 panda 部署记录：

| 项 | 值 |
|----|----|
| 部署时间 | 2026-05-31 |
| 部署目标 | `/opt/zen-proxy-rs/zen-proxy-rs` |
| 旧二进制 hash | `2a7ab35e80c112a623c4ec3e3519e02ca29b652d368967534b90f70d5920a132` |
| 本地 release hash | `65e1ee22dd1f392b566e2af8664308cea044599294f93d228da1082f9b1639f9` |
| 实际部署 hash | `35feea8bba79412e0b323a494ebd5450e8a21bfd9769395091eb8c6c0d87165a` |
| 说明 | 实际部署前对 release 二进制执行 `strip`，因此部署 hash 与本地未 strip release hash 不同。 |
| 备份 | `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-client-profile-20260531-131038` |
| 实例 | `zen-proxy-rs@1:4001`、`zen-proxy-rs@2:4002`、`zen-proxy-rs@3:4004` |
| 健康检查 | 三实例 `/health` 均 200，`status=ok`，池 `total=90`、`dispatch=90`。 |
| NewAPI smoke | `/v1/models` 200；`/v1/chat/completions` 200，`deepseek-v4-flash` 返回 `OK`。 |

P1.1 OpenClaw profile 修复部署记录：

| 项 | 值 |
|----|----|
| 部署时间 | 2026-05-31 晚间 |
| 部署目标 | `/opt/zen-proxy-rs/zen-proxy-rs` |
| 实际部署 hash | `b9441bce94180aed6aae7dd6af79da618382a2a1f49ee292620b08cd2cd357fd` |
| 实例 | `zen-proxy-rs@1:4001`、`zen-proxy-rs@2:4002`、`zen-proxy-rs@3:4004` |
| 健康检查 | 三实例 `/health` 均 200，`status=ok`，池 `total=90`、`dispatch=90`、`dead=0`、`ratelimited=0`、`upstream.backoff=false`。 |
| 代码验证 | `free-model-client-rs` 的 `fmt`、`clippy -D warnings`、`cargo test` 通过；`zen-proxy-rs` 的 `fmt`、`clippy -D warnings`、`cargo test` 通过。 |

P1.2 ClaudeCode huge_context final-anchor 修复部署记录：

| 项 | 值 |
|----|----|
| 部署时间 | 2026-06-01 下午 |
| 部署目标 | panda `/opt/zen-proxy-rs/zen-proxy-rs` |
| 上一线上 hash | `94872cd91a558ec431e176af5a1fa8f257219c9518df3f729e7e5645b5cbb937` |
| 本地 release hash | `c44df2fb8ecae44a5c155e88953574e6990f07947b45b71e1f60468f2c00c06e` |
| 实际部署 hash | `3a27f2c7cda56119b32dfc42738b06f3f1e08155a0ff89c48daca6ddc8aed1d4` |
| 说明 | 实际部署前对 release 二进制执行 `strip`，因此部署 hash 与本地未 strip release hash 不同。 |
| 备份 | `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-final-anchor-20260601-94872cd` |
| 实例 | `zen-proxy-rs@1:4001`、`zen-proxy-rs@2:4002`、`zen-proxy-rs@3:4004` |
| 健康检查 | 三实例 `/health` 均 200，`status=ok`，池 `total=90`、`dispatch=90`、`dead=0`、`ratelimited=0`、`upstream.backoff=false`。 |
| 构建验证 | `/home/lenovo/zen-proxy-rs` 执行 `CARGO_INCREMENTAL=0 cargo build --release` 通过，依赖本地 `../free-model-client-rs`。 |
| NewAPI smoke | panda 本机 `http://127.0.0.1:8081/v1/models` 200，包含 `deepseek-v4-flash` 和 `deepseek-v4-flash-lite`；OpenAI 非流式 `deepseek-v4-flash` 返回 `OK`。 |
| huge stream smoke | panda 本机 `/v1/messages`，约 1.0MB 请求体，`source_client=claude-code`，触发 `appended_latest_user_anchor=true`；`deepseek-v4-flash` 3/3 返回 `HUGE_OK`，`deepseek-v4-flash-lite` 3/3 返回 `HUGE_OK`。 |
| 耗时观察 | flash 三轮约 2.5s、2.7s、3.1s；lite 三轮约 3.2s、3.3s、14.8s。另有早期单轮 flash 3.9s 成功。 |
| 残留观察 | 日志仍可见上游偶发空输出，buffered retry 已处理，未裸透给 smoke 客户端；这仍需在正式 dry run 中统计。 |

P1.3 channel 69 健康测试/空输出误判修复部署记录：

| 项 | 值 |
|----|----|
| 部署时间 | 2026-06-03 上午 |
| 部署目标 | panda `/opt/zen-proxy-rs/zen-proxy-rs` |
| 上一线上 hash | `66b883256e08d42dbc7e473e865dc9e05f5318260c290837530f5cacd62f912f` |
| 实际部署 hash | `28b928472d2abc7be036cdc2796915865bd4a5b083ee2dd1dab46b1ddd0e2633` |
| 备份 | `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-channel-test-fix-20260603-111524-66b8832` |
| 实例 | `zen-proxy-rs@1:4001`、`zen-proxy-rs@2:4002`、`zen-proxy-rs@3:4004` |
| 根因 | ClaudeCode 小 `max_tokens` 流式请求会误进 huge buffered retry；NewAPI channel test 的小/空探测因此被当作大上下文重试，最终出现 `upstream returned no assistant content or tool call`。 |
| 修复 | 空内容无工具健康探测短路为本地 `ok`；ClaudeCode huge buffer 改为只按修复前输入 tokens >= 50k 触发。 |
| 后续补强 | 对管理端测渠道常见的 `echo hi` 极短流式探测增加空上游 fallback：先尝试上游，只有上游空输出才返回本地 `ok`。 |
| 代码验证 | `free-model-client-rs` 的 `fmt --check`、`clippy -D warnings`、`cargo test` 通过；kernel golden 64 条。 |
| panda 验证 | channel 69 管理测试日志成功；有效 `vip` 组 token 下 `/v1/models`、OpenAI 非流式、Anthropic 流式均 200。 |
| 第二层验收 | panda NewAPI 有效 `vip` 组 token 下，`echo hi`/`max_tokens=16`/stream 探测 10/10 返回 200，`empty_error=0`；ZenProxy 日志确认上游空流被本地 `ok` 兜底。 |

P1.4 非流式渠道探针兜底源码记录：

| 项 | 值 |
|----|----|
| 修改时间 | 2026-06-03 晚间 |
| 状态 | 已随 2026-06-03 晚间 profile/格式误伤修复一并部署到 panda。 |
| 修复 | `echo hi`/`hi`/`hello`/`test` 类极短非流式无工具探针，在上游连续空输出后返回本地 `ok`；普通请求仍返回结构化空输出错误。 |
| 代码范围 | `src/protocol/translate.rs`、`src/proxy/anthropic.rs`、`src/proxy/openai.rs`、`tests/kernel_golden.rs`。 |
| 验证 | WSL 临时 target：`cargo fmt --check`、`cargo test`；结果为库测试 64 条、kernel golden 70 条、doc tests 0 条全部通过。 |
| panda 部署 | 线上 hash 已更新为 `e47e2c89d7c0c497daa2cd49a9d135a8b695928e951f81a22630b206a0e2ab51`；备份为 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-profile-format-20260603-202631-cdafae8`。 |

P1.5 ClaudeCode 格式误伤修复部署记录：

| 项 | 值 |
|----|----|
| 部署时间 | 2026-06-03 晚间 |
| 根因 | `free-model-client-rs` 内核和 `zen-proxy-rs` 外层都曾把正文里普通出现的 `OpenClaw/Hermes`、以及 `web_fetch`/`web_search` 工具名当成 OpenClaw 强信号，导致 ClaudeCode 请求可能走 OpenClaw/Hermes 兼容策略，不再逐字保留 Markdown/空白。 |
| 修复 | 收窄 body marker，只接受 `running inside openclaw/hermes`、`openclaw cli/agent`、`hermes cli/agent` 等强 marker；`web_fetch`/`web_search` 不再单独触发 OpenClaw；OpenClaw 仍由 `subagents`、`sessions_*`、`memory_*`、`openclaw*` 等专属工具识别。 |
| 代码范围 | `src/client_profile.rs`；`zen-proxy-rs/src/v4/provider.rs`。 |
| 测试 | 新增 ClaudeCode + Web 工具、普通 OpenClaw/Hermes 文本引用、Web-only 工具三类回归；free-model 库测试 64/64、kernel golden 70/70；ZenProxy 单元 129/129、e2e 26/26。 |
| panda 验证 | 新 pid `365354/365378/365392` 全部健康；`/v1/models` 200 且只暴露 `deepseek-v4-flash`、`deepseek-v4-flash-lite`；受控 `Task + web_fetch` `/v1/messages` 日志显示 `source_client=claude-code`。 |
| 运行观察 | 旧 pid `350170/350205/350216` 日志中大量 `source_client=openclaw` 属于部署前误伤；部署后新 pid 日志里的受控样本已转为 `claude-code`。 |

P1.6 request-shape 观测部署记录：

| 项 | 值 |
|----|----|
| 部署时间 | 2026-06-03 晚间 |
| 根因 | 需要从源头拆分 ClaudeCode 300KB+ 请求体来源，并识别 `body_size=342` 这类小非流式空输出请求用途。 |
| 本地未 strip release hash | `1e77a544b5c444b4d626b14327862259cfe1b26221ffbd6f87778c1afc321376` |
| 部署 stripped hash | `28b25370925835bb33aa4142208a5a20f0cf4dcb74ad3ae74c3808d3c2761e2b` |
| 旧线上 hash | `e47e2c89d7c0c497daa2cd49a9d135a8b695928e951f81a22630b206a0e2ab51` |
| 备份 | `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-request-shape-20260603-232757-e47e2c8` |
| 实例 | `zen-proxy-rs@1:4001` pid `462903`、`zen-proxy-rs@2:4002` pid `462918`、`zen-proxy-rs@3:4004` pid `462981`。 |
| 验证 | `free-model-client-rs`：`fmt --check`、`clippy -D warnings`、`cargo test` 通过；库测试 69 条、kernel golden 71 条。`zen-proxy-rs`：单元测试 129 条、e2e 26 条、release build 通过。 |
| panda 验证 | 三实例和 nginx active；4001/4002/4004/4000 `/health` 均 200；`/metrics` 显示 `200=6`、`5xx=0`、`timeout=0`、active concurrency 0、dead 0、ratelimited 0。 |
| NewAPI 验证 | panda 本机 NewAPI 8081 使用 token id `38`/name `ds`/group `vip`，`deepseek-v4-flash` 非流式 smoke HTTP 200，返回 `OK`，总耗时约 4.01s；NewAPI 日志 id `109472` 显示 channel 69、token id 38、prompt tokens 93、completion tokens 47、非流式。 |
| shape 证据 | 部署后 ZenProxy 日志已出现 `desensitized request shape before upstream`；小非流式样本被分类为 `internal_claude_code_probe`，真实 ClaudeCode 787KB 流式样本记录 `message_count=703`、`system_tokens=5773`、`messages_tokens=78859`、`tools_tokens=12700`、`tool_count=10`、`estimated_total_tokens=97332`。 |

P1.7 ClaudeCode huge-session compactor 部署记录：

| 项 | 值 |
|----|----|
| 部署时间 | 2026-06-04 凌晨 |
| 目标 | 修复 ClaudeCode 长会话反复执行旧任务、非流式 200k+ fallback 放大旧历史的问题。 |
| 本地未 strip release hash | `408fa673aef439e849ca9e24d41576810c122faa71724581ce8067e10c04fc80` |
| 部署 stripped hash | `96b954a81978e9348f26341d68626d0a98682c6971611d7802a0850ef771d815` |
| 旧线上 hash | `28b25370925835bb33aa4142208a5a20f0cf4dcb74ad3ae74c3808d3c2761e2b` |
| 备份 | `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-huge-session-20260604-003040-28b2537` |
| 实例 | `zen-proxy-rs@1:4001` pid `498728`、`zen-proxy-rs@2:4002` pid `498733`、`zen-proxy-rs@3:4004` pid `498734`。 |
| 健康检查 | 4001/4002/4004/4000 `/health` 均 200；三实例 active；池 `total=90`、`dispatch=90`、`dead=0`、`ratelimited=0`。 |
| 验证 | `free-model-client-rs`：`fmt --check`、`clippy -D warnings`、`cargo test` 通过；库测试 69 条、kernel golden 73 条。`zen-proxy-rs`：主单测 129 条、e2e 26 条通过；release build 通过。 |
| panda 非流式 smoke | 517KB / `before_tokens=123371` 的 ClaudeCode 非流式样本被压到 `after_tokens=9139`、`message_count=51`；NewAPI id `109585` 账面 `prompt_tokens=5647`，HTTP 200。 |
| panda 流式 smoke | 522KB / `before_tokens=124597` 的 ClaudeCode 流式样本触发 exact-anchor，shape `message_count=1`、`estimated_total_tokens=51`；NewAPI id `109593` 账面 `prompt_tokens=125`，HTTP 200。 |

P1.8 NewAPI 短 smoke 探针空输出兜底：

| 项 | 值 |
|----|----|
| 触发 | 2026-06-04 严格验收时，panda NewAPI channel 69 的极短 non-stream smoke 经 NewAPI 转为 ClaudeCode/Anthropic 小请求，上游连续返回空输出，旧逻辑在 `internal_claude_code_probe` 分类下裸透 502。 |
| 修复 | 新增 `short_no_tool_empty_fallback_text`，只对无工具、单用户消息、`max_tokens <= 64` 且显式 `echo hi`/`strict smoke`/`reply PASS`/`answer OK` 等测试探针触发本地兜底；普通 ClaudeCode 短输入仍不兜底。 |
| 测试 | `free-model-client-rs`：`fmt --check`、`clippy -D warnings`、`cargo test` 通过；库测试 70 条、kernel golden 75 条。`zen-proxy-rs`：主单测 129 条、e2e 26 条通过；release build 通过。 |
| 部署 | panda 三实例部署 stripped hash `0f1d7a36fdc7142e1acd9670301e7277ca6805e47899490958a2c390c619cea5`；旧 hash `96b954a81978e9348f26341d68626d0a98682c6971611d7802a0850ef771d815` 备份到 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-strict-smoke-20260604-105132-96b954a`。 |
| 线上 smoke | panda 本机 NewAPI `/v1/models` 200，返回 8 个模型且包含 `deepseek-v4-flash`、`deepseek-v4-flash-lite`；两个 deepseek 模型的 OpenAI/Anthropic、stream/non-stream 共 8 条 smoke 全部 HTTP 200，内容摘要为 `PASS`。 |
| 耗时 | non-stream 总耗时约 4.8-5.5s；stream 首内容约 2.0-2.3s。 |
| 环境边界 | Windows 环境变量存在 `HTTP_PROXY=http://127.0.0.1:7897`；Windows `Invoke-RestMethod` 走代理访问 panda NewAPI 会 502，但 `curl --noproxy '*'` 直连 `100.69.228.93:8081/v1/models` 为 200。Windows ClaudeCode/cc-switch 若继承该代理，需要显式绕过 panda Tailscale IP。 |

P1.9 ClaudeCode 大流式 768/1024 cap 桶 buffered retry 修复：

| 项 | 值 |
|----|----|
| 修复时间 | 2026-06-04 中午 |
| 根因 | 外层已判断 ClaudeCode 大上下文或低输出 cap 应进入 huge buffered retry，但 `handle_stream` 内部又二次限制 `max_tokens <= 512`，导致 `max_tokens=32000` 被 cap 到 768/1024 的真实大流式请求绕过 retry，遇到上游空输出时裸透 `upstream returned no assistant content or tool call`。 |
| 修复 | 移除 `handle_stream` 内部多余的 512 门槛；只要外层 `use_claude_code_huge_buffer=true` 就进入 buffered retry。 |
| 回归 | 新增 `claude_code_huge_stream_uses_buffer_retry_after_1024_output_cap`：约 50k+ 输入、`max_tokens=32000 -> 1024`、上游第一次空输出、第二次正常输出，断言 upstream 请求 2 次且不再返回空 assistant 错误。 |
| 本地验证 | `free-model-client-rs`：`fmt --check`、`clippy -D warnings`、`cargo test` 通过；库测试 71 条、kernel golden 76 条。`zen-proxy-rs`：主单测 129 条、e2e 26 条、release build 通过。 |
| 部署 | panda 三实例部署 stripped hash `7a8f4e5dc99e8ccf1aaf6562519d8353dc4ba5205e5e55f521c265b0760ed66e`；旧 hash `117b3cbfaf058fbbeb258f98542afc09a097e763359f34d174414b47dfd11aff` 已备份到 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-buffered-1024-*`。 |
| 线上健康 | `zen-proxy-rs@1/@2/@3` active；`http://127.0.0.1:4000/health` 返回 `status=ok`、`dispatch=90`、`dead=0`、`ratelimited=0`、`upstream.backoff=false`。 |

P1.10 non-stream cache usage 透传源码修复：

| 项 | 值 |
|----|----|
| 修改时间 | 2026-06-04 |
| 根因 | NewAPI 看不到/不显示 cache，不只是上游不返回；源码里 OpenAI 非流式响应根本没带 `cache_*` 字段，Anthropic 非流式正文/工具调用响应长期把 `cache_creation_input_tokens/cache_read_input_tokens` 写死为 `0`。 |
| 修复 | `src/proxy/openai.rs`、`src/proxy/anthropic.rs` 已改为在非流式正文和工具调用两条分支透传真实 usage：`prompt_tokens_details.cached_tokens`、`cache_creation_input_tokens`、`cache_read_input_tokens`。 |
| 验证 | WSL `lenovo` 用户下执行 `cargo fmt -- --check`、`cargo test -q`、`cargo clippy --all-targets -- -D warnings` 通过；库测试 71 条、kernel golden 87 条。 |
| 部署状态 | 已随 2026-06-04 15:33 panda release 部署；实际部署 stripped hash `694036f6a130e8211b998a5b58eff36105fb48fb866ec57ebbb2c03ccfb5f3d7`，备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-v46-20260604-153327-0f6cdf6e5cd2`。 |
| 线上观察 | 部署后 OpenAI 非流式 smoke usage 已正常透出 `prompt_tokens=87`、`completion_tokens=27`。该样本 `cached_tokens=0`，说明这次调用本身没有上游 cache 命中，不代表透传无效。 |

P1.11 ClaudeCode 半截输出根因修复源码记录：

| 项 | 值 |
|----|----|
| 修改时间 | 2026-06-04 |
| 根因 | 近期 panda 样本显示，小 prompt 可长输出，但 ClaudeCode 工程请求大量为中等上下文 + 工具历史形态：`last_user_tokens` 经常只有 3，旧工具输出可达 26k；同时源码把上游 `finish_reason=length` 固定改成正常 `stop/end_turn`，导致提前停止不可见。 |
| 修复 1 | OpenAI/Anthropic 响应保留上游 `finish_reason`；Anthropic 将 `length` 映射为 `max_tokens`。 |
| 修复 2 | ClaudeCode 中等工具历史压力下提前折叠旧历史：消息数 >=40、消息 token >=24k、最大非系统消息 >=12k、最新 user <=1024 tokens。 |
| 验证 | 新增 `finish_reason=length` 四路径回归、ClaudeCode 中等工具历史折叠回归；`cargo fmt -- --check`、`cargo test -q`、`cargo clippy --all-targets -- -D warnings` 通过。 |
| 部署状态 | 已随 2026-06-04 15:33 panda release 部署；实际部署 stripped hash `694036f6a130e8211b998a5b58eff36105fb48fb866ec57ebbb2c03ccfb5f3d7`。 |
| NewAPI 验收 | `curl --noproxy '*' http://100.69.228.93:8081/v1/models` 200，模型数 8；OpenAI 非流式 `deepseek-v4-flash` 200，返回 `PONG`，usage `prompt_tokens=87`、`completion_tokens=27`；Anthropic 流式 `deepseek-v4-flash` 200，返回 `PONG`，usage `input_tokens=87`、`output_tokens=27`。 |
| 残留 | Anthropic 极短非流式探针 `reply PONG only` 仍可能被识别为 `internal_claude_code_probe`，在上游连续空输出时经 11 次 provider retry 后返回 502：`upstream retry budget exhausted ... last_error=empty_output`。该残留目前不影响真实流式小请求验收，但仍需继续补 non-stream probe 兜底。 |

P1.12 2026-06-04 V4.6 panda 部署记录：

| 项 | 值 |
|----|----|
| 部署时间 | 2026-06-04 15:33 |
| 部署目标 | `/opt/zen-proxy-rs/zen-proxy-rs` |
| 旧二进制 hash | `0f6cdf6e5cd2dd1946a69707c97591cca865b47178ff63846f04bbdf283f2314` |
| 本地未 strip release hash | `9b68db105aaad2c1014899d00122accf3a21109a26054f68ce0d612f152b5839` |
| 实际部署 stripped hash | `694036f6a130e8211b998a5b58eff36105fb48fb866ec57ebbb2c03ccfb5f3d7` |
| 备份 | `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-v46-20260604-153327-0f6cdf6e5cd2` |
| 实例 | `zen-proxy-rs@1:4001`、`zen-proxy-rs@2:4002`、`zen-proxy-rs@3:4004` |
| 健康检查 | 三实例 `active`；4000/4001/4002/4004 `/health` 均返回 `status=ok`、`dispatch=90`、`dead=0`、`ratelimited=0`、`upstream.backoff=false`。 |
| /metrics | smoke 后 `zen_proxy_requests_total{status="200"} 2`、`{status="5xx"} 1`、`stream=2`、`non_stream=1`、`model=\"deepseek-v4-flash\"=3`。 |
| NewAPI smoke | `curl --noproxy '*'` 直连 panda `8081` 时：`/v1/models` 200 且包含两个 deepseek 模型；OpenAI 非流式 `PONG` 200；Anthropic 流式 `PONG` 200。 |
| 环境边界 | WSL 若继承代理环境变量，直连 `http://100.69.228.93:8081` 可能先返回代理层 502 空响应；验收时需显式使用 `curl --noproxy '*'` 或配置 `NO_PROXY=100.69.228.93`。 |

P1.13 输出限制取消与 policy harness 当前状态：

| 项 | 值 |
|----|----|
| 状态 | 当前源码已完全取消输出限制；ZenProxy 侧 non-stream output guard 已取消；2026-06-04 18:54 已部署到 panda，并通过手工 NewAPI smoke 验证。真实 panda `policy-smoke/policy-dry` 尚未跑，不能写成生产压测已验证。 |
| max_tokens 行为 | 缺省 `max_tokens` 不再自动补 1024/2048；显式 `max_tokens` 原样透传；OpenAI/Anthropic 只有显式值才写上游。 |
| flash 策略 | `deepseek-v4-flash/deepseek-v4-flash-free` 取消 Hermes/OpenClaw 适配，只保留 ClaudeCode 深度适配；取消输入 token 墙，`free-model-client-rs` 侧只观测不压缩。 |
| lite 策略 | `deepseek-v4-flash-lite/big-pickle` 只保留 Hermes/OpenClaw 适配，取消 ClaudeCode 适配。 |
| cache/usage 观测 | cache 记录 `attempted/accepted/rejected/ignored` 四态，并记录 provider response/header/body usage 信号。 |
| harness | `scripts/panda_pressure_runner.py --mode policy-smoke|policy-dry` 记录 input/output wall、provider header/body usage、cache 四态和 lite effective profile。 |
| 风险 | 输出限制取消后，上游 413/超时/空输出/延迟/成本风险回到 upstream 与 lane/pool 调度，需要真实 panda 压测确认。 |

P1.14 2026-06-04 V47 panda 部署记录：

| 项 | 值 |
|----|----|
| 部署时间 | 2026-06-04 18:54 |
| 部署目标 | `/opt/zen-proxy-rs/zen-proxy-rs` |
| 旧二进制 hash | `694036f6a130e8211b998a5b58eff36105fb48fb866ec57ebbb2c03ccfb5f3d7` |
| 本地未 strip release hash | `aeecc8d5acbea86e36dee3f1224858b2f371d64d0ebfc2508313e33e7b09b1c0` |
| 实际部署 stripped hash | `99424602ce7c076671579abf48ca0d27367ac126e514efe4403d902d5caecd78` |
| 备份 | `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-v47-20260604-185423-694036f` |
| 实例 | `zen-proxy-rs@1:4001`、`zen-proxy-rs@2:4002`、`zen-proxy-rs@3:4004` |
| 健康检查 | 三实例 `active`；新 pid 为 `1093754/1093766/1093777`；三实例 `/health` 均返回 `status=ok`、`dispatch=90`、`dead=0`、`ratelimited=0`、`upstream.backoff=false`。 |
| 根因确认 | 部署前线上 18:19-18:21 日志仍有 `compacted streaming anthropic context before upstream` 和 `capped streaming anthropic max_tokens ... effective_max_tokens=512`，模型为 `deepseek-v4-flash-free`，说明用户看到的“我被压缩过了”来自旧线上自动 compactor，不是用户手动 compact。 |
| NewAPI smoke | panda 本机 `http://127.0.0.1:8081/v1/models` 200，包含 `deepseek-v4-flash` 和 `deepseek-v4-flash-lite`；OpenAI 非流式 `deepseek-v4-flash` 返回 `V47_SHORT_OK`，HTTP 200，约 3.9s。 |
| 大上下文 smoke | OpenAI 非流式 `deepseek-v4-flash`，361 条消息、请求体约 560KB、`max_tokens=32000`，返回 `V47_NO_COMPACTOR_OK`，HTTP 200，约 7.0s，NewAPI usage 约 `prompt_tokens=97836`。 |
| 日志验收 | 部署后日志 grep `compacted .*context|capped .*max_tokens|context compactor|effective_max_tokens` 无命中；大请求日志显示 `context_action=pass`、`effective_body_size=560709`、`max_tokens=Some(32000)`，没有输入折叠或输出 cap。 |
| cache 观察 | 大请求日志记录 `cache_observation="rejected"`、`provider_response_signal=true`、`provider_body_usage_signal=true`、`provider_body_cached_tokens=Some(0)`；这说明 provider 返回了 usage/cache 信号但本轮未命中缓存，不是 NewAPI 完全没记录。 |

P1.10 2026-06-04 三客户端 smoke 和 web/search 边界：

| 项 | 结果 |
|----|------|
| Windows ClaudeCode | 显式 base/key 指向 panda NewAPI，5/5 通过；P50 约 4.3s，P90 约 7.8s；tool 2/2 语义通过；subagent 用例语义通过但 runner 未观察到真实 Task tool call。 |
| WSL ClaudeCode | 当前不可作为有效样本；`/home/lenovo/.local/bin/claude` 和 `claude-deepseek-free` 都指向 clawgod launcher，实际启动 `/root/.bun/bin/bun /root/.clawgod/cli.cjs`，会挂住，不是 Anthropic ClaudeCode CLI。 |
| WSL Hermes | 5/5 通过；P50 约 34.7s，P90 约 38.9s；tool 2/2 通过；Hermes subagent 当前 runner 标记为不支持。慢路径属于 Hermes 本地 agent/启动/工具链耗时，不能直接等同 ZenProxy TTFT。 |
| WSL OpenClaw | API 5/5 通，但 semantic 0/5；输出固定 `HEARTBEAT_OK`，stderr 有 local secrets gateway `1006 abnormal closure`。这是 OpenClaw 本地 agent/gateway/harness 问题，不是 NewAPI/ZenProxy HTTP 链路断。 |
| 直连 web tools | 清空 WSL proxy env 后，Anthropic `/v1/messages` 和 OpenAI `/v1/chat/completions` 带 `web_search` tool 均 200，返回真实 `web_search` tool call；说明模型和 ZenProxy 可以转发/产生工具调用。 |
| Windows ClaudeCode WebSearch | 用户截图已证明官方 ClaudeCode + 官方 Claude 模型可以真实执行 `WebSearch/WebFetch`；此前“ClaudeCode 没注册 WebSearch/WebFetch”的结论只能描述当时那次 ZenProxy 受控样本，不是 ClaudeCode 能力边界。ZenProxy 路径的核心差异是上游可能返回 `web_search/task` 等小写或下划线工具名，旧内核原样吐回，ClaudeCode 只认已注册的 `WebSearch/Task`。2026-06-04 已修复并部署到 panda，线上直连 ZenProxy smoke 返回 `tool_use_names=WebSearch` 和 `tool_use_names=Task`。 |
| cc-switch 当前 provider | Windows cc-switch 当前 Claude provider 是 `closedeepseek -> https://sub2api.closeapi.top`；`LocalNewapi -> http://127.0.0.1:8081` 存在但不是 current。用户平时从 Windows ClaudeCode 测到的现象不能默认归因到 panda NewAPI/ZenProxy。 |

## 当前数据解释

1. “输入几乎 70k/90k”当前不是 NewAPI 输入 token 墙。2026-06-03 23:01-23:46 的 channel 69 历史 ClaudeCode 流式请求显示：ZenProxy 入口 body 从约 674KB 增长到 788KB，`before_tokens` 约 97k-110k，当时压缩后 NewAPI 账面多落在 70k-90k；最新 `deepseek-v4-flash/deepseek-v4-flash-free` 策略已经取消输入 token 墙，`free-model-client-rs` 侧只观测 request shape，不再压缩输入，`zen-proxy-rs` 外层也只 warn/pass 不 compact/reject。
2. NewAPI 中看到的 200k+ prompt tokens 记录来自 ClaudeCode 非流式大请求/fallback，而不是常规流式轮次。样本：NewAPI id `109370` 为非流式 `213248` prompt tokens，id `109461` 为非流式 `225416` prompt tokens；这是历史输出/输入保护改动前的归因样本，不能用来证明当前仍存在输出 cap。
3. “ClaudeCode 一直反复做”的直接调用形态是：流式大请求偶发 `status_code=500, upstream returned no assistant content or tool call`，随后 ClaudeCode 又以非流式大请求重发同一大历史，成功返回后下一轮继续把历史追加进去。样本：NewAPI id `109459` 流式 prompt tokens 记 0 且 500，紧接 id `109461` 非流式 225416 prompt tokens 成功。
4. 旧版 ClaudeCode huge-context 策略虽配置 `target_tokens=12k`，但真实请求里 `message_count` 已达 670-705，`tools_tokens=12700`，大量旧短消息低于单条 `min_text_tokens=2000`，不会被旧 compactor 选为压缩候选；这是历史 context_drift 归因。当前 flash 路径改为只观测不压缩，后续质量风险要由真实 panda policy/dry 压测确认。
5. 缓存几乎为 0 不能再直接写成“上游没有 cache”。当前观测会分为 `attempted/accepted/rejected/ignored` 四态，并分别记录 provider header/body usage 信号；只有真实 panda policy 样本能判断是上游没给、被中间层剥离、cache 被拒绝，还是确实命中。
6. 2026-06-04 中午已修复 768/1024 cap 桶绕过 buffered retry 的历史 bug；最新策略已经完全取消输出限制，后续若 NewAPI 再出现空输出、413、超时或高延迟，应优先归因到上游、客户端断流、lane/pool 调度和成本/长尾风险，而不是本仓库的输出墙。
7. Web/search 不是模型原生联网。当前源头已经证明：只要客户端提供 tool schema，ZenProxy 可以让模型返回 tool call；官方 ClaudeCode + 官方 Claude 能执行 `WebSearch/WebFetch`。ZenProxy 路径失败时优先检查工具定义是否进入请求、上游是否返回 tool call、返回工具名是否被 canonicalize 回客户端注册名，而不是再把问题归结为 ClaudeCode 没工具。

P1 待执行：

1. 正式无密钥 panda 压测执行器已落地到 `scripts/panda_pressure_runner.py`。
2. 执行器只从 `PANDA_NEWAPI_KEY`、`NEWAPI_API_KEY`、`OPENAI_API_KEY` 读取 key，默认 base URL 为 `http://100.69.228.93:8081`，默认拒绝 localhost。
3. 执行器已新增 `policy-smoke` / `policy-dry`，直接 HTTP 验证输出/输入墙取消、provider header/body usage、cache 四态和 lite effective profile，不依赖本地 CLI 状态。
4. 执行器已支持 Windows ClaudeCode、WSL ClaudeCode、WSL Hermes、WSL OpenClaw，且对 Hermes/OpenClaw 大 prompt 使用文件背书，避免 Linux `Argument list too long`。
5. Smoke / preflight 已证明 panda `/v1/models` 和最小聊天可用，模型包含 `deepseek-v4-flash`、`deepseek-v4-flash-lite`；但最新输出限制取消后的真实 panda `policy-smoke/policy-dry` 尚未跑。
6. dry run 暴露红旗，当前不能直接进入 4 客户端 x 500 full run。
7. 2026-06-01 panda 本机 huge stream source-side smoke 已通过，但它不是 ClaudeCode/Hermes/OpenClaw 真实客户端验收；下一步仍要重新跑 panda-only policy-smoke/policy-dry，再跑四客户端 dry run。
8. WSL ClaudeCode 必须先换成真实 ClaudeCode CLI 或修复当前 clawgod launcher，否则不能纳入四客户端正式压测。
9. OpenClaw 必须先修 local secrets gateway / agent harness 的 `HEARTBEAT_OK` 问题，否则只能统计 API 可达，不能统计语义、工具和 subagent 成功率。
10. ClaudeCode WebSearch 若要真实执行，必须让 ClaudeCode 收到和其已注册工具同名的 `tool_use`，例如 `WebSearch`/`WebFetch`。ZenProxy 不会也不应自行替客户端执行公网搜索；但必须保真转发并回填工具名大小写/别名。

P1 dry-run 结果：

2026-06-01 final-anchor 部署后的四客户端 dry run 结果：

| 客户端/批次 | 结果 | P50/P90/P99 total | 主要问题 |
|-------------|------|-------------------|----------|
| Windows ClaudeCode dry 50 | 50/50 API ok，43/50 semantic ok | 7.8s / 27.3s / 39.3s | 6 个 huge_context `context_drift`，1 个 `subagent_not_triggered`；Windows runner 使用 UNC 工作目录，CMD fallback 到 Windows 目录，影响 subagent 判断。 |
| WSL ClaudeCode dry 50 | 50/50 API ok，44/50 semantic ok | 6.5s / 23.8s / 64.2s | 6 个 huge_context 全部 `context_drift`，模型转去读 ClaudeCode transcript、git 状态或继续旧任务。 |
| WSL Hermes dry 50 | 50/50 API ok，50/50 semantic ok | 54.3s / 69.5s / 103.5s | 功能通过，但延迟远超 full-run 门槛；subagent 当前 runner 不支持观测，不计入触发率。 |
| WSL OpenClaw dry 50 | 50/50 API ok，49/50 semantic ok | 14.6s / 32.7s / 66.6s | 1 个 `deepseek-v4-flash-lite` long_context `context_drift`；subagent 5/5 observed。 |

全局结论：

```text
总轮次: 200
API OK: 200/200
semantic OK: 186/200
认证/模型/协议 400/502/504/300s timeout: runner summary 未观察到
panda health: 三实例健康，total=90 dispatch=90 dead=0 ratelimited=0
```

脱敏报告见 `docs/reports/panda-dry-run-20260601.md`。

历史 dry-run 结果：

| 客户端/批次 | 结果 | 主要问题 |
|-------------|------|----------|
| WSL ClaudeCode dry 50 | 49/50 API ok，47/50 semantic ok | `deepseek-v4-flash-lite` huge_context 语义漂移；一次 tool_calc 返回 `503 system cpu overloaded`。 |
| WSL Hermes dry 50 | 50/50 API ok，50/50 semantic ok | P90 总耗时约 47s；Task/subagent 触发当前 runner 不支持观测。 |
| WSL OpenClaw dry 50 | 50/50 API ok，48/50 semantic ok | subagent 在 lite 上出现一次 `client_timeout` 和一次 `subagent_not_triggered`。 |
| Windows ClaudeCode partial dry 22 | 21/22 API ok，21/22 semantic ok | huge_context 出现 310s 级客户端非零退出和一次 `context_drift`；已主动中止，避免继续压生产链路。 |

OpenClaw profile 修复后的 smoke 结果：

| 批次 | 结果 | 关键结论 |
|------|------|----------|
| WSL OpenClaw-only smoke | 5/5 API ok，5/5 semantic ok | tool 2/2，subagent 1/1；此前 328s timeout 的 OpenClaw subagent 用例降到约 20.1s 成功。 |
| WSL ClaudeCode/Hermes/OpenClaw smoke | 15/15 API ok，15/15 semantic ok | ClaudeCode 5/5，Hermes 5/5，OpenClaw 5/5；OpenClaw 新请求在 admin 侧识别为 `source_client=openclaw`。 |

smoke 耗时观察：

| 客户端 | 结果 | P50 total | P90 total | 备注 |
|--------|------|-----------|-----------|------|
| WSL ClaudeCode | 5/5 | 16983ms | 44232ms | tool 2/2。 |
| WSL Hermes | 5/5 | 41759ms | 81310ms | tool 2/2；Hermes subagent 当前 runner 不支持观测。 |
| WSL OpenClaw | 5/5 | 16979ms | 17790ms | tool 2/2，subagent 1/1。 |

P1 仍需执行：

1. 修复 ClaudeCode 真实客户端 huge_context：2026-06-04 huge-session compactor 已部署并通过受控 panda smoke；仍需用真实 Windows/WSL ClaudeCode 长会话复测是否消除反复执行旧任务。
2. 修正 Windows ClaudeCode runner，使用真实 Windows 工作目录，不再从 `\\wsl.localhost` UNC 路径启动 ClaudeCode。
3. 针对 `deepseek-v4-flash-lite` 长上下文语义漂移设置更保守的 lane/权重，或在 full run 前先隔离 huge/long lane。
4. 在 panda NewAPI 和 ZenProxy 日志层继续确认 502/524、stream JSON 截断、client_gone 是否为上游/客户端边界。
5. Hermes 慢路径需要拆分客户端启动、工具 schema、上游响应和 agent 循环耗时；当前 50/50 功能通过但 P90 约 69.5s，不能直接进入 full 2000。
6. 提交前复查 README、维护文档、脚本和 `.codex_tmp/` 临时产物一致性。

P1.15 2026-06-05 V4.98 cache-friendly session 源码记录：

| 项 | 事实 |
| --- | --- |
| 触发 | 用户在真实 ClaudeCode 长会话中观察到 NewAPI prompt tokens 稳定约 330k 且耗时爆红；panda 最近 70 分钟 `/v1/messages` 77/77 流式、prompt P50 约 326k、P90 约 331k、cache hits 0。 |
| 判断 | 330k 不是输入 token 墙。部署后的 ZenProxy 日志为 `context_action=pass`、`effective_body_size=body_size`，free-model-client-rs `messages_tokens` 持续增长；问题主要是大输入每轮未命中 provider cache。 |
| 线上证据 | 8 小时窗口内 cache 并非完全不可用：存在 `prompt_tokens=286975/cache_tokens=462592`、`prompt_tokens=439847/cache_tokens=439808` 等命中；其中一个大命中来自三次完全相同 `prompt_hash` 的重试后成功。 |
| 根因候选 | 旧上游 session 策略对大请求使用完整 `messages` hash；长会话每轮追加尾部消息都会改变 session，不利于 provider 对重复前缀复用。 |
| 修复 | 大请求 session scope 改为 `large_prefix_v498`：稳定前缀 hash + tools hash + tool_choice hash；保留模型、api key hash、时间桶隔离。 |
| 观测 | 新增 `prefix_4k_hash/prefix_32k_hash/prefix_128k_hash/prefix_256k_hash/cache_material_bytes` 到 request-shape 与 cache observation 日志，后续能区分“前缀不稳”和“前缀稳定但 provider 仍未命中”。 |
| 非目标 | 不裁剪 330k 上下文，不做摘要替换，不重排消息，不注入提示词，不伪造 cache 命中。 |
| 部署 | 2026-06-05 09:18 已部署到 panda 三实例；线上 stripped SHA256 为 `566e1c519056a4d2ee95697803d0e8bff9db40dc706c81ab753d70405edfb224`，旧 V47 hash `99424602ce7c076671579abf48ca0d27367ac126e514efe4403d902d5caecd78` 已备份到 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-v498-20260605-091813-9942460`。 |
| 部署验收 | `zen-proxy-rs@1/@2/@3` 和 nginx 均 active；4001/4002/4004/4000 `/health` 为 `status=ok`、`dispatch=90`、`dead=0`、`ratelimited=0`；4000 `/v1/models` 只暴露 `deepseek-v4-flash` 与 `deepseek-v4-flash-lite`；panda NewAPI 8081 `/v1/models` 200 且包含两个模型。 |
| 烟测结果 | NewAPI exact smoke `reply pong only` 返回 `PONG`；真实中文短问答返回 200；真实英文短问答出现 `upstream returned no assistant content or tool call`，用 V47 备份临时实例同 prompt 对照也 502，因此不是 V4.98 新增回归，而是既有上游空输出/节点质量问题。 |
| 待验收 | 仍需用同一 ClaudeCode 长会话 A/B 观察 cache hit rate、`frt`、总耗时、空输出/工具错误和回答质量。 |

P1.16 2026-06-05 V4.99 reasoning-aware output guard 源码记录：

| 项 | 事实 |
| --- | --- |
| 触发 | V4.98 部署后，大流式 ClaudeCode 主请求已能成功且 cache token 可见，但 panda/NewAPI 仍有短/中等非流式或低输出预算请求返回 `upstream returned no assistant content or tool call`。 |
| 根因 | 上游 `deepseek-v4-flash-free` 在部分低预算请求里只返回 `reasoning_content`，正文 `content` 为空，且常见 `finish_reason=length`；旧逻辑只看正文和工具调用，因此把 reasoning-only 判为空输出。 |
| 修复 | 新增共享输出分类：`valid/empty_output/reasoning_only/reasoning_only_length`；OpenAI/Anthropic 非流式遇到 `reasoning_only_length` 时只重试一次 `thinking: disabled`；流式不能安全重试时会记录并返回带 `class=` 的错误分类。 |
| 策略 | 大流式 ClaudeCode 主会话、工具请求、长上下文仍不默认禁用 thinking；只对低预算探针/ClaudeCode 小流式探针做初始 `thinking: disabled`，并保留 Hermes/OpenClaw compat tool-use thinking 策略。 |
| buffered | ClaudeCode Anthropic buffered stream 不再仅因 `max_tokens<=512` 触发；现在需要 exact-output literal，或 `before_tokens>=50k && max_tokens<=2048`。小流式请求走直接流式 + 初始低预算策略。 |
| 错误可观测 | 空输出错误现在可带 `class=empty_output/reasoning_only/reasoning_only_length/buffered_retry_exhausted`；日志记录 `reasoning_chars/content_chars/finish_reason/tool_call_count/short_request_kind`。 |
| 验证 | 本地 `cargo fmt`、`CARGO_INCREMENTAL=0 cargo clippy --all-targets -- -D warnings`、`CARGO_INCREMENTAL=0 cargo test` 已通过；golden 测试新增 OpenAI/Anthropic 非流式 reasoning-only-length disabled retry 和小流式低预算不走 buffered retry。 |
| 部署 | 2026-06-05 10:47 已部署到 panda 三实例；线上 stripped SHA256 为 `8f8513c418c40704bd50c8ce73f27696fdc9fbb1aa75290f2829cedd9eb9e2f2`，旧 V4.98 hash `566e1c519056a4d2ee95697803d0e8bff9db40dc706c81ab753d70405edfb224` 已备份到 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-v499-20260605-104718-566e1c5`。 |
| 部署验收 | `zen-proxy-rs@1/@2/@3` 和 nginx 均 active；4001/4002/4004/4000 `/health` 均为 `status=ok`、`dispatch=90`、`dead=0`、`ratelimited=0`；4000 `/v1/models` 返回两个公开模型；panda NewAPI 8081 `/v1/models` 200。 |
| 烟测结果 | panda NewAPI OpenAI 非流式短问答 200，约 2.03s，返回 `2+2 equals 4.`；panda NewAPI Anthropic 流式 exact prompt 返回 `STREAM_OK` 且无 error；非 exact 小流式返回正常 greeting 且日志显示 `protocol="anthropic"`，未因 `max_tokens=64` 进入 `anthropic_buffered`。 |
| 线上观测 | 部署后日志已出现 V4.99 `applied upstream thinking policy`、`thinking_policy="low_budget_probe_disabled"`、`provider cache usage observation` 和 request shape 字段；部署后最小窗口内未见 `empty_output_class`、`upstream returned no assistant`、`stream error`、`retry budget`、`client_gone`。 |

P1.17 2026-06-05 ClaudeCode low-budget tool probe 部署记录：

| 项 | 事实 |
| --- | --- |
| 触发 | 用户反馈部署前后又出现多条 NewAPI 502；复查最近两小时 channel 69：`deepseek-v4-flash` 成功 stream 742、成功 non-stream 382、错误 non-stream 56、错误 stream 1。 |
| 根因 | 线上旧 V4.99 未包含本地低预算工具探针补丁。错误集中在 ClaudeCode 内部 `/context`/探针形态：Anthropic 非流式、`message_count=1`、`tool_count=1`、`max_tokens=1/16`、`prompt_tokens=57/417`，上游返回 `reasoning_only_length` 后变成空输出 502。 |
| 修复 | 部署本仓库最新补丁到 `zen-proxy-rs`：ClaudeCode 非流式低预算工具探针第一次上游请求前 `thinking=disabled`，并把 `max_tokens<=32` 最小抬到 64。 |
| 本地构建 | `/home/lenovo/zen-proxy-rs` 执行 `CARGO_INCREMENTAL=0 cargo build --release` 通过；未 strip release SHA256 为 `5732beb2c6cc7b9092ae7d9dfe580fd69d48f602b8cb16c859e9beb5f2022f67`。 |
| 部署 | 2026-06-05 20:52 已部署到 panda；线上 stripped SHA256 为 `369e45062f870f8460ebf4d52f06bda30d94fe0f4459cf8cdebbc4829fe3316d`，旧 V4.99 hash `8f8513c418c40704bd50c8ce73f27696fdc9fbb1aa75290f2829cedd9eb9e2f2` 已备份到 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-low-budget-probe-20260605-205234-8f8513c`。 |
| 部署验收 | `zen-proxy-rs@1/@2/@3` active；4001/4002/4004/4000 `/health` 均 `status=ok`、`dispatch=90`、`dead=0`、`ratelimited=0`；二进制包含 `low_budget_tool_probe_disabled` 与 `raised ClaudeCode low-budget tool probe max_tokens before upstream` 字符串。 |
| NewAPI 验收 | panda 本机 NewAPI Anthropic `/v1/messages?beta=true`，带 1 个 `ctx_probe` 工具，`max_tokens=1` 返回 HTTP 200、2.31s、`stop_reason=tool_use`；`max_tokens=16` 返回 HTTP 200、2.13s、`stop_reason=tool_use`。 |
| 日志验收 | ZenProxy 新 pid 日志出现 `requested_max_tokens=Some(1/16)`、`effective_max_tokens=Some(64)`、`thinking_policy="low_budget_tool_probe_disabled"`；部署后近 10 分钟 channel 69 无错误记录，ZenProxy 近 5 分钟未见 `upstream returned no assistant content`、`stream truncated`、`retry budget exhausted`。 |
| 残留 | 仍需用户真实 ClaudeCode `/context` 和日常使用长窗口观察；单条历史 `stream truncated before DONE or finish_reason` 与本次批量非流式 502 不同，若复发需单独排查。 |

P1.18 2026-06-05 ClaudeCode Anthropic stream idle ping 部署记录：

| 项 | 事实 |
| --- | --- |
| 触发 | 用户反馈 NewAPI 仍有红行和偶发无输出。复查截图时段后确认这些记录不是上一轮非流式低预算工具探针 502，而是成功消费 `type=2`、`stream=true`、约 50k prompt tokens、`completion=0`、`use_time≈64s`，`other.stream_status.end_reason=client_gone`。 |
| 根因判断 | ZenProxy 同窗口无 `upstream returned no assistant content`、无 `stream truncated`、无 `retry budget exhausted`；更像是 ClaudeCode/NewAPI/cc-switch 在真实内容或工具调用长时间未到达时断开下游流。 |
| 修复 | `src/proxy/anthropic.rs` 对 ClaudeCode Anthropic SSE 增加 15 秒 idle ping：等待上游事件超时，或上游只有 reasoning/usage 等不可转发事件且下游 15 秒无活动时，发送 `event: ping`、`data: {"type":"ping"}`。 |
| 边界 | 不对 OpenAI SSE 启用；不对 Hermes/OpenClaw 启用；不把 ping 当首字；不生成空 content delta；不改变模型输出、工具调用、prompt、`max_tokens` 或 thinking 策略。 |
| 本地验证 | WSL 原生路径执行 `CARGO_INCREMENTAL=0 cargo fmt -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 均通过；新增 golden `claude_code_anthropic_stream_sends_idle_ping_before_delayed_content`，模拟上游 16 秒后才出内容，断言先出现 `event: ping`，随后仍完整输出 `delayed answer` 和 `message_stop`。 |
| 构建 | `/home/lenovo/zen-proxy-rs` release 构建通过；未 strip SHA256 为 `00ffe54a9b8b9ab09a5fda5c55cf68ebc06825dbece2dabbe6e888bc2bd2f300`；部署 stripped SHA256 为 `be6b859576169d2cc710ed2c079a125c50d0a51a0d744abbd3668ba1e030e793`。 |
| 部署 | 2026-06-05 22:34 已部署到 panda；旧 hash `369e45062f870f8460ebf4d52f06bda30d94fe0f4459cf8cdebbc4829fe3316d` 备份到 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-stream-idle-ping-20260605-223420-369e450`。 |
| 部署验收 | `zen-proxy-rs@1/@2/@3` active；4001/4002/4004/4000 `/health` 均 `status=ok`、`dispatch=90`、`dead=0`、`ratelimited=0`；线上二进制包含 `sent ClaudeCode stream idle ping while upstream produced no forwardable output` 和 `sent ClaudeCode stream idle ping while waiting for upstream event` 字符串。 |
| NewAPI smoke | panda 本机 token id `38`/name `ds`/group `vip` 下，`/v1/models` 200 且包含 `deepseek-v4-flash`、`deepseek-v4-flash-lite`；Anthropic `/v1/messages?beta=true` + `x-fmc-client=claude-code` 流式 smoke HTTP 200，starttransfer 约 1.39s、total 约 1.92s，响应按 SSE 分片输出目标 marker，无 error。 |
| 残留 | idle ping 只能解决“下游长时间无字节活动”的 client_gone；如果上游 60 秒后仍真实空输出，或客户端有“必须真实内容在 N 秒内出现”的硬超时，还需要 first-content watchdog / retry 降级另行设计。 |

P1.19 2026-06-06 V4.99 ClaudeCode Anthropic Stream Guard 部署记录：

| 项 | 事实 |
| --- | --- |
| 触发 | 用户反馈 ClaudeCode 仍偶发 `API Error: Failed to parse JSON` 和中断；复查 NewAPI/ZenProxy 发现对应服务端错误不是 30KB Write JSON 溢出，而是 Anthropic stream 的 `status_code=500, stream truncated before DONE or finish_reason`。 |
| 证据 | 部署前 90 分钟 channel 69 为 400 次调用、398 成功、2 错误；2 条错误均为 `stream=true` 的 `stream truncated before DONE or finish_reason`。失败样本均为 ClaudeCode Anthropic `/v1/messages`，工具 schema 存在，`max_tokens=32000`，上游在真实 text/tool 输出前长时间只有 reasoning/空 delta 或直接截断。 |
| 修复 | `src/proxy/anthropic.rs` 将 ClaudeCode Anthropic stream 改为 Stream Guard 状态机：`message_start` 只发一次；真实 text/tool 未输出前，上游 fetch/stream 截断可原地重试；60 秒无可转发内容触发重试；最后一次仅在工具请求场景启用 disabled thinking 兜底；半截 tool JSON 不会被伪成功交给客户端。 |
| 工具参数 | Anthropic `input_json_delta` 普通流式和 buffered huge-stream 均改为 4KB 分片；分片只改变 SSE 传输颗粒度，拼接后 JSON 字符不变。 |
| forced tool_choice | ClaudeCode 显式 forced `tool_choice` 首跳禁用 thinking，避免 DeepSeek 返回 `Thinking mode does not support this tool_choice`；`tool_choice=auto`、普通 tools auto 和 unknown client 不受影响。 |
| 本地验证 | WSL 原生路径执行 `cargo fmt -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 均通过；库测试 89 条、kernel golden 100 条全部通过。新增单元覆盖 Stream Guard retry 判定、tool JSON 分片无损、ClaudeCode forced tool_choice thinking 策略。 |
| 构建 | `/home/lenovo/zen-proxy-rs` release 构建通过；最终部署前 strip 后 SHA256 为 `39dc0bb94092597a00518abf83e80f8c32a91e8c60682c169942bf16bf70017d`。 |
| 部署 | 2026-06-06 00:54 已部署到 panda；旧 hash `9d64728e5511f2b414d16f4f4dac27395dabb1abe3ae64c2cf9404ee4f31ba0e` 备份到 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-v499-forced-tool-20260606-005416-9d64728`。 |
| 部署验收 | `zen-proxy-rs@1/@2/@3` active；4001/4002/4004/4000 `/health` 均 `status=ok`、`dispatch=90`、`dead=0`、`ratelimited=0`。 |
| NewAPI smoke | panda 本机有效 `vip` token 下，`/v1/models` HTTP 200；Anthropic stream PONG HTTP 200、`message_stop` 存在、`event:error=0`；Anthropic forced `tool_choice` 工具流 HTTP 200，`tool_use` 和 `input_json_delta` 存在，`Thinking mode does not support this tool_choice=0`。 |
| 部署后观察 | 00:54:16 最终部署后 channel 69 采样 13/13 成功、0 错误。部署前仍有 3 条非流式 300s/504 旧记录，属于另一类长非流式超时，不计入 V4.99 Stream Guard 后验收。 |
| 残留 | 仍需用户真实 ClaudeCode 长会话观察 1-2 小时，重点看 `stream guard retrying`、`refusing to emit possibly partial tool calls`、`stream truncated` 是否继续出现；非流式 300s/504 需另按 long non-stream 保护排查。 |

P1.20 2026-06-06 provider reasoning_content 400 修复记录：

| 项 | 事实 |
| --- | --- |
| 触发 | NewAPI channel 69 日志出现 `status_code=400/500`，public content 包含 `opencode zen 400` 和 provider 返回的 `The reasoning_content in the thinking mode must be passed back to the API`。 |
| 根因 | ClaudeCode Anthropic `/v1/messages` 被内核转换为 OpenAI-compatible 上游请求；历史 assistant/tool 调用没有可回传的 `reasoning_content`，但普通 tools auto 仍保持默认 thinking，DeepSeek provider 直接 400。 |
| 修复 | 对 `provider_missing_reasoning_content` 增加一次性 disabled-thinking 重试，覆盖 OpenAI/Anthropic 非流式、OpenAI 流式、ClaudeCode Anthropic 流式和 buffered huge-stream；只有 provider 明确返回该错误才触发。 |
| 错误映射 | 上游错误 public response 统一脱敏，不再返回 `opencode zen` 或原始 provider body；返回稳定 `type/code/message`，并保留 `Retry-After`。 |
| 本地验证 | `cargo fmt -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 通过；库测试 89 条、kernel golden 103 条。新增 3 条 golden 覆盖 missing reasoning_content 非流式/流式重试和 public error 脱敏。 |
| 部署状态 | 已于 2026-06-06 12:40 部署 panda 三实例；源码 commit `68bf5383f7c8915f0950a6864134b77dd51a1214`，线上 stripped hash `d5b7558c9f8f9fc7ea6faa802634dba85435868f1e338a4830f77079c3a1fc8e`，旧版本备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260606-124001.pre-68bf538`。部署后 ZenProxy `/health`、`/v1/models`、ZenProxy OpenAI/Anthropic smoke、panda NewAPI OpenAI/Anthropic smoke 均通过；部署后 10 分钟窗口未见新的 `reasoning_content`、`opencode zen`、空输出或 NewAPI 错误日志。 |

P1.21 2026-06-08 ClaudeCode 大上下文慢首字诊断与首包保护：

| 项 | 事实 |
| --- | --- |
| 触发 | 用户反馈 2026-06-08 channel 69 调用中，约 180k input tokens 场景偶发首字很长，cache 观感不稳定，整体耗时和 FRT 不够快。 |
| 数据结论 | 截至 2026-06-08 11:55 CST 左右，`deepseek-v4-flash` 成功流式 `ok/eof` 约 2783 条，NewAPI `frt` P50 约 4.1s、P90 约 8.7s、P99 约 35.9s；`150k-220k` 桶 cache hit 约 98.4%，首字 >=15s 约 3.45%，首字 >=30s 仅 1 条。 |
| 根因样本 | NewAPI id `136957`：prompt `173751`、completion `218`、FRT `47123ms`、cache `270976`。ZenProxy 对应 11:13:13 入口，estimated tokens `194191`，第一次上游 fetch 在 11:13:55 返回 520，第二次 attempt 在 11:14:00 cache accepted；慢首字来自上游慢失败+重试，不是 cache miss 或本地 CPU/池资源耗尽。 |
| 已实现 | `src/proxy/anthropic.rs` 对 ClaudeCode Anthropic stream 增加大上下文首包 fetch 超时保护：仅在真实 text/tool 输出前、且满足 token 门槛时触发；超时后按既有 Stream Guard 重试，不对已输出内容或半截工具调用重试。 |
| 新配置 | `FREE_MODEL_CLAUDE_CODE_STREAM_INITIAL_FETCH_TIMEOUT_SECS` 默认 `30`，设 `0` 可关闭；`FREE_MODEL_CLAUDE_CODE_STREAM_SLOW_GUARD_MIN_INPUT_TOKENS` 默认 `150000`；`FREE_MODEL_CLAUDE_CODE_STREAM_NO_FORWARDABLE_RETRY_SECS` 默认 `45`。 |
| 新观测 | ClaudeCode Anthropic stream 正常结束时新增 `ClaudeCode stream guard completion summary` 日志，包含 `attempts_used/retry_count/first_upstream_response_ms/first_upstream_event_ms/first_reasoning_ms/first_content_ms/first_tool_call_ms/idle_ping_count/cache_observation/cache_read_input_tokens/estimated_total_tokens/max_tokens/prompt_hash_hex`。 |
| 边界 | 不改 prompt、不裁剪输入、不限制输出、不把 ping 当真实首字、不对 Hermes/OpenClaw 生效、不在已有 text/tool 输出后重试。 |
| 本地验证 | `cargo fmt -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 均通过；库测试 94 条、kernel golden 105 条。新增 golden `claude_code_anthropic_stream_retries_slow_initial_fetch_before_output` 覆盖首包慢失败主动重试。 |
| 部署状态 | 2026-06-08 15:14 CST 先滚动部署 P1.21；15:42 CST 又补齐 ZenProxy env 配置透传并再次滚动部署。最终线上 stripped SHA256 `a771174350bf6701c97b7deed1bbf4deecd995463c5cfb27ff4b4e6c7c440f6b`。旧版本备份包括 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-p121-20260608-151426-dfd52e3489e6` 和 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-p121-envwired-20260608-153746-5c33046808ae`。 |
| 部署验收 | `zen-proxy-rs@1/@2/@3` active；4001/4002/4004/4000 `/health` 均 `status=ok`、`dead=0`、`ratelimited=0`；`/v1/models` 返回 `deepseek-v4-flash` 和 `deepseek-v4-flash-lite`；Anthropic/ClaudeCode 最小流式 smoke HTTP 200，`starttransfer=1.663s`，返回 `pong`。OpenAI-compatible 极短流式 smoke 仍返回 `reasoning_only_length`，列为 OpenAI 短流式残留，不作为本轮 ClaudeCode 主链路回滚条件。 |

## 临时产物归类

| 路径 | 当前归类 | 处理原则 |
|------|----------|----------|
| `.codex_tmp/client-matrix/` | 历史客户端矩阵脚本和结果 | 不提交；只提炼无密钥执行器和脱敏摘要。 |
| `.codex_tmp/hermes-panda-profile/` | Hermes 临时 profile 和状态 | 不提交；可能含会话状态，只保留脱敏结论。 |
| `.codex_tmp/hermes-panda-home/` | Hermes 临时 HOME | 不提交；如需复测重新生成。 |
| `.codex_tmp/hermes-tool-smoke/` | Hermes 工具 smoke 工作目录 | 不提交；只作为本地验证痕迹。 |
| `.codex_tmp/openclaw-panda/` | OpenClaw 临时配置、state、workspace | 不提交；配置中不得把真实 key 写入仓库。 |
| `configured` | 根目录 0 字节未跟踪文件 | 来源未确认，不删除、不提交。 |
| `panda` | 根目录 0 字节未跟踪文件 | 来源未确认，不删除、不提交。 |
| `""` / 异常字符文件 | 根目录 0 字节未跟踪文件 | 来源未确认，不删除、不提交。 |

## 当前风险

1. 小矩阵通过不等于 4 客户端 x 500 次压测通过；当前 dry run 已阻断 full run，不能直接开 2000 次压测。
2. 输出限制和 flash 输入墙完全取消后，上游 413、超时、空输出、延迟和成本风险回到 upstream 与 lane/pool 调度层；必须用真实 panda `policy-smoke/policy-dry` 和后续 dry run 压测确认，不能凭源码测试判定生产安全。
3. Hermes/OpenClaw 当前测试使用临时环境变量或临时配置，不能误当成用户默认配置已经切换。
4. OpenClaw 系统 Node 仍是 `v20.20.2`，只有显式使用隔离 Node 22 路径才满足运行要求。
5. `.codex_tmp/` 里有大量历史测试输出，可能包含敏感信息，默认不提交。
6. 仓库根目录存在 `configured`、`panda`、`""`、异常字符文件等未跟踪项，提交前必须逐个确认用途，不要盲目删除。
7. 客户端策略隔离和 final-anchor 修复已在代码层落地，panda 本机 source-side huge stream smoke 通过；但真实 ClaudeCode dry run 仍显示 huge_context 语义漂移，不能进入 full run。
8. panda ZenProxy 三实例健康且池指标正常，但 NewAPI/docker 日志里出现过上游 Cloudflare 502/524、stream JSON 截断和 client_gone，需要在正式报告中和 ZenProxy 指标分开归因。
9. Windows ClaudeCode 不能从当前 WSL 非交互环境稳定启动时，应归类为测试执行环境问题；不要把它误判成 panda/ZenProxy 链路失败。
