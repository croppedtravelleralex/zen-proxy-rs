# panda-only 四客户端压测方案

更新时间：2026-06-04

## 目标

只通过 panda NewAPI 做正式混合压力测试，不再使用本机 WSL NewAPI 或本机 `127.0.0.1:8081` 代替生产链路。

目标链路：

```text
client -> panda NewAPI http://100.69.228.93:8081 -> configured upstream
```

总规模：

```text
Windows ClaudeCode: 500 次
WSL ClaudeCode: 500 次
WSL Hermes: 500 次
WSL OpenClaw: 500 次
总计: 2000 次
```

## 硬约束

1. API key 只从环境变量或用户已配置的 cc-switch/NewAPI 配置读取，报告统一写 `sk-***`。
2. 不永久修改 Hermes、OpenClaw、ClaudeCode 用户默认配置；必须使用临时 profile、临时 env 或测试后可恢复的配置。
3. 不提交 `.codex_tmp/`、临时配置、原始大日志、密钥、完整请求体和完整响应体。
4. 不执行真实公网攻击、凭据窃取、持久化、绕过检测或武器化 payload；安全能力测试只使用本地靶场、模拟 prompt、授权测试文件和防御性分析。
5. 每一阶段先跑 smoke，再跑 dry run，再跑 full run；任何客户端 smoke 失败，不进入该客户端 full run。

## 客户端矩阵

| 客户端 | 连接方式 | 配置纪律 | 主要验收点 |
|--------|----------|----------|------------|
| Windows ClaudeCode | 用户已配置 cc-switch -> panda NewAPI | 不由仓库脚本永久改配置 | ClaudeCode CLI 是否稳定、Task/subagent 是否触发、长上下文是否超时 |
| WSL ClaudeCode | WSL 侧 cc-switch 或临时 env -> panda NewAPI | 测前记录配置来源，测后确认未污染 | WSL 开发链路、工具调用、长会话连续性 |
| WSL Hermes | `CUSTOM_BASE_URL=http://100.69.228.93:8081/v1` + 临时 key env | 不写入默认 `~/.hermes/config.yaml` | provider custom、文件/终端工具、web 工具断言 |
| WSL OpenClaw | 临时 `OPENCLAW_CONFIG_PATH` + 隔离 Node 22 | 不覆盖系统 Node，不写默认配置 | models list、infer、agent、exec/write/read/web_fetch |

## 每客户端 500 次组成

| 类型 | 次数 | 目的 |
|------|------|------|
| 短请求 stream | 120 | 测短请求首字、短指令服从、普通对话稳定性 |
| 短请求 non-stream | 60 | 测无输出 cap 后的短输出、usage 透传、NewAPI 兼容性 |
| 中上下文 1k-10k | 80 | 测常规工程上下文和工具 schema 压力 |
| 长上下文 10k-50k | 70 | 测 first_content、总耗时、上下文保持 |
| 超长上下文 50k-200k | 40 | 测长请求隔离、非流式保护、客户端超时边界 |
| 工具调用 | 70 | 测 Bash/Read/Write/exec/web_fetch 等工具成功率 |
| Task/subagent 或等价 agent 任务 | 30 | 测明确要求分派子任务时是否触发 |
| 错误与边界 | 30 | 测认证失败、模型不存在、畸形工具历史、空输出、stream decode 保护 |

合计每客户端 500 次。客户端不支持某一类能力时，必须记录为 `not_supported`，不能静默替换成普通短请求。

## 阶段安排

1. Preflight：
   - 确认 panda `/v1/models` 可达。
   - 确认模型包含 `deepseek-v4-flash` 和 `deepseek-v4-flash-lite`。
   - 确认 key 脱敏、代理环境变量、客户端配置来源。
   - 确认 OpenClaw 使用隔离 Node 22。
2. Smoke：
   - 每客户端 5 次：models、PONG、stream、non-stream、一个工具用例。
   - 任一客户端出现认证、模型、连接或配置污染问题，先停。
3. Dry run：
   - 每客户端 50 次，按 full run 比例抽样。
   - 用来校准超时、日志字段、错误分类和结果文件大小。
4. Full run：
   - 每客户端 500 次。
   - 分客户端独立输出原始结果，统一生成汇总报告。
5. Post-check：
   - 检查默认配置是否被污染。
   - 检查 `.codex_tmp/` 是否有密钥、完整请求体、完整响应体。
   - 只把脱敏摘要写入 `docs/`。

## 策略 smoke/dry harness

四客户端 CLI 压测之外，`scripts/panda_pressure_runner.py` 现在提供直接 HTTP policy harness，用来先验收本轮策略信号，不依赖本地 ClaudeCode/Hermes/OpenClaw CLI 状态。

当前状态：harness 已落地；2026-06-04 18:54 已将输出限制取消、flash/free 输入放行和 cache 四态观测部署到 panda，并完成手工 NewAPI smoke 与大上下文不折叠 smoke。真实 panda `policy-smoke` / `policy-dry` 尚未跑；不得把手工 smoke 写成生产压测已验证。

运行方式：

```bash
cd /home/lenovo/free-model-client-rs
PANDA_NEWAPI_KEY=sk-*** python3 scripts/panda_pressure_runner.py --mode policy-smoke
PANDA_NEWAPI_KEY=sk-*** python3 scripts/panda_pressure_runner.py --mode policy-dry --timeout-ms 300000
```

如果要把 provider header 作为硬门槛：

```bash
PANDA_NEWAPI_KEY=sk-*** python3 scripts/panda_pressure_runner.py --mode policy-smoke --require-provider-header
```

policy harness 固定覆盖 OpenAI `/v1/chat/completions` 和 Anthropic `/v1/messages` 两类协议，每类协议各跑：

| case_type | 目的 | 关键断言 |
|-----------|------|----------|
| `flash_input_room` | 验证 `deepseek-v4-flash` 没有输入墙 | 大 prompt 不返回 400/413/422，`input_wall_ok=true`。 |
| `flash_output_room` | 验证 `deepseek-v4-flash` 没有输出墙 | 输出 token 达到门槛，`finish_reason` 不能是 `length/max_tokens`，`output_wall_ok=true`。 |
| `lite_not_claudecode` | 验证 `deepseek-v4-flash-lite` 不走 ClaudeCode 适配 | 请求带 `x-fmc-client=claude-code` 和 `Task` 工具；结果字段预期 `expected_source_client=claude-code`、`expected_effective_client=unknown`，线上日志需按 `request_shape_hash` 对齐确认。 |
| `provider_usage_probe` | 验证 provider body usage 信号 | 非流式简单请求必须返回 `provider_body_usage_signal=true` 和 usage token 字段。 |
| `cache_probe` | 验证缓存观测分类 | 输出 `cache_observation=attempted/accepted/rejected/ignored`，不伪造 cache 命中。 |

样本数据字段：

```text
request_id
protocol
endpoint
model
stream
case_type
x_fmc_client
expected_source_client
expected_effective_client
request_shape_hash
request_shape_estimated_total_tokens
request_shape_tool_name_classes
prompt_est_tokens
request_body_bytes
max_tokens
status_code
api_ok
policy_ok
provider_header_signal
provider_header_names
provider_body_usage_signal
usage_input_tokens
usage_output_tokens
usage_cached_tokens
usage_cache_read_tokens
usage_cache_creation_tokens
cache_attempted
cache_observation
input_wall_ok
output_wall_ok
finish_reason
redaction_ok
```

其中 provider response usage 信号由 `usage` 原文及 `usage_input_tokens/usage_output_tokens/usage_cached_tokens/...` 派生；provider header/body usage 信号分别由 `provider_header_signal/provider_header_names` 和 `provider_body_usage_signal` 表示。

验收标准：

1. `policy-smoke` 至少产生 10 条记录，OpenAI/Anthropic 两协议均覆盖，`redaction_ok=true`。
2. `flash_input_room` 全部 `api_ok=true` 且 `input_wall_ok=true`。
3. `flash_output_room` 全部 `output_wall_ok=true`；dry 模式输出门槛高于历史 1024 token 桶，用来暴露旧输出墙回归。
4. `lite_not_claudecode` 记录必须显示 `expected_source_client=claude-code`、`expected_effective_client=unknown`；同时用服务端 `desensitized request shape before upstream` 日志按 `request_shape_hash` 对齐，确认 `source_client=ClaudeCode`、`effective_client=Unknown`。
5. `cache_probe` 必须给出 `cache_observation` 分类：`accepted` 表示 usage cache token > 0，`attempted` 表示 provider 返回 cache 字段但为 0，`rejected` 表示 provider 明确拒绝 cache 控制，`ignored` 表示未返回 cache 字段。
6. 两协议都应有 provider response/body usage 信号；如果验证 provider header 透传，使用 `--require-provider-header`，否则报告必须说明 NewAPI 是否剥离了 `x-zen-observed-exit-ip` 等 header。
7. 任何 `policy_ok=false`、`auth_error`、`model_error`、`network_error` 或 `redaction_ok=false` 都阻断后续四客户端 dry/full。

补充本地验收边界：

- `zen-proxy-rs` e2e 已覆盖 flash/free 外层输入放行：大旧工具结果请求在 `ZEN_COMPACTOR_MODE=enforce` 下返回 `x-zen-context-action=warn`、`x-zen-context-trimmed=false`，上游仍看到原始大 tool content。
- `zen-proxy-rs` e2e 也覆盖 lite compactor 仍工作：同样的大旧工具结果请求改用 `deepseek-v4-flash-lite` 时返回 `x-zen-context-action=compact`，上游看到 `ZenProxy context compactor` 占位。
- panda policy-smoke 不能只看 HTTP 200；必须按服务端日志确认 flash/free 没有 `context_action=compact`，lite 的 compactor 保护仍可触发。

## 必采集字段

每条记录至少包含：

```text
run_id
request_id
timestamp
client
host
base_url_kind
model
protocol
stream
case_type
prompt_est_tokens
prompt_bytes
output_est_tokens
response_bytes
status
api_ok
status_code
error_class
retry_count
timeout_ms
protocol_first_byte_ms
first_content_ms
first_tool_call_ms
total_ms
tool_call_count
tool_success
subagent_requested
subagent_supported
subagent_observed
config_mode
redaction_ok
```

字段解释：

- `protocol_first_byte_ms`：协议首包，可以是 role delta 或 message_start。
- `first_content_ms`：第一个真实文字，不能用空 delta 冒充。
- `first_tool_call_ms`：第一个真实工具调用。
- `base_url_kind`：只能写 `panda-newapi`，如果出现 `localhost` 或 `wsl-local`，该批次无效。
- `api_ok`：客户端进程/API 层是否正常完成；如果 API 成功但语义失败，`api_ok=true`、`status=error`、`error_class` 记录语义失败原因。
- `subagent_supported`：当前 runner 是否能观测该客户端的 Task/subagent 能力；不支持观测时不计入触发率分母。
- `redaction_ok`：结果文件不含真实 key、完整请求体和完整响应体时才为 true。

## 错误分类

固定错误类：

```text
ok
auth_error
model_error
client_timeout
newapi_timeout
upstream_timeout
upstream_overloaded
stream_decode_error
empty_upstream
tool_protocol_error
tool_runtime_error
subagent_not_triggered
context_drift
safety_classification_mismatch
semantic_mismatch
rate_limited
network_error
config_error
client_exit_nonzero
not_supported
unknown_error
```

遇到 `unknown_error` 必须保留脱敏样本和归因说明，不能只统计数量。

当前 runner 规则：

- CLI 进程返回 0 但内容包含 `Request timed out`、`embedded run timeout`、`system cpu overloaded`、`Failed to parse JSON` 等，必须按真实失败分类，不允许记为 `ok`。
- API 层成功但没有按测试断言输出，必须记为 `context_drift`、`tool_runtime_error`、`subagent_not_triggered` 或 `semantic_mismatch`，不允许用 `error_class=ok` 掩盖。
- Hermes 当前无法稳定观测 Task/subagent 触发，`subagent_supported=false` 时不计入触发率分母，但报告必须列出该缺口。

## 当前 dry-run 闸口结论

正式 full run 之前必须先通过 dry run。2026-06-01 final-anchor 部署后的四客户端 dry run 仍未通过，不能直接启动 4 客户端 x 500：

| 客户端/批次 | 结果 | 关键问题 |
|-------------|------|----------|
| Windows ClaudeCode dry 50 | 50/50 API ok，43/50 semantic ok | 6 个 huge_context `context_drift`，1 个 `subagent_not_triggered`；Windows runner 使用 UNC 工作目录导致 CMD fallback 到 Windows 目录。 |
| WSL ClaudeCode dry 50 | 50/50 API ok，44/50 semantic ok | 6 个 huge_context 全部 `context_drift`；模型尝试读取 ClaudeCode transcript、git 状态或继续旧任务。 |
| WSL Hermes dry 50 | 50/50 API ok，50/50 semantic ok | 功能通过，但 P50/P90/P99 total 约 54.3s/69.5s/103.5s，性能门槛未过。 |
| WSL OpenClaw dry 50 | 50/50 API ok，49/50 semantic ok | 1 个 `deepseek-v4-flash-lite` long_context `context_drift`；subagent 5/5 observed。 |

全局结果：

```text
总轮次: 200
API OK: 200/200
semantic OK: 186/200
协议/认证/模型/502/504/300s timeout: runner summary 未观察到
panda health: 三实例健康，total=90 dispatch=90 dead=0 ratelimited=0
```

脱敏报告：`docs/reports/panda-dry-run-20260601.md`。

2026-05-31 历史 dry run：

| 客户端/批次 | 结果 | 关键问题 |
|-------------|------|----------|
| WSL ClaudeCode dry 50 | 49/50 API ok，47/50 semantic ok | `deepseek-v4-flash-lite` huge_context 出现 `context_drift`；一次 tool_calc 返回 `503 system cpu overloaded`。 |
| WSL Hermes dry 50 | 50/50 API ok，50/50 semantic ok | 总耗时偏高，P90 约 47s；subagent 触发当前 runner 不支持观测。 |
| WSL OpenClaw dry 50 | 50/50 API ok，48/50 semantic ok | subagent 在 lite 上出现一次 `client_timeout` 和一次 `subagent_not_triggered`。 |
| Windows ClaudeCode partial dry 22 | 21/22 API ok，21/22 semantic ok | huge_context 出现 310s 级客户端非零退出和一次 `context_drift`；已主动中止避免继续压生产链路。 |

OpenClaw profile 修复后的新增证据：

| 批次 | 结果 | 关键结论 |
|------|------|----------|
| WSL OpenClaw-only smoke | 5/5 API ok，5/5 semantic ok | tool 2/2，subagent 1/1；OpenClaw subagent 约 20.1s 成功。 |
| WSL ClaudeCode/Hermes/OpenClaw smoke | 15/15 API ok，15/15 semantic ok | 三客户端均 5/5；Hermes P90 仍高，不能直接进入 full run。 |

2026-06-01 ClaudeCode huge_context final-anchor source-side smoke：

| 批次 | 结果 | 关键结论 |
|------|------|----------|
| panda `/v1/messages` huge stream `deepseek-v4-flash` | 3/3 semantic ok | 约 1.0MB 请求体，ZenProxy 识别 `source_client=claude-code`，压缩后 `appended_latest_user_anchor=true`，三轮均返回 `HUGE_OK`。 |
| panda `/v1/messages` huge stream `deepseek-v4-flash-lite` | 3/3 semantic ok | 同样触发 final-anchor，三轮均返回 `HUGE_OK`；其中一轮耗时约 14.8s，仍需 dry run 统计尾延迟。 |

解释：

```text
这只能证明 NewAPI -> ZenProxy -> free-model-client source-side huge stream 修复有效；
不能替代 Windows ClaudeCode、WSL ClaudeCode、Hermes、OpenClaw 的真实客户端 dry run。
```

继续 full run 前的硬条件：

1. 最新输出限制取消必须先通过真实 panda `policy-smoke/policy-dry`：缺省 `max_tokens` 不补值、显式值原样透传、OpenAI/Anthropic 只有显式值才写上游；同时确认没有高发 413、超时、空输出或明显成本/延迟失控。
2. huge_context 不再让 ClaudeCode 进入 transcript/gist/git 状态续写；2026-06-01 source-side smoke 已通过，但真实 ClaudeCode dry run 仍未通过。
3. OpenClaw subagent 已在 dry run 5/5 observed，但 `deepseek-v4-flash-lite` long_context 仍有 1 个 `context_drift`，需要复验或隔离。
4. `deepseek-v4-flash-lite` 的长/超长上下文语义漂移有隔离策略，至少不能拖死短请求和工具请求。
5. panda NewAPI / ZenProxy 日志确认没有持续 502/524、stream JSON 截断或 client_gone 高发。
6. Hermes 慢路径需要拆分并设定保护阈值，避免长 agent 循环拖死短请求 lane。

## 通过门槛

全局门槛：

```text
总请求数 >= 2000
协议类 400 = 0
stream_decode_error = 0，或全部被明确重试/降级且不裸透
empty_upstream = 0，或全部有清晰上游空输出分类
非流式 300s 超时 = 0
上游 413 = 0，或全部被明确归类并通过 lane/case 隔离
输出限制取消后的异常成本/长尾延迟 = 0，或有明确调度降级方案
短请求 P90 <= 4s
<50k first_content P90 <= 8s
工具调用成功率 >= 95%
Task/subagent 或等价 agent 触发成功率 >= 90%
默认配置污染 = 0
密钥泄漏 = 0
```

客户端单独门槛：

```text
单客户端完成请求数 >= 500
单客户端成功率 >= 99%
单客户端 client_timeout <= 1%
单客户端 config_error = 0
```

如果某客户端能力不支持 subagent/Task，报告要单列 `not_supported`，不参与该客户端触发率计算，但全局报告必须说明缺口。

## 原始结果与报告位置

原始结果默认放临时目录：

```text
.codex_tmp/panda-pressure/YYYYMMDD-HHMMSS/
```

允许包含：

```text
raw-results.jsonl
summary.json
client-logs/
redacted-samples/
```

不允许提交：

```text
真实 API key
完整请求体
完整响应体
客户端个人配置
大日志原文
```

最终报告建议写入：

```text
docs/reports/panda-pressure-YYYYMMDD.md
```

报告只写摘要、分位、错误样本和脱敏证据。

## 报告模板

```text
# panda-only 四客户端压测报告

时间：
执行人：
链路：client -> panda NewAPI http://100.69.228.93:8081 -> configured upstream
key：sk-***
模型：

## 总览

总请求：
成功：
失败：
成功率：
stream / non-stream：
短 / 中 / 长 / 超长：

## 客户端结果

| 客户端 | 请求数 | 成功率 | P50 total | P90 total | P99 total | P90 first_content | 工具成功率 | subagent/agent 成功率 | 主要错误 |
|--------|--------|--------|-----------|-----------|-----------|-------------------|------------|-----------------------|----------|

## 错误明细

| error_class | count | client | 代表 request_id | 处理结论 |
|-------------|-------|--------|-----------------|----------|

## 慢请求 Top 20

| request_id | client | case_type | prompt_est_tokens | first_content_ms | total_ms | 结论 |
|------------|--------|-----------|-------------------|------------------|----------|------|

## 工具与 agent

工具调用总数：
工具成功率：
Task/subagent 请求数：
Task/subagent 触发成功率：
not_supported：

## 配置与安全

默认配置污染：
密钥泄漏：
未提交临时文件：

## 结论

是否通过：
阻塞项：
下一步：
```

## 当前未落地事项

1. 无密钥执行器已在 `scripts/panda_pressure_runner.py`；本轮新增 `policy-smoke/policy-dry` 直接 HTTP 策略 harness。
2. 仍需要在 panda 上按本方案重新跑 policy-smoke、policy-dry、四客户端 smoke、dry run、full run。
3. 需要把真实结果以脱敏摘要形式写入 `docs/reports/`。
4. 需要在报告后更新 `docs/02-current-state.md`、`docs/03-roadmap.md`、`docs/04-improvement-backlog.md`。
