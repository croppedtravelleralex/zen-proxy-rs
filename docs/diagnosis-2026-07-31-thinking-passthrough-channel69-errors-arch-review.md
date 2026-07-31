# 2026-07-31 渠道 69 thinking 透出方案 + 报错诊断 + 架构可优化空间

- 日期：2026-07-31
- 范围：ClaudeCode → cc-switch → NewAPI channel 69 (ocrs) → zen-proxy-rs (free_model_kernel) → free-model-client-rs kernel → opencode/zen 上游（100 Webshare 节点）
- 数据来源：
  - `free-model-client-rs/src/proxy/anthropic.rs`（handle_stream / handle_non_stream）
  - `free-model-client-rs/src/zen/client.rs`、`src/protocol/translate.rs`、`src/canonical/mod.rs`、`src/session/reasoning_store.rs`
  - `zen-proxy-rs/src/v4/provider.rs`、`src/proxy.rs`、`src/sse.rs`、`src/utils.rs`
  - NewAPI PostgreSQL `public.logs`（channel_id=69，2026-05-25 至 2026-07-31 全量 384,155 条，只读查询）
  - panda 生产：`ZEN_PROVIDER_MODE=free_model_kernel`，nginx :4000 → zen-proxy-rs@4001/4002/4004

---

## 0. 结论速览

| # | 主题 | 结论 |
|---|---|---|
| 1 | **thinking 为什么看不到** | kernel 在 `handle_stream` 把上游 `reasoning_content` 只累加（`reasoning.push_str`）**从不发射成 Anthropic `thinking` block**，设计上不透出。客户端界面永远看不到思考。已给出完整改动方案 |
| 2 | **渠道 69 报错真相** | 全表 384,155 条 type=1(成功)=0；type=5 真实上游错误 102,348 条中 75.7%（77,468）是 6/10 已自愈的 503 代理池耗尽。**当前活跃问题**：reasoning_only（7 天 2,617 条，占 51%）、上下文超限（885 条，单用户）、上游 500/No provider（~1,900 条）。type=2 的 60.4%（17 万条）为记账污染 |
| 3 | **架构可优化空间** | 15 项优化清单（O1-O15），P0 三项：流式 EO 不重试、audit 同步写盘默认关、部署链路无版本溯源 |

> 报告 1 与报告 2 指向同一根因：reasoning_only 报错正是 kernel 吞掉思考后的直接后果。加 thinking 透出既能解决"看不到思考"，也可能降低 reasoning_only 报错率。

---

## 1. thinking 透出实现原理与改动方案

### 1.1 现状：thinking 为何看不到（证据链）

完整链路：

```text
ClaudeCode(EFFORT=max, thinking enabled)
→ ccswitch 127.0.0.1:15721
→ newapi channel 69 (ocrs, pass_through_body_enabled, thinking_to_content:true)
→ panda nginx :4000
→ zen-proxy-rs (ZEN_PROVIDER_MODE=free_model_kernel)
→ free-model-client-rs kernel (删 thinking 的地方)
→ opencode/zen 上游 (走 Webshare 反代)
```

各环节 thinking 处理：

| 环节 | 处理 | 依据 |
|---|---|---|
| new-api 渠道 69 | `thinking_to_content: true`，但流式 Anthropic 响应里根本没有 thinking 块可转，此开关在该链路形同虚设 | 渠道 setting JSON |
| zen-proxy v4（生产模式） | `free_model_kernel`，请求交给 kernel，流式 `Body::from_stream` 直通不改写 | `zen-proxy-rs/src/v4/provider.rs:328`、`:3031` |
| zen-proxy 旧路径（Legacy） | `sse.rs:88` / `utils.rs:80` 会把 `reasoning_content` 压成 `content` | **未部署**，仅 Legacy 模式 |
| **free-model-client-rs kernel（实际剥离点）** | 上游 `reasoning_content` 只 `reasoning.push_str()` 累加，从不 yield thinking 块 | `free-model-client-rs/src/proxy/anthropic.rs:2098-2102` |

### 1.2 kernel 对 reasoning 的三个消费方向（均不透出）

在 `handle_stream` 里，上游推理内容唯一的 downstream 出口：

1. **判空/重试分类**：`reasoning_only` / `reasoning_only_length`（决定 reasoning-enrich 重试或 thinking-disable 兜底，`should_retry_stream_completed_reasoning_only` at anthropic.rs:2275）
2. **工具调用推理回放**：存 Redis `reasoning_store`，下轮把工具调用对应思考回填进消息历史（`canonical::record_tool_call_reasoning` at anthropic.rs:2430）
3. **纯 reasoning 的 fallback 文本**（仅非流式边界场景，`handle_non_stream` 554-561 行）

真实响应的 SSE 只发四类块：`message_start` / `text`（`content_block_delta`） / `tool_use` / `message_delta`，**从不发 `content_block_start {type:"thinking"}`**。

### 1.3 改动点清单

**核心：handle_stream（`free-model-client-rs/src/proxy/anthropic.rs:1674`）内，在 content 块发射逻辑旁插入 thinking 块发射。** 分 5 处：

| # | 位置 | 改动 |
|---|---|---|
| A | 首个 reasoning delta 到达处（~2090 前） | 在 `if !message_started` 检查后，增加 `if !thinking_block_open { yield content_block_start {type:"thinking"} }`；再 `yield content_block_delta {type:"thinking_delta", thinking: reasoning_content}` |
| B | reasoning delta 累加处 `reasoning.push_str` 旁 | 把当前 delta 的 reasoning_content 也 emit 出去（与 A 合并） |
| C | 流收尾（~2330-2370，text_block_stop 附近） | 若 `thinking_block_open` 且文本/tool 正常结束，补发 `content_block_stop`；**仅在最终输出（text 或 tool）时发**，避免纯 reasoning_only 也被当成成功 |
| D | 重试/清空处 `reasoning.clear()` | 清空前若 `thinking_block_open`，先发 `content_block_stop`（客户端需要配对的 block 边界） |
| E | 非流式 `handle_non_stream`（~422-800） | 在 `response_text_for_profile` 组装响应时，若 `collected.reasoning` 非空，把 response content 改为 `[{"type":"thinking"...},{"type":"text"...}]` 数组形式 |

### 1.4 thinking block 发射伪代码（流式）

```rust
// 在 content delta 处理块之前，处理 reasoning delta
if let Some(reasoning_content) = delta.reasoning_content {
    if first_reasoning_ms == 0 { first_reasoning_ms = ...; }
    reasoning.push_str(&reasoning_content);
    // ===== 新增：thinking 透出 =====
    if !reasoning_content.trim().is_empty() {
        if !message_started {
            yield message_start(usage{input_tokens: initial_input_tokens});
            message_started = true;
        }
        if !thinking_block_open {
            thinking_block_index = pending_block_index();  // 见 1.5
            yield content_block_start(index: thinking_block_index,
                content_block: {"type":"thinking","thinking":""});
            thinking_block_open = true;
        }
        yield content_block_delta(index: thinking_block_index,
            delta: {"type":"thinking_delta","thinking": reasoning_content});
        emitted_downstream_event = true;
    }
}
```

### 1.5 index 冲突处理（关键设计）

现状 `text_block_index = emitted_tool_call_blocks`，tool 块用 `emitted_tool_call_blocks` 递增。Anthropic 协议要求 content_block 的 index 从 0 连续递增、按发射顺序。**最稳妥做法**：引入统一下游块计数器 `downstream_block_seq`，thinking/text/tool 三种块都从它取 index，替代现在的 `emitted_tool_call_blocks` 逻辑：

```rust
let mut downstream_block_seq = 0u64;
// thinking 块
yield content_block_start(index: downstream_block_seq, ...); downstream_block_seq += 1;
// text 块（现 text_block_index 改为取 seq）
text_block_index = downstream_block_seq; yield content_block_start(...); downstream_block_seq += 1;
// tool 块同理
```

这样 thinking 永远在 text/tool 之前（Anthropic 规范 thinking 必须前置），index 连续无冲突。改动集中在 handle_stream 内部，不影响现有 tool 去重逻辑（`emitted_tool_call_signatures` 等）。

### 1.6 风险与影响面

| 风险 | 等级 | 说明 |
|---|---|---|
| `reasoning_only` 重试分类破坏 | **高** | 若 thinking 被透出，客户端会认为有输出，`should_retry_stream_completed_reasoning_only`（2275）在 `reasoning` 非空且 text/tool 空时触发重试。透出后此判断逻辑会误判。**必须保留内部 reasoning 与"可转发输出"分离**，即透出不影响重试判定 |
| reasoning 回放冲突 | 中 | `record_tool_call_reasoning`（2430 附近）把 reasoning 存 Redis 供下轮回填，透出后该机制仍可并行保留（不冲突），但下一轮请求会把 thinking 作为 assistant history 发上游，需确认上游接受 |
| newapi/客户端兼容 | 中 | newapi 的 `thinking_to_content` 开关此时反而有用——它会把 thinking 块转成 content 文本给不支持 thinking 的客户端；支持 thinking 的客户端（ClaudeCode）直接显示。**这正是我们要的效果** |
| 纯 thinking 响应 | 低 | 上游只吐思考不吐正文时，透出后客户端会看到思考但无正文，仍应触发 reasoning-only 重试（见风险 1） |
| buffered 路径 | 中 | `handle_buffered_claude_code_huge_stream` 走 `CLAUDE_CODE_BUFFERED_STREAM_MAX_OUTPUT_TOKENS=2048` 缓冲，reasoning 在缓冲里，透出需同步改造该路径 |
| 测试 | 低 | 现有 tests 断言 SSE 输出不含 thinking（如 `patches_reasoning_to_content` 是旧路径），新增 thinking 块不会破坏现有断言，但需加新测试 |

### 1.7 分步实施建议

1. **阶段一（验证）**：在 `handle_stream` 加 thinking 透出 + 统一 block index，单测覆盖「有 thinking+text」「有 thinking+tool」「纯 thinking」三场景，确认重试分类不受影响。
2. **阶段二（非流式）**：`handle_non_stream` 响应改为 `[{type:thinking},{type:text}]` 数组。
3. **阶段三（部署）**：按部署铁律走 `git commit → push → GHCR → panda pull`，先单实例 canary，再全量。
4. **阶段四（回测）**：确认 newapi `thinking_to_content` 对该链路行为——建议开 `pass_through_body_enabled` + `thinking_to_content:true`，让不支持 thinking 的客户端拿到 content 文本，支持的直接看思考。

---

## 2. 渠道 69 报错统计与诊断

### 2.1 报错总览

渠道 69 全表 **384,155 条，type=1(成功)=0 条**（留存期无任何成功日志）。错误全部分布在两个类型：

| type | 全量 | 近 30 天 | 近 7 天 | 近 24h | 内容归属 |
|---|---|---|---|---|---|
| 2（请求/系统错误） | 281,899 | 60,939 | 37,231 | 15,287 | 含 17 万条"上游实际成功但记账为错误"（见 2.6） |
| 5（上游渠道错误） | 102,348 | 9,915 | 5,148 | 768 | 携带真实上游报错文本，是诊断主战场 |

### 2.2 按模型分布（全量错误）

| model | type=2 | type=5 | 合计 | 近 7 天活跃度 |
|---|---|---|---|---|
| deepseek-v4-flash | 265,868 | 101,248 | **367,116（95.6%）** | 高（24h 内持续） |
| mimo-v2.5 | 10,020 | 661 | 10,681 | 中（7/31 仍有） |
| big-pickle | 2,706 | 291 | 2,997 | 中 |
| deepseek-v4-flash-lite | 2,561 | 37 | 2,598 | 已停用（6/28 后 0） |
| hy3 | 695 | 107 | 802 | 低（7/28 后几乎无） |

### 2.3 type=5 按根因聚合（全量 102,348 条）

| 根因类别 | 条数 | 占比(type5) | 近 7 天 | 趋势 |
|---|---|---|---|---|
| 503 代理池资源耗尽（proxy lane exhausted） | 77,468 | 75.7% | **6** | **历史风暴，已自愈** |
| └ no proxy resources available | (71,585) | | 0 | 集中爆发于 6/10（71,049 条） |
| └ zenproxy lane is saturated | (4,923) | | ~6 | 6 月为主 |
| 500 通用上游错误（do request failed / bad 500 / Internal server error / upstream error 500 / timeout / connection） | 13,088 | 12.8% | ~1,600 | **活跃中** |
| 上游协议/转换层错误（400 invalid_request / missing field / tool_choice / reasoning_content） | 2,069 | 2.0% | ~1,200 | 活跃中 |
| reasoning_only（上游只吐思考无正文） | 5,712 | 5.6% | **2,617** | **当前第一大 live 错误，7 天内 51%** |
| 上下文超限（context length / context_length_exceeded） | 1,163 | 1.1% | **885** | 活跃，单一用户贡献 |
| 401 No provider available / ModelError | 1,114 | 1.1% | 257 | 活跃 |
| empty_output / 空响应 | 485 | 0.5% | 46 | 低 |
| 其他（quota 40 / rate_limit 27 / model_not_found 1） | 68 | 0.1% | 3 | 低 |

### 2.4 时间趋势

- **7 天逐日 type=5**：7/24=1,330 → 7/25=396 → 7/26=562 → 7/27=493 → 7/28=613 → 7/29=791 → 7/30=1,197（7/30 晚 18:00 后从个位数跃升至数十/小时）→ 7/31 持续每时 50-75 条。
- **近 24h 逐时 type=5**：峰值 7/30 11:00 的 151 条/时，7/31 06:00 达 75 条/时。
- **type=2 持续刷屏**：近 48h 每时 400-1,200 条，7/30 18:00 峰值 1,226 条/时。

### 2.5 每类错误根因分析（原始样本打码 + 归因）

#### 2.5.1 reasoning_only（当前第一大问题，7 天 2,617 条）

```text
status_code=500, upstream returned no assistant content or tool call (class=reasoning_only)
```

new-api 容器日志同步印证：

```text
[ERR] ... | relay error: upstream returned no assistant content or tool call (class=reasoning_only)
```

**归因**：上游（zen-proxy-rs 背后的 DeepSeek/Console 模型）在长上下文思考模式下只输出了 `reasoning_content`，没有返回正文字段。7 天样本几乎全部来自 `deepseek-v4-flash`（2,627/2,637），且触发这批请求的 prompt 极大——容器日志样本显示单请求 prompt_tokens 达 **120,073 / 188,971**，缓存 token 18 万，明显是超长文档/批量任务。另有 16 条 `reasoning_only_length`（思考超长被截断）佐证。这不是客户端问题，是**上游在超长上下文下思考未收尾** 或 **kernel 对 reasoning 流的提取不透出**。

#### 2.5.2 上下文超限（7 天 885 条，单一用户 100% 贡献）

```text
status_code=500, upstream provider error (status=400, code=context_length_exceeded,
detail=Error from provider (Console): Request exceeds the context window of the model)

status_code=500, upstream provider error (status=400, code=invalid_request_error,
detail=Error from provider (DeepSeek): This model's maximum context length is 1048576 tokens.
However, you requested 2649326 tokens (2617326 in the messages, 32000 in ...)
```

DeepSeek 侧请求量级分布（打码，仅 token 数）：1,050,336 / 1,059,738 / 1,061,111 / 1,061,150 / 1,268,401 / 1,460,452 / 1,510,211 / 1,560,763 / 1,648,837 / 1,700,238……**全部超 104.8 万上限 1.0~1.6 倍**。

**归因**：两类的 `username` 均为同一用户（记为 *u1*），该用户在跑超长上下文批处理（单请求 100 万~170 万 tokens），每次都被上游以 400 拒回，形成大量错误。属**用户行为/无截断保护**问题，而非代理故障。

#### 2.5.3 502/503 代理池资源耗尽（历史风暴，已自愈）

```text
status_code=503, no proxy resources available        # 71,585 条
status_code=503, zenproxy lane is saturated          # 4,923 条
status_code=502, bad response status code 502        # 680 条
status_code=502, stream truncated before DONE or finish_reason   # 89 条
```

**归因**：全量 7.7 万条 **71,049 条（99.3%）集中在 2026-06-10 一天**，另有 6/6（259）、6/11（56）、6/13（122）、6/15（63）零星，7/5（31）、7/6（5）尾部，近 7 天为 0。这是**历史上某次 zen-proxy-rs 代理 lane 打满（或上游账号池耗尽）的集中性故障**，后续已恢复。此问题当前不构成威胁，但它是渠道 69 "全量错误率" 的最大历史贡献者（占全表 18.6%）。

#### 2.5.4 上游通用 500 / 连接 / 超时（活跃）

```text
status_code=500, upstream error: do request failed                     # 9,802 条（历史）/ 当前活跃
status_code=500, bad response status code 500                          # 2,008 条
status_code=500, upstream provider error (status=500, code=error, detail=Internal server error)   # 393 条，7天372
status_code=500, upstream error 500                                    # 222 条，7天222
status_code=500, upstream initial stream fetch timeout after 30s       # 108 条，7天59
status_code=500, upstream connection error: ... (https://***.ai/***/*** ... caused by: tunnel error)  # 7天73条
```

**归因**：zen-proxy-rs 上游（真实 DeepSeek/Console 端点）本身不稳定——部分请求上游直接 500/连接失败/30s 流超时。这部分是**上游服务波动**，在长上下文高负载时段（7/30 11:00，7/31 06:00）放大。

#### 2.5.5 No provider available（7 天 257 条）

```text
status_code=401, upstream provider error (status=401, code=ModelError, detail=No provider available)
status_code=500, upstream provider error (status=401, code=ModelError, detail=No provider available)
```

**归因**：zen-proxy-rs 内部路由时"没有可用的上游 provider"。这是 **zen-proxy 侧账号/供应商池在高峰时段无可用资源**的直证，与 2.5.3 的 503 同源但为不同错误码路径（401/500 由 upstream 返回，503 由 zenproxy 自身返回）。

#### 2.5.6 协议转换层错误（约 900 条/7 天，低但持续）

```text
status_code=400, invalid Anthropic messages request: missing field `messages`        # 458 条
status_code=400, invalid Anthropic messages request: missing field `input_schema`    # 112 条
status_code=400, upstream provider rejected thinking with forced tool choice
  (code=provider_thinking_tool_choice_unsupported)                                   # 134 条
status_code=500, upstream provider rejected transformed tool-history request
  (code=provider_missing_reasoning_content)                                          # 145 条
```

**归因**：new-api 与 zen-proxy-rs 之间的 Anthropic→OpenAI→上游协议转换存在边界 case 不兼容（空 body 请求缺 `messages`/`input_schema`、思考模式与强制 tool_choice 冲突、转换后缺 reasoning_content）。属**两侧版本兼容性缺口**。

### 2.6 type=2 的真相：六成是"记账污染"，并非真实失败

type=2 全量 281,899 条细分：

- **170,367 条（60.4%）** `other` 字段含 `status:ok`——new-api 容器日志中同批请求显示 `stream_status: {status: ok}`、`frt>0`、上游已正常返回，但最终被写入 **type=2 错误日志**。典型样本 `other` 节选：`... "stream_status":{"end_reason":"eof","status":"ok"} ...`；夹杂少量 `... {"end_error":"context canceled","end_reason":"client_gone","status":"error"}`（2,551 条为真错误，即客户端中途断开/上下文取消）。
- **806 条** `content='模型测试'`，为 new-api 后台模型测试探测（`use_channel:null`、`use_time=1ms`、`frt=-1000`，未真正路由上游）。
- 其余 ~10.8 万条其他无 stream_status，多为 `frt=-1000` 的本地失败（未发上游）。

**结论**：渠道 69 的真实上游失败应看 type=5（约 10.2 万），而 type=2 里约 17 万条实为 **new-api 记账/分类缺陷**，把成功或客户端断开误记为错误。这导致"渠道 69 全量报错 38 万"的观感被显著夸大；真实持续故障面是 type=5 的 5,149 条/7 天。

### 2.7 可落地解决建议（按优先级）

| 优先级 | 动作 |
|---|---|
| **P0-1** | reasoning_only 风暴：上游 reasoning-only 时做**一次自动重试**或"强制非思考模式重发"的补偿逻辑，不直接透传 500；对超长上下文字段做 **max_tokens/上下文窗口上限校验**，在 new-api 侧前置拦截 |
| **P0-2** | 上下文超限：渠道/模型配请求 token 上限（如 900k），超限直接 400，不打到上游；核对 u1 用户（7 天 type=2 87% 来源之一）的批处理行为，加 truncate/分段 |
| P1-1 | No provider/上游 500：排查 zen-proxy provider 池健康度、上游账号限额、连接与超时配置；对 `do request failed` 加 retry/backoff |
| P1-2 | 修 type=2 记账污染（17 万条假错误）：new-api 核对「成功响应但 type=2」的记录逻辑，修正 `context canceled / client_gone` 归类 |
| P2-1 | 协议转换兼容：对齐 newapi/zen-proxy 版本，修三类边界 case（空 body、thinking+forced tool_choice、tool-history 重建） |
| P2-2 | 复盘 6/10 代理池耗尽：确认 6/10 lane 配置/上游账号是否打满，保留 lane 扩容与限流预案 |
| P2-3 | `模型测试`（806 条）每发必失败（use_channel:null），建议在 new-api 后台重新选择渠道测试，避免面板误报渠道不可用 |

---

## 3. 架构与可优化空间梳理

### 3.1 整体架构概览

#### 3.1.1 链路拓扑

```text
ClaudeCode/客户端
  → https://sub2api.closeapi.top → NewAPI channel 69
  → nginx :4000 → zen-proxy-rs@1/@2/@4 (:4001/4002/4004)
  → free-model-client-rs kernel (协议翻译/thinking/缓存键)
  → opencode.ai/zen 上游 (经 100 Webshare 代理节点)
```

#### 3.1.2 zen-proxy-rs 模块划分（控制面 + 数据面）

| 模块 | 文件 | 行数 | 职责 |
|---|---|---|---|
| V4 主入口 | src/v4/provider.rs | 4,824 | 全链路编排：guard → context → 身份/缓存取证 → dispatch → kernel 调用 → 重试 → audit |
| 配置 | src/config.rs | 1,614 | 99 个配置字段（env 加载） |
| 管理 API | src/admin/service.rs | 1,612 | 池状态/审计查询/health/metrics |
| 动态模型发现 | src/v4/model_discovery.rs | 1,596 | 上游模型自动发现与公开/隐藏路由 |
| 节点池 | src/pool/dispatch.rs | 1,466 | score 调度/AIMD 并发/预算/affinity |
| 模型探针 | src/v4/model_probe.rs | 1,364 | 新模型 min-canary 探针矩阵 |
| 协议守卫 | src/v4/protocol_guard.rs | 998 | 工具历史完整性修复（Pre/Post compact 两段） |
| 池管理 | src/pool/manager.rs | 846 | dispatch/活性/ratelimited/dead 四池联动、EO 摘除 |
| 上下文治理 | src/v4/context.rs | 1,232 | 请求体压缩/artifact cache |
| 审计 | src/collector/* | 2,350 | per-request 遥测 62+4 字段、WAL、聚合器 |

#### 3.1.3 free-model-client-rs 模块划分（内核）

| 模块 | 文件 | 行数 | 职责 |
|---|---|---|---|
| Anthropic 代理 | src/proxy/anthropic.rs | 3,909 | ClaudeCode 特化：stream guard、reasoning-only 重试、工具完整性 |
| 协议翻译 | src/protocol/translate.rs | 2,191 | Anthropic↔OpenAI 双向、tool id 稳定化、cache material |
| 代理通用 | src/proxy/mod.rs | 1,672 | 会话/推理 scope、profile 分发 |
| Zen 上游客户端 | src/zen/client.rs | 1,243 | SSE 流收集、usage 透传 |
| 缓存键 | src/ccp/mod.rs | 318 | USK/icp_scope/affinity key 计算 |
| 客户端画像 | src/client_profile.rs | 615 | ClaudeCode/Hermes/OpenClaw 差异化策略 |

### 3.2 已记录问题清单（引用 docs，并核对代码现状）

#### 3.2.1 docs 文档层已确认问题

来源 `docs/diagnosis-2026-07-27-channel69-comprehensive.md` 根因矩阵（P0–P3）与 `docs/PROJECT_HANDOFF.md` 六次排障记录：

| 文档问题 | 级别 | 代码现状核对 |
|---|---|---|
| 部署链路断裂：`version: "0.2.0"` 无 commit，Cargo.toml 无 vergen | P0 | 属实，Cargo.toml 无 git hash 嵌入；git HEAD 与 panda 二进制脱节 |
| EO（empty_output）不重试不换节点，7d 4,294 条 `retry_count=0` | P0 | 流式路径仍属实（provider.rs:1662） |
| 坏节点无熔断，`pool_transitions=0`，最差节点 EO 率 59.7% | P0 | 代码已补 EmptyOutput 摘除（manager.rs:263），但 panda 生产二进制是否含此修复无法核实 |
| 10k–50k 桶缓存异常（50.06% 零命中，比 1k–10k 桶还差） | P1 | 未归因，无对应代码 |
| `ccp_raw_prefix_match_32k=0%`（19,623/19,623 False） | P1 | 代码层面已是必然恒 false（两种不同哈希算法不可比） |
| affinity 12.1% vs session_pin 68.1% | P2 | 属实，affinity 是进程内 `RwLock<HashMap>`（dispatch.rs:589） |
| 节点负载 6.5 倍倾斜（头号 1,360 vs 均值 209） | P2 | 采样算法 `try_sampled_acquire` 每请求只采 8 个，随机命中头部概率高 |
| tiny 桶双重恶化（EO 32.1% + 命中 22.5%，贡献 40% EO） | P3 | 未归因 |
| 缓存命中率 77–85% 稳定，USK 1:1 稳定，prefix_drift 0.23% | 已排除 | DefaultHasher 稳定性风险仍存 |

#### 3.2.2 docs 记录、代码已实现的修复（验证通过）

| 文档修复 | 代码位置 |
|---|---|
| dispatch 前计算 USK（messages 路径先转 ChatRequest） | provider.rs:946 `cache_identity_chat_request` |
| sticky fallback 不再递归命中同 pin | manager.rs:178 `dispatch_sticky` fallback 走 `dispatch_without_session_pin` |
| `session_pin_hit` 只在真实 pin 命中时置 true | manager.rs:203 |
| empty_output 清 session pin | provider.rs:1670（非流式）/2993（流式） |
| tool_result/tool_call_id 稳定化 | translate.rs:65 / :214 |
| 长上下文 `icp:p32k:{prefix_32k_hash}` | ccp/mod.rs:130 `icp_scope_for_request` |
| 流式 reasoning-only enrich 重试 | anthropic.rs:1197 + 2216（方案 A/B 已落地） |
| 空输出节点摘除 + probe 恢复 | manager.rs:263–309 |

### 3.3 可优化空间清单

| # | 优化项 | 现状问题 | 建议 | 工作量 | 优先级 |
|---|---|---|---|---|---|
| O1 | **流式 EO 不重试不换节点** | 流式响应直接返回不检测输出，`metered_stream_response` 只标 `outcome=empty_output` 上报，**不触发 call_with_retry 的换节点循环**。非流式路径才有重试。诊断 4,294 条 EO 100% 零重试即此路径 | 在 `metered_stream_response` 判定 empty_output 后，让调用方换节点重试（上限 2 次，复用 `v4_retry_budget_ms` 总预算），参考 FMC 层 anthropic.rs:2216 的 reasoning-only 重试 | M | **P0** |
| O2 | **audit 字段缺 `empty_output_class`** | 诊断 §11 缺此字段，无法区分 reasoning_only vs 其他 EO；audit schema（collector/mod.rs）无该字段 | 在 empty_output 分支（provider.rs:2993）记录 reasoning 是否存在 | S | P0 |
| O3 | **部署链路无版本溯源** | Cargo.toml 无 vergen，`/health` 只报 `version: "0.2.0"`（admin/service.rs:1562），无法知道线上二进制对应哪个 commit | 加 vergen 嵌入 git hash+构建时间 | S | P0 |
| O4 | **audit 同步写盘默认开启** | `V43_ASYNC_COLLECTOR_ENABLED` 默认 **false**（config.rs:688），`DefaultCollector::record_request` → `audit.append` 每行 `writeln + flush`（audit.rs:71-81），每请求同步磁盘 flush | 默认开 async collector（mpsc 8192 已有，async_collector.rs:33） | S | **P0**（性能） |
| O5 | **`ccp_raw_prefix_match_32k` 恒 false 是字段设计错误** | `provider.rs:840` 比较 `ccp_prefix_32k_hash`（fnv1a64,hex）vs `raw_body_prefix_32k_hash`（sha256,hex16），**两种不同哈希算法不可比**，恒 false 且被写成"缓存上限被锁死"的 P1 | 改为同算法前缀比较，或删除该字段改为 `raw_body_bytes 是否 ≤32k` 的布尔 | S | P1 |
| O6 | **smart_backoff 未被 v4 使用** | `utils.rs:148` 有完整退避（429 0.5s 起+抖动/5xx 指数/8s 封顶），但**只被旧路径 proxy.rs:600/642 用**；v4 主路径用 `provider.rs:2073` 线性 `100ms*(attempt+1)` 无抖动、不读 Retry-After | v4 退避换 smart_backoff，429 尊重上游 Retry-After；重试总预算已有（provider.rs:2100） | S | P1 |
| O7 | **affinity 不跨实例** | `dispatch.rs:589` `RwLock<HashMap>`，12.1% vs Redis pin 68.1%；进程内表重启即丢 | affinity 迁移到 Redis（与 session_pin 统一） | M | P1 |
| O8 | **负载采样偏斜（6.5 倍）** | `try_sampled_acquire`（dispatch.rs:695）每请求随机采 8 个取最高分，头部节点被反复命中；`acquire_from_shard`（dispatch.rs:806）用 score 加权轮盘，热门节点概率更高 | 采样后对候选做并发/频率归一化，或限制单节点权重上限 | M | P1 |
| O9 | **10k–50k 桶缓存异常未归因** | 诊断 §4.5：此桶 50.06% 零命中，比 1k–10k 桶（36.91%）还差，打破单调。`icp_scope`（ccp/mod.rs:130）在 ≥10k 时按 `prefix_32k_hash` 分桶，该桶恰是分桶切换的临界区 | 取真实请求对比 10k 上下 cache material 组成（tool_result 占比/角色序列），确认是否 icp 分桶临界导致 | M | P1 |
| O10 | **`DefaultHasher` 跨进程不稳定** | `ccp/mod.rs:151`、`proxy/mod.rs:189` 用 `DefaultHasher::new()`（SipHash 随机 seed），**三实例间/重启后 USK 全变**。当前 1:1 稳定是单实例观测 | 换 `short_hash16` 用固定 seed（fnv/sha 截断），三实例 USK 对齐才能跨实例 cache 复用 | S | P2 |
| O11 | **同步 Redis 阻塞 async runtime** | `session_pin.rs` 的 `redis_lookup/record/clear` 用 `client.get_connection()`（同步阻塞）；`global_budget.rs:371` 用 `std::thread::sleep`；`reasoning_store.rs`（FMC）同样同步 GET/SETEX。高峰期 Redis 延迟会卡住 async worker | 换 `redis::aio`/`deadpool-redis`，或同步调用放 `spawn_blocking` | M | P2 |
| O12 | **session pin TTL 固定 86,400s 且无容量上限** | `session_pin.rs:23` `PIN_TTL_SECS=86_400` 硬编码；内存 fallback `HashMap` 无淘汰，Redis key 不设上限 | TTL 降为可配置（如 6h），内存表加容量上限/LRU | S | P2 |
| O13 | **配置项 99 个，魔法值偏多** | config.rs 99 字段；硬编码：budget 上限 `30_000`（provider.rs:2113）、延迟归一化 `5000.0`（dispatch.rs:261）、大流阈值 `128*1024/512*1024`（dispatch.rs:221-222）、dead probe 60–120min、stream send timeout、`100ms` 退避 | 分组配置（Dispatch/Retry/Collector），魔法值提为常量或 env；不改行为，只提可观测性 | S | P2 |
| O14 | **redundant 遥测字段** | RequestTelemetry 63 字段 + CacheForensics 34 字段；`node_url` 与 `selected_node_url_redacted` 重复；`ccp_*` 前缀哈希 4 档 ×2 算法 = 16 个 hash 字段，诊断只用到 32k | 按实际消费字段裁剪，或标注 deprecated 不序列化 | S | P3 |
| O15 | **模型发现/探针体积过大** | model_discovery.rs 1,596 + model_probe.rs 1,364，合计近 3k 行 | 拆 body 构造器与分类器为独立模块 | M | P3 |

### 3.4 架构风险与短板

#### 3.4.1 高风险

| 风险 | 位置 | 说明 |
|---|---|---|
| **流式空输出 20.9% 直接失败** | provider.rs:2992 | 流式 EO 只上报不重试，`retry_count=0` 属实；FMC kernel 层的 reasoning-only 重试（anthropic.rs:2216）与 zen-proxy 层 EO 换节点是两层机制 |
| **生产二进制与源码脱节** | Cargo.toml 0.2.0 | panda 二进制与 git HEAD 脱节；任何代码层"已修复"结论都可能在 panda 上不成立 |
| **audit 每行同步 flush 写盘** | config.rs:688 默认 false / audit.rs:71 | 高 RPM 下磁盘 IO 成为请求路径瓶颈；AsyncCollector 已有实现但默认关 |
| **同步 Redis 阻塞 async worker** | session_pin.rs / global_budget.rs:371 | 阻塞式 `get_connection()` 在 async 上下文，Redis 抖动直接拉高 `dispatch_wait_ms` |

#### 3.4.2 中风险

| 风险 | 位置 | 说明 |
|---|---|---|
| **单体文件过大** | provider.rs 4,824 / anthropic.rs 3,909 / translate.rs 2,191 | 超过 800 行红线数倍，编译慢、审查难 |
| **DefaultHasher 随机 seed** | ccp/mod.rs:151 | 三实例 USK 不同，跨实例 cache shard 分叉；单实例 1:1 稳定掩盖了该问题 |
| **EO 摘除的抖动** | manager.rs:263 | 单次 EO 即把节点移出 dispatch + 全量 probe（间隔 60–120min），若 EO 是瞬时上游问题节点会被过度惩罚 |
| **重试预算用尽 4.4 分钟** | provider.rs:2100 | 12 条 p99 264s，总预算已存在（30s 封顶 provider.rs:2113）但流式路径不经过该预算 |
| **tiny 桶双重恶化** | 诊断 §3.4 | 40% EO 来自 tiny 桶，同时命中率 22.5%；无归因 |

#### 3.4.3 低风险 / 已缓解

| 项 | 位置 | 说明 |
|---|---|---|
| **日志/URL 脱敏** | ledger.rs:56-79 `sanitize_request_telemetry` | client_id hash、node_url redact、failure_message sanitize，隐私处理到位 |
| **无 prompt 原文入审计** | collector/mod.rs | 63 字段只含 hash/计数/长度，不含正文内容，无内容泄漏面 |
| **密钥管理** | config.rs `upstream_api_key` | env 加载，未硬编码；admin api key 同 env |
| **测试覆盖** | 278+44 / 314 | 覆盖 dispatch 调度、pin 清理、协议翻译、退避等关键路径；无 TODO/FIXME 残留 |
| **三实例一致性** | 诊断 §8 | EO 极差 1.1pp，非实例级问题 |

### 3.5 建议实施顺序

1. **阶段 0（阻塞）**：O3 版本溯源 + 走通 GitHub Actions→GHCR→panda 链路（诊断 P0，不做完所有修复都无法上线）
2. **阶段 1（收益最大）**：O1 流式 EO 换节点重试 + O2 empty_output_class + O4 异步 audit
3. **阶段 2（缓存专项）**：O5 ccp_raw_prefix_match 字段修复 + O9 10k–50k 桶归因 + O10 固定 seed hash
4. **阶段 3（路由/稳定性）**：O7 affinity 接 Redis + O8 负载均衡 + O11 异步 Redis

---

## 4. 三份报告之间的关联

任务 1（thinking 透出）与任务 2（渠道 69 报错）指向**同一根因**：

> 渠道 69 最大的活跃错误 `reasoning_only`（7 天 2,617 条），正是 kernel 把上游思考吞掉不透出的直接后果——上游只吐推理时，客户端看到的是"无正文"，kernel 判定 `reasoning_only` 后重试失败就报 500。

**给 handle_stream 加 thinking 透出，既能解决"看不到思考"，也可能顺带降低 reasoning_only 报错率**（因为透出后客户端有感知输出，不会触发 reasoning-only 重试风暴）。

任务 3（架构梳理）提供了独立于前两者的底层改进空间，其中 O1（流式 EO 换节点重试）与渠道 69 的 empty_output/上游波动问题直接相关。

---

## 附：数据与证据保留

- 所有 NewAPI 数据库查询为**只读**（SELECT only），未执行任何 UPDATE/DELETE。
- 报错样本中 URL 已由 new-api 打码（`https://***.ai/***`），未发现明文 key/token。
- 涉及的用户名/token 名已打码（*u1* / *u2*）。
- 生产环境：未修改任何配置、未重启任何服务。

---

## 5. deepseek-v4-flash 延迟与缓存诊断（渠道 69，近 7 天）

- 追加日期：2026-07-31
- 数据源：NewAPI PostgreSQL （channel_id=69, model_name=deepseek-v4-flash, 近 7 天, 只读）

### 5.1 首字延迟（TTFB / frt，仅流式有物理意义）

| 分位 | frt（ms） |
|---|---|
| p50 | **5,027** |
| p95 | **17,345** |
| p99 | **39,578** |
| avg | 7,033 |
| 样本 | 19,580 |

### 5.2 全程耗时（use_time）

| 模式 | n | p50 | p95 | p99 | avg |
|---|---|---|---|---|---|
| 全部 | 36,042 | **7s** | **22s** | **53s** | 9.0s |
| 流式 | 23,930 | 7s | 27s | 61s | 9.4s |
| 非流式 | 12,153 | 6s | 10s | 12s | 6.3s |

### 5.3 缓存命中率（cache_tokens ÷ prompt_tokens）

**总体 82.0%**（n=31,438）

| 维度 | 分桶 | 命中率 |
|---|---|---|
| 模式 | 流式 | **82.4%** |
| 模式 | **非流式** | **31.9%** |
| 体量 | huge>400k | 83.4% |
| 体量 | large 200-400k | 84.0% |
| 体量 | mid 100-200k | 82.2% |
| 体量 | small 10-100k | 69.4% |
| 体量 | **tiny<10k** | **41.7%** |

按天趋势：7/24 81.3% → 7/27 77.7% → 7/29 **86.1%** → 7/30 80.4% → 7/31 **75.3%**（近两天下滑）。

### 5.4 缓存率低 —— 三个真凶

**① 非流式请求几乎不缓存（31.9%）**
- 非流式 11,777 条里 **5,982 条（51%）cache_tokens=0**，另一半命中 48%。
- 非流式请求都是极小请求（avg prompt 3.4k, avg comp 494），来自大量不同 token_name（11/defualt/BOT/hermes/ds…），每种 token 请求形态不同、量小，形不成稳定缓存前缀。
- 结论：非流式是测试/一次性调用，缓存价值低，属"上游缓存对短请求不友好 + 调用方复用率低"双重因素。

**② tiny 桶（41.7%）被同样的原因命中**
- 流式 tiny 只有 135 条（命中 48.6%），但非流式 tiny 占绝大多数，拖低整体。
- 与诊断 §4.5「10k–50k 桶缓存异常」呼应：10k 以下反而成重灾区，证明上游对短请求 prefix cache 命中率天然低。

**③ 7/31 命中率下滑到 75.3%**
- 7/30 请求量暴增到 9,455（平时 3-4k），avg prompt 从 200k 骤降到 86k——大量新请求形态进入，冷缓存稀释整体命中率。

### 5.5 首字长 —— 与缓存命中基本无关，是上游 prefill 耗时

| 体量桶 | TTFB p50 | 命中率 |
|---|---|---|
| huge>400k | **8,413ms** | 84.8% |
| large 200-400k | 5,579ms | 84.1% |
| mid 100-200k | 4,291ms | 82.6% |
| small 10-100k | 3,844ms | 65.0% |
| tiny<10k | 2,809ms | 48.6% |

- **cached vs no_cache 的 TTFB 几乎一样**（p50 4,954 vs 5,365）——缓存命中不加速首字。
- TTFB 随请求体**单调上升**——上游对 15 万 token 平均体量的 prefill 本身就是瓶颈（平均 prompt 153k）。
- p95 达 17-25s，尾部来自上游节点波动 + 换节点重试。

### 5.6 耗时久 —— 大请求体量主导

- 流式 p95 27s / p99 61s：大上下文（>200k）首字就 5-8s，再加生成时间。
- 非流式反而快（p50 6s / p99 12s）：因为全是短问答，上限低。

### 5.7 结论

> **缓存率低 = 非流式小请求（测试性、不复用）拖垮；首字长 = 上游对超长上下文的 prefill 耗时，与缓存命中无关；耗时久 = 大请求体量 + 上游 p95 尾部波动。**

### 5.8 可落地动作

| 优先级 | 动作 | 预期 |
|---|---|---|
| P1 | 梳理非流式 token（/// 等）的真实用途，若是测试可停 | 命中率整体 +0-3% |
| P2 | 大请求 prefill 无法从代理侧缓解，但换节点重试（架构 O1）可压 p95 尾部 | 首字 p95 降 |
| P2 | 7/30-31 新请求形态（cold cache）趋势持续观察，是否新业务上线 | 命中率稳定在 80%+ |
