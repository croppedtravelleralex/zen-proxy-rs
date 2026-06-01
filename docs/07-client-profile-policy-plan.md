# 客户端识别与策略隔离方案

更新时间：2026-05-31

## 背景

Hermes/OpenClaw 适配后，ClaudeCode 体感变差，主要风险来自共享策略误伤：

1. 有 tools 就默认注入 `thinking: disabled`，ClaudeCode 几乎每次请求都带 tools，容易导致短视、不听话、Task/subagent 触发下降。
2. 流式输出过滤 `trim().is_empty()` 片段，可能丢空格、换行、缩进，造成 ClaudeCode 输出格式怪。
3. 工具历史修复策略全局生效，Hermes/OpenClaw 的畸形历史兜底可能改变 ClaudeCode 原本严格工具链的上下文语义。

结论：下一步不继续堆全局兼容逻辑，先做客户端识别和 policy 隔离。

## 阶段目标

先做 90 分版本：

```text
明确识别客户端 -> 按客户端选择策略 -> 修复 ClaudeCode 误伤 -> 保留 Hermes/OpenClaw 兼容能力 -> 小矩阵拿数据
```

拿到数据后再做 99+：

```text
基于真实指标做动态 profile、per-client 质量/延迟/工具成功率优化、灰度和回滚。
```

当前状态：90 分代码实现已落地；OpenClaw body/profile 修复已部署到 panda，OpenClaw-only smoke 5/5、WSL ClaudeCode/Hermes/OpenClaw smoke 15/15 已通过。下一步是重新跑 dry run，再决定 99+ 动态 profile 范围。

## 90 分版本范围

### 1. 客户端识别

新增统一客户端类型：

```text
claude-code
hermes
openclaw
cherrystudio
openai-sdk
anthropic-sdk
unknown
```

识别顺序：

1. 显式请求头：`x-fmc-client`。
2. 常见头：`user-agent`、`x-client-name`、`anthropic-client`、`openai-organization` 等。
3. 请求体特征兜底：ClaudeCode 常见 `Task`、`Bash`、`Read`、`Edit`、`TodoWrite` 工具集；OpenClaw/Hermes 自身工具命名。
4. 仍无法识别则为 `unknown`。

显式头优先，兜底识别只能作为辅助，不能覆盖显式头。

### 2. 策略隔离

`claude-code` 策略：

1. 不因为 tools 默认禁用 thinking。
2. 流式文本保留空格、换行、缩进；只用是否有真实文本累计来判断空输出。
3. 工具历史只做硬协议修复：缺 `tool_call_id`、缺 tool id、orphan tool result 等会导致上游 400 的问题。
4. 不做语义性重写，不把缺失工具结果改成大段解释文本，除非是最后兜底且有日志。
5. 不自动合成 Task/Bash/Read 工具调用。

`hermes` / `openclaw` 策略：

1. 保留更强工具历史兼容修复。
2. 对缺 id、缺 `tool_call_id`、错位 tool result 做协议级修复。
3. 可补齐工具调用参数，但必须记录为 `tool_args_completed`。
4. 不全局禁用 thinking；只在明确工具 JSON 稳定性需要时启用保守模式。

`unknown` 策略：

1. 默认不禁用 thinking。
2. 只修硬协议错误。
3. 不做客户端专属优化。

### 3. 观测字段

每次请求至少记录：

```text
client_profile
client_profile_source(header/user-agent/body/unknown)
thinking_policy
whitespace_preserved
tool_history_policy
tool_history_repair_counts
tool_args_completed
stream_text_delta_count
stream_whitespace_delta_count
empty_upstream_class
```

### 4. 验收标准

90 分版本必须满足：

1. ClaudeCode 带 tools 的普通请求不再默认 `thinking: disabled`。
2. ClaudeCode 流式输出不丢纯换行、纯空格、缩进片段。
3. ClaudeCode 工具历史只做协议级修复，不做 Hermes/OpenClaw 专属重写。
4. Hermes/OpenClaw 小矩阵不回退：models、短回复、文件/终端工具、web_fetch 或等价 web 用例仍可跑。
5. `x-fmc-client` 显式头可覆盖自动识别。
6. `unknown` 不走高侵入兼容策略。
7. 单元/集成测试覆盖三类 profile：`claude-code`、`hermes/openclaw`、`unknown`。

## 90 分执行任务

T1：定义 `ClientProfile`

- 位置建议：`src/client_profile.rs` 或 `src/protocol/client_profile.rs`。
- 输出：`ClientProfile { kind, source }`。
- 状态：已完成，位于 `src/client_profile.rs`。

T2：入口识别并贯穿请求上下文

- `routes/chat.rs`
- `routes/models.rs`
- `proxy/openai.rs`
- `proxy/anthropic.rs`
- kernel 层如果需要测试，补测试入口。
- 状态：已完成 OpenAI/Anthropic chat 路径；models 路径当前只做认证和列表返回，不需要策略贯穿。

T3：拆 policy

- thinking policy：按 profile 决定是否禁用。
- stream text policy：ClaudeCode 保留空白 delta。
- tool history policy：ClaudeCode strict，Hermes/OpenClaw compat，unknown minimal。
- 状态：已完成 90 分版本。

T4：补测试

- ClaudeCode + tools：上游请求不带 `thinking: disabled`。
- Hermes/OpenClaw + tools：保留兼容修复，但不污染 ClaudeCode。
- 流式空白 delta：ClaudeCode 输出保留换行/缩进。
- `x-fmc-client` 优先级高于 UA/body 推断。
- 状态：已完成 kernel golden 回归测试。

T5：小矩阵验收

- ClaudeCode：短回复、工具调用、Task/subagent、格式化 markdown/代码块。
- Hermes：短回复、文件/终端工具、web 用例。
- OpenClaw：models、infer、agent 文件工具、web_fetch。
- 状态：panda smoke 已通过；OpenClaw subagent 已从历史 328s timeout 修复为约 20.1s 成功。Hermes 慢路径仍需在 dry run 拆分。

## 99+ 后续范围

90 分版本拿到数据后再做：

1. per-client 指标分位：first_content、total、工具成功率、Task/subagent 触发率、格式异常率。
2. per-client 动态 policy：按真实失败类型调整，而不是写死。
3. profile 配置化：环境变量或配置文件允许覆盖每类客户端策略。
4. admin/metrics 暴露：每个 profile 的请求量、错误率、修复次数、延迟。
5. A/B 对比：同 prompt 对比旧策略、新策略、不同 thinking policy。
6. 灰度和回滚：单客户端 profile 可单独关闭。

## 当前未做

1. 还没有 dry-run 级别的 profile 维度真实运行数据。
2. Windows ClaudeCode 原生执行链路尚未纳入修复后 smoke。
3. 还没有动态 profile、per-client metrics、灰度和回滚。
