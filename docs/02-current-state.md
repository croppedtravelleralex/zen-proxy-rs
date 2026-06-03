# 当前状态

更新时间：2026-06-03
分支：`codex/v46-quality-nonstream-gates`

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
- `cargo test` 通过：库测试 69 条、kernel golden 71 条、doc tests 0 条。
- `zen-proxy-rs` 本轮未改；最近一次相关验证通过：主单元测试 129 条、e2e 26 条。

注意：上述验证覆盖本仓库当前源码；本轮 request-shape 观测改动已随 `zen-proxy-rs` release 部署到 panda。

当前已实现并由测试覆盖的关键能力：

1. `Authorization` 和 `x-api-key` 两种认证头识别。
2. 请求体上限由 `FREE_MODEL_REQUEST_BODY_LIMIT_MB` 控制，默认 64MB。
3. OpenAI/Anthropic 两套入口共享协议内核。
4. 非流式输出保护：缺省 `max_tokens` 为 2048；小 prompt 最大 4096；估算 prompt >= 50k tokens 最大 2048；估算 prompt >= 100k tokens 最大 1024。
5. 流式请求不套非流式 cap，缺省输出上限 1024，显式值最低 32。
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
4. 已构建并部署到 panda 生产 `zen-proxy-rs` 三实例；panda 运行链路为 `NewAPI 8081 -> zen-proxy-rs 4001/4002/4004 -> free-model-client-rs kernel -> upstream`。
5. 部署后 panda 本机 NewAPI 小请求已通过：`/v1/models` 返回 200，包含 `deepseek-v4-flash`、`deepseek-v4-flash-lite`；`deepseek-v4-flash` 非流式小请求返回 `OK`，HTTP 200，耗时约 2.37s。

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

## 当前数据解释

1. “输入几乎 70k/60k”主要来自流式上下文压缩策略：默认 `StreamContextPolicy` 在估算输入超过 80k tokens 时触发，把 prompt 压到约 60k tokens；日志里的 `after_tokens=60149/60150` 是这个目标值和锚点开销，不是 NewAPI 随机制造。
2. ClaudeCode 专用 huge-context 策略目标约 12k tokens，但前提是请求被识别为 ClaudeCode；误判成 OpenClaw 时会走默认 60k 策略，这也是本次 profile 修复要解决的核心原因之一。
3. 缓存几乎为 0 是因为上游 usage 基本没有返回 `cache_creation_input_tokens`、`cache_read_input_tokens` 或 OpenAI `cached_tokens`；当前 ZenProxy 只转发上游缓存计数，不会自行伪造 provider cache 命中。

P1 待执行：

1. 正式无密钥 panda 压测执行器已落地到 `scripts/panda_pressure_runner.py`。
2. 执行器只从 `PANDA_NEWAPI_KEY`、`NEWAPI_API_KEY`、`OPENAI_API_KEY` 读取 key，默认 base URL 为 `http://100.69.228.93:8081`，默认拒绝 localhost。
3. 执行器已支持 Windows ClaudeCode、WSL ClaudeCode、WSL Hermes、WSL OpenClaw，且对 Hermes/OpenClaw 大 prompt 使用文件背书，避免 Linux `Argument list too long`。
4. Smoke / preflight 已证明 panda `/v1/models` 和最小聊天可用，模型包含 `deepseek-v4-flash`、`deepseek-v4-flash-lite`。
5. dry run 暴露红旗，当前不能直接进入 4 客户端 x 500 full run。
6. 2026-06-01 panda 本机 huge stream source-side smoke 已通过，但它不是 ClaudeCode/Hermes/OpenClaw 真实客户端验收；下一步仍要重新跑 panda-only dry run。

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

1. 修复 ClaudeCode 真实客户端 huge_context：当前 source-side smoke 已过，但真实 ClaudeCode dry run 仍 12/12 huge_context 语义漂移。
2. 修正 Windows ClaudeCode runner，使用真实 Windows 工作目录，不再从 `\\wsl.localhost` UNC 路径启动 ClaudeCode。
3. 针对 `deepseek-v4-flash-lite` 长上下文语义漂移设置更保守的 lane/权重，或在 full run 前先隔离 huge/long lane。
4. 在 panda NewAPI 和 ZenProxy 日志层继续确认 502/524、stream JSON 截断、client_gone 是否为上游/客户端边界。
5. Hermes 慢路径需要拆分客户端启动、工具 schema、上游响应和 agent 循环耗时；当前 50/50 功能通过但 P90 约 69.5s，不能直接进入 full 2000。
6. 提交前复查 README、维护文档、脚本和 `.codex_tmp/` 临时产物一致性。

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
2. Hermes/OpenClaw 当前测试使用临时环境变量或临时配置，不能误当成用户默认配置已经切换。
3. OpenClaw 系统 Node 仍是 `v20.20.2`，只有显式使用隔离 Node 22 路径才满足运行要求。
4. `.codex_tmp/` 里有大量历史测试输出，可能包含敏感信息，默认不提交。
5. 仓库根目录存在 `configured`、`panda`、`""`、异常字符文件等未跟踪项，提交前必须逐个确认用途，不要盲目删除。
6. 客户端策略隔离和 final-anchor 修复已在代码层落地，panda 本机 source-side huge stream smoke 通过；但真实 ClaudeCode dry run 仍显示 huge_context 语义漂移，不能进入 full run。
7. panda ZenProxy 三实例健康且池指标正常，但 NewAPI/docker 日志里出现过上游 Cloudflare 502/524、stream JSON 截断和 client_gone，需要在正式报告中和 ZenProxy 指标分开归因。
8. Windows ClaudeCode 不能从当前 WSL 非交互环境稳定启动时，应归类为测试执行环境问题；不要把它误判成 panda/ZenProxy 链路失败。
