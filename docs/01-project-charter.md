# 项目章程

## 项目定位

`free-model-client-rs` 是 Rust 实现的轻量 OpenAI/Anthropic 兼容反代适配层。它面向 ClaudeCode、Hermes、OpenClaw、CherryStudio、OpenAI SDK、Anthropic SDK 等客户端，把客户端请求规范化后转发到 OpenAI-compatible 上游。

当前公开模型只保留两项：

| 对外模型 | 上游模型 | 状态 |
| --- | --- | --- |
| `deepseek-v4-flash` | `deepseek-v4-flash-free` | 已配置 |
| `deepseek-v4-flash-lite` | `big-pickle` | 已配置 |

## 目标

1. 兼容 OpenAI `/v1/chat/completions` 和 Anthropic `/v1/messages`。
2. 修复不同客户端产生的畸形工具历史，避免 `tool_call_id`、`tool_use_id`、空 assistant content 等协议错误裸透给上游。
3. 对非流式长上下文请求做输出保护，避免长非流式请求拖到 300s 或拖死短请求。
4. 保持小内核、低依赖、可嵌入 ZenProxyRS 或单独作为反代进程部署。
5. 形成可重复的 panda NewAPI / Hermes / OpenClaw / ClaudeCode 验收链路。

## 非目标

1. 不在本仓库实现 NewAPI 管理端、计费、渠道管理或用户系统。
2. 不在请求里注入隐藏“提高智商”提示词。
3. 不从客户端侧修改 ClaudeCode、Hermes、OpenClaw 的行为来掩盖服务端问题；适配优先放在本仓库协议层。
4. 不把真实凭证、密钥、代理节点明文写入仓库文档。
5. 不把真实公网攻击、凭据窃取、绕过检测、持久化、武器化 payload 作为自动化验收内容；安全测试只做授权环境和防御/协议鲁棒性验证。

## 成功标准

代码层必须满足：

- `cargo fmt -- --check` 通过。
- `cargo clippy --all-targets -- -D warnings` 通过。
- `cargo test` 通过。
- OpenAI 和 Anthropic 的工具历史修复、空输出识别、SSE 解析、非流式 cap 都有回归测试。

运行层必须满足：

- `/health`、`/v1/models`、`/v1/chat/completions`、`/v1/messages` 可用。
- panda NewAPI 链路必须通过真实请求确认，不允许凭印象描述上游。
- Hermes/OpenClaw/ClaudeCode 的测试报告必须列出 base URL、模型、请求类型、状态码、耗时、错误分类，密钥脱敏。
