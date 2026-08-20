# Anthropic 格式：DSML 截断 / 漏格式（2026-08-20）

> 状态：**已修**（源码 `repos/free-model-client-rs` + `repos/zen-proxy-rs`）。公开包 `dist/` 未同步。
> 链路：Windows Claude Code → Anthropic `/v1/messages` → ZenProxy → DeepSeek V4 Flash
> 复现会话：
> - `a9d2afc4-f98c-4593-9614-b9d3f6bf6804`（cwd `D:\SelfMadeTool\AutoRegister\grok`，DSML 漏格式）
> - `270aa06e-8a07-4d35-b3b8-3d731a8f4f56`（cwd `D:\SelfMadeTool\personal`，假 `end_turn` 无 tool）

## 已确认复现（P0）

最后一条助手消息（`2026-08-19T15:57:50Z`，CST 23:57）：

| 字段 | 值 |
|------|-----|
| model | `deepseek-v4-flash` |
| `stop_reason` | `end_turn`（客户端当成说完了） |
| `output_tokens` | 16 |
| `cache_read_input_tokens` | 226048 |
| thinking + text | `</｜DSML｜parameter>` / `</｜DSML｜invoke>` / `</｜DSML｜tool_calls>` |

同一会话另有两次 DSML 泄漏进 thinking（`14:33:50` Bash invoke XML、`15:05:57` playwright invoke XML），当时仍发出了 `tool_use`，所以体感是「偶发漏格式」而不是每次都停。

## 修复

1. thinking 与 text 同一套 DSML holdback；泄漏进 `dsml_leak_blob`，不写出网。
2. `repair_invoke_leak` 解析完整 `｜DSML｜invoke` + `parameter name=`；只有收尾标签时 `None`。
3. 修不了：Claude Code 在未出网时 retry；仍修不了则 SSE error，禁止 `end_turn` + XML 当正文。
4. OpenAI 路径对齐 holdback / repair / retry；非 DSML 的纯 reasoning 用 visible-text bridge，避免 empty_output。
5. 非流式 / buffered：`collected_visible_text` 不把 DSML reasoning 当答案；全失败的 tool 报 incomplete。
6. `stop_reason` 以已发出的 tool 为准；`content_filter` → Anthropic `refusal`。

## 同族问题

### 漏格式

| ID | 位置 | 问题 | 状态 |
|----|------|------|------|
| L1 | `anthropic.rs` thinking passthrough | DSML / invoke XML 走 `reasoning_content` 时原样泄漏 | **已修** |
| L2 | `mod.rs` `stream_reasoning_visible_text_bridge` | 正文空则把 reasoning（含 DSML）抄进可见 text | **已修**（DSML 拒绝桥接） |
| L3 | `dsml_repair.rs` | 不解析 `｜DSML｜parameter name=`；截断尾巴无法修复 | **已修** |
| L4 | `openai.rs` stream | 完全没有 DSML holdback | **已修** |
| L5 | Anthropic 非流式 | `collected_visible_text` 可把 reasoning（含 DSML）当正文 | **已修** |
| L6 | `dsml_guard.rs` 标记过宽 | `<command>` `<description>` `<prompt>` 可能误吞正常 XML/HTML | **已修**（去掉裸 HTML 标记） |
| L7 | 公开包 / dist | 无任何 DSML 防护 | **未同步 dist** |

### 截断 / 假结束

| ID | 位置 | 问题 | 状态 |
|----|------|------|------|
| T1 | `anthropic_stop_reason` | 无 `finish_reason` 默认 `end_turn`；DSML 截断看起来像说完 | **已修**（DSML 走 error，不再假结束） |
| T8 | `unfinished_intent` | 短句宣布「去检查/再等」却无 `tool_use`，被标成 `end_turn`（会话 `270aa06e`） | **已修**（hold + retry；重试用尽则 `max_tokens`，不删原文） |
| T2 | `dsml_text_holdback` | 流结束不 flush；末尾 1–16 字节若像 DSML 前缀会被丢掉 | **已修**（`flush_holdback`） |
| T3 | OpenAI stream | `reasoning_content` 只入库不转发；纯 thinking → empty_output | **已修**（非 DSML 桥到 content） |
| T4 | 未启动的不完整 tool JSON | `streamable_anthropic_tool_call` 失败则 `continue` 静默丢弃 | **已修**（全失败则 incomplete error） |
| T5 | `stop_reason` 用 `tool_calls.len()` | 收集到但未发出的 tool 仍标 `tool_use` | **已修**（看 `emitted_tool_call_indexes`） |
| T6 | 已启动的不完整 tool | 报 `incomplete tool call arguments` | 保持 |
| T7 | `content_filter` | Anthropic 映射成 `end_turn` | **已修**（`refusal`） |

### 其它（保留）

| ID | 位置 | 问题 | 状态 |
|----|------|------|------|
| O1 | 同名同参去重 | 偶发少一次工具 | 保留 |
| O2 | MarkdownFenceGuard | 边界提前闭 fence | 保留 |
| O3 | context compactor 标记 | `deepseek-v4-flash` 已 disable compaction | 保留 |
