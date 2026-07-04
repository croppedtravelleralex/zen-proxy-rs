# Cache 99+ 架构方案 — ICP × CCP × 五层协同

更新时间：2026-07-04
版本：**v2.4（生产 41afc 已验收但未达 95%；tool_result cache identity 修复已本地验证）**
状态：见下表 **「部署 vs 生效」** — 禁止将字段覆盖、运维部署成功或单侧面板数据写成缓存 95%+ 达成

## 2026-07-04 10:20 四次严格验收更新

新增现场事实：

- panda 三个 `zen-proxy-rs@1/2/3` 均运行 sha256 `41afc662f35482293a55d400d6f91a6a4cea721a86e3daedd0abca23a20eda32`，health OK。
- WSL ClaudeCode 经 cc-switch `127.0.0.1:15722` + `https://sub2api.closeapi.top` 对三模型做 provider-specific 双轮验收；Windows cc-switch 保持运行，不作为进程操作对象。
- 严格窗口 `2026-07-04 09:55:08` 后，panda audit 中三模型 `usk/prefix_32k_hash/prompt_cache_key` 均非空，但真实命中仍未达标：
  - `deepseek-v4-flash`：R1/R2 `11.40%`，`pin_hit=60%`。
  - `big-pickle`：R1/R2 `15.17%`，`pin_hit=40%`。
  - `mimo-v2.5`：R1 `87.70%`，R2 `100%` 但 `cache_miss_input_tokens` 经常缺失/为 0，不能单独认定达标；同窗仍有 `empty_output=1`。
- NewAPI `logs.other` 仍无 cache 字段，`rows_with_cache_fields=0`；NewAPI 面板不能单独作为真实 provider cache 验收依据。

本轮新增根因：

1. **缓存低已从“身份字段缺失”推进到“缓存材料前缀被动态 tool_result 污染”。** 严格窗口中 4K 左右稳定系统/工具请求可复用，但 59KB、含约 12KB `tool_result` 的中等请求出现不同 `prefix_32k_hash/USK/session_id/node`，每轮等同冷启动。
2. **`prompt_cache_key` 覆盖 100% 不等于 95% 命中。** 当 ClaudeCode 工具结果落在 32K cache identity 前缀内时，即使字段全量存在，仍会产生多个 USK 和 pinned node，DeepSeek/BigPickle 继续停在 10-20% R1。
3. **Mimo 的 R2=100% 口径不可靠。** 该族经常不回传 miss token；必须同时看 R1、prompt/read、错误率和 audit outcome。

本轮本地修复：

- `free-model-client-rs/src/protocol/translate.rs`：`request_cache_material()` 在 cache identity 中将 `role=tool` 的动态工具结果内容标准化为固定占位，不再让工具输出文本、tool id 或结果长度污染 `prefix_32k_hash`；完整 prompt hash 仍保留真实内容变化。
- 新增单测 `cache_prefix_ignores_dynamic_tool_result_payloads`：39 工具 schema + 不同 12KB tool_result 时，`prompt_hash` 必须变化，但 `prefix_4k_hash/prefix_32k_hash` 必须稳定。

本轮验证：

- `free-model-client-rs`：`cargo fmt --check`、`cargo test`（143 unit + 136 kernel golden）、`cargo clippy --all-targets -- -D warnings` 均通过。
- `zen-proxy-rs`：针对 `affinity_key_uses_stable_prefix_scope` 测试通过，确认调用侧 stable-prefix 语义未破坏。
- 尚未通过 GitHub release/download 部署该 tool_result 修复；部署后必须重新用 Windows/WSL ClaudeCode + cc-switch + NewAPI + panda audit 新窗口验收，不能沿用本轮低命中窗口。

## 2026-07-04 00:30 三次根因确认

新增生产证据：

- `2026-07-04 00:08` 后 NewAPI 全渠道没有 DeepSeek/Mimo 真实用户流量进入；截图中的 45.3% 和 21:58 Mimo 502 属于旧窗口，不能当作最新二进制验收。
- 最新 panda 三实例无 `stack overflow/core-dump`；Mimo 内部探针已返回 200，但这只证明小探针兜底，不等于真实 ClaudeCode 全路径完成。
- `2026-07-03 21:00-00:08` DeepSeek audit：125 行，成功 115 行，`usk/prefix_32k_hash/prompt_cache_key` 覆盖 `37/125`，provider R2 `62.68%`。
- `2026-07-03 22:00` 后身份覆盖已接近 100%，但仍发现同一 `prefix_32k_hash=a1a6c89803c073d6` 因 `tools_hash` 从 `e177...` 变为 `c3a...` 导致 USK 从 `usk_v1:7234...` 变为 `usk_v1:6d6f...`；23:16 同前缀请求因此 `session_pin_hit=false`、`warmup_state=cold`、`cache_miss_input_tokens=50039`。

确切新增根因：

1. **第二层缓存低不是 provider 随机失效，而是 USK 过度分裂。** 旧 `icp_scope` 把 `tools_hash/tool_choice_hash` 放入 `prompt_cache_key` 的 USK；ClaudeCode 工具 schema 轻微变化时，即使可缓存 32K 前缀相同，也会换 cache key，等同冷启动。
2. **正确边界是 prefix-scope provider key。** `tools_epoch_id` 仍可作为观测、冻结和质量信号，但不能参与 provider `prompt_cache_key` 分桶；长上下文 USK 应按 `prefix_32k_hash` 稳定。

本轮本地修复：

- `free-model-client-rs/src/ccp/mod.rs`：长上下文 `icp_scope` 改为 `icp:p32k:{prefix_32k_hash}`，不再拼入 tools/choice hash；新增单测覆盖“同 32K 前缀、工具 epoch 变化、USK 不变”。
- `zen-proxy-rs/src/v4/provider.rs`：更新 affinity 测试，确认同 32K 前缀工具变化不切 key，真实前缀变化仍切 key。

验收状态：

- 本地 `free-model-client-rs` fmt/clippy/test 通过；`zen-proxy-rs` fmt/clippy/test 通过。
- 尚未通过 GitHub release 部署到 panda；尚未用 Windows/WSL ClaudeCode + ccswitch + NewAPI 真实路径验证 95%+。

## 2026-07-03 22:40 二次根因确认

用户侧新证据：

- Claude Code 统计窗口 `2026-07-03 21:00:00 -> 当前`：92 次请求，缓存命中率约 **45.3%**。
- NewAPI channel 69 在 `2026-07-03 21:58:34-21:58:38` 连续 `mimo-v2.5` 502。

panda 生产取证：

- 当前三实例健康，部署前运行二进制 sha256 为 `8817109bcb6c428ee083477b096a907de415d037fa16c5298f01449733a3d21d`。
- `2026-07-03 21:55-22:05` journal 连续出现 `fatal runtime error: stack overflow, aborting`。
- `2026-07-03 21:40-22:00` deepseek 主流量 `session_pin_hit` 很高，但 `usk/prompt_cache_key` 覆盖很低：
  - `21:40-21:50`：74 行，72 ok，`pin=72`，但 `usk=6`。
  - `21:50-22:00`：20 行，20 ok，`pin=20`，但 `usk=0`。
- Mimo `21:50-22:00`：13 行，11 ok / 2 err，audit 中 `rate_limited=false`；与 journal stack overflow 同窗。

确切根因：

1. **DeepSeek 缓存低不是文档目标本身错，而是生产主路径没有把 CCP 身份算到 dispatch 前。** ClaudeCode 走 Anthropic `/v1/messages`，旧 `resolve_session_identity()` 只按 OpenAI `ChatRequest` 解析，导致主流量 `usk=""`、`prefix_32k_hash=""`、`prompt_cache_key=""`。结果是 L3 `session_pin_hit=true` 只能把请求粘到节点，L4 provider shard 没拿到同一个 `prompt_cache_key`，所以累计命中仍能停在 40-60%。
2. **Mimo 不是上游真限流。** 生产 audit 对应错误 `rate_limited=false`；真正触发 NewAPI 502 的是 `PoolManager::dispatch_sticky()` 在 pinned node 忙/不可用时回退到 `self.dispatch(meta)`，再次命中同一个 session pin 后递归，最终 stack overflow abort。
3. 旧 `session_pin_hit` 指标有膨胀：之前只要 `dispatch_sticky()` 返回就外层标成 true，即便实际已经 fallback 到普通 dispatch。

本轮代码修复：

- `zen-proxy-rs/src/v4/provider.rs`：在 dispatch 前用同一上游 api key bucket 计算 cache identity；`messages` 路径先转换成 OpenAI `ChatRequest`，再生成 `usk/session_id/prefix_32k_hash/prompt_cache_key`。
- `free-model-client-rs/src/ccp/mod.rs` 与 `src/proxy/mod.rs`：统一 api key cache id，避免 zen-proxy 与 kernel 计算 USK 的 key bucket 分裂。
- `zen-proxy-rs/src/pool/manager.rs`：新增 `dispatch_without_session_pin()`；sticky fallback 不再递归查同一个 pin，且 `session_pin_hit=true` 只在真正拿到 pinned node 时写入。

验收口径：

- 可以说：根因已定位并有本地测试覆盖。
- 不可说：三模型已经 95%+。必须等 GitHub 路径部署后，生产 ClaudeCode / NewAPI / ZenProxy 新窗口同时证明 `usk/prefix_32k_hash/prompt_cache_key` 全量非空、Mimo 无 stack overflow、deepseek/big-pickle 稳态 R1/R2 达标。

## 部署 vs 生效（2026-07-03）

| 维度 | 状态 | 说明 |
|------|------|------|
| 运维部署（`20260703-115429`） | ✅ | 二进制 `572cba42…` 上传；`@1/@2/@3` active；nginx 粘性；smoke ok |
| 生产生效（CCP audit 覆盖） | ❌ 未证实 | 13:57 后 deepseek **140 行 audit：仅 9 行含 `usk`（6.4%）** |
| 缓存 99+ | ❌ | ccswitch **~41%**；NewAPI **54%**；ZenProxy **56%**（同窗口） |
| F5 `--strict` | ❌ FAIL | |

**表述规范**

- ✅ 可说：「运维部署成功」「二进制已替换」「健康检查通过」
- ❌ 不可说：「部署完全成功」「TMCC 2.0 已上线」「CCP 已生效」—— **尚无全流量 audit 证据，缓存未达标**

**实施快照**

| 项 | 状态 |
|----|------|
| 代码 F0–F4 | ✅ 本地已落地 |
| 本地测试 | ✅ FMC 132 + zen-proxy 199 单元 + 44 e2e |
| panda 磁盘二进制 | `572cba42…` |
| 13:57 后 audit schema | **93.6% 旧代际（无 `usk` 键）** |
| pin_hit | ~96%（L3 正常） |
| affinity_hit（audit） | 0%（**pin 短路致指标恒 false**，见 §7.4） |
| provider cache 信号 | 135/140 `no_cache_signal` |
| 未接线 | F3.2 BBM、RPM governor |

---

## 0. 评分对照：70 分版缺什么

| 70 分版已有 | 99+ 版补齐 |
|-------------|------------|
| 三层 + ICP + USK | **五层全链路**（含 ClaudeCode 信封层、NewAPI 计费层） |
| 双轨 Cache/Quality Body | **Reasoning Sidecar** + **Breakpoint Budget Manager** |
| Redis pin + affinity 改造 | **CCP 控制平面**（状态机、漂移自愈、RPM 治理） |
| tools epoch | **TRF 工具注册表冻结** + ToolSearch 不解冻到 tools 数组 |
| 压测验收 | **分解公式 + 归因树 + 四层 join 对账**（ClaudeCode/ccswitch/NewAPI/ZenProxy） |
| 分模型策略 | **分路径 SLO**（stream/buffered/non-stream/探针隔离） |
| — | **Opencode IP 隔离物理学**（Webshare egress = cache 账户边界） |
| — | **Warm-up 三态机** + 冷启动 SLA |
| — | **Compaction Firewall**（禁止静默改写前缀） |
| — | **legacy 双实现清除**（`opencode_headers.rs` vs `zen/client.rs`） |

---

## 1. 执行摘要

### 1.1 生产故障的数学表述

用户可见缓存命中率（R2）：

```text
R2 = Σ(cache_read) / Σ(cache_read + cache_miss)
```

在 ClaudeCode 多轮工具会话中，**每一轮都是一次条件概率**：

```text
P(hit_t) = P(prefix_stable_t) × P(route_stable_t) × P(provider_shard_t) × P(breakpoint_valid_t)
```

TMCC v1 部署后 R2≈41%，是因为 **四项同时崩塌**（panda 审计 + 用户截图互证）：

| 因子 | 部署后状态 | 历史高命中条件 |
|------|------------|----------------|
| `prefix_stable` | ❌ reasoning 回填、affinity_key 含漂移 `p*` | V4.113 billing strip 后受控 **94.42%** |
| `route_stable` | ❌ `affinity_hit=0%`，pin 内存冷、键分裂 | `affinity_hit=true` → **99.5%** |
| `provider_shard` | ❌ 无 `prompt_cache_key` | OpenAI 文档：60%→**87%** |
| `breakpoint_valid` | ❌ 无 `cache_control`；tools 数组可漂移 | Anthropic/OpenCode #14743 |

**结论**：不是「没部署」，是 **只部署了 TMCC 质量轨，没部署缓存控制平面**。

### 1.2 99+ 目标（可量化）

| 维度 | 门槛 | 测量源 |
|------|------|--------|
| **R2** deepseek/big-pickle | ≥**95%**（底线 90%） | NewAPI `logs` + audit join |
| **R2** mimo | ≥**85%** | 独立矩阵，不与 deepseek 混算 |
| **L3 pin_hit** | ≥**98%** | audit `session_pin_hit` |
| **L3 affinity_hit** | ≥**95%**（mimo 豁免） | audit |
| **prefix_32k_unique** / USK | **=1**（固定会话） | `panda_pressure_runner` |
| **prefix_drift_rate** | <**0.1%** | audit |
| **thinking_disabled**（生产） | **0** | audit `thinking_policy` |
| **reasoning 回传** | ≥**99%** | ClaudeCode 工具矩阵 9/9 |
| **Wall time** | ≤**1.2×** opencode 同任务 | 用户 A/B |

---

## 2. 五层缓存物理学（全链路）

```text
┌─────────────────────────────────────────────────────────────────────────┐
│ L0  Client Envelope   ClaudeCode → ccswitch → NewAPI                  │
│     Anthropic system/tools/metadata；billing header；动态 user.system    │
├─────────────────────────────────────────────────────────────────────────┤
│ L1  ICP Body Bytes    free-model-client canonical upstream JSON         │
│     tools→system→messages 字节序；suffix-only 增长                       │
├─────────────────────────────────────────────────────────────────────────┤
│ L2  Opencode Session  x-opencode-session/project/request + USK          │
│     opencode.ai/zen 账户/会话路由                                        │
├─────────────────────────────────────────────────────────────────────────┤
│ L3  Egress IP         Webshare 出口 IP（ZenProxy node pin）             │
│     opencode free tier **IP 级**限额与 cache 分区（社区/issue 实证）      │
├─────────────────────────────────────────────────────────────────────────┤
│ L4  Provider Shard    DeepSeek 磁盘 KV / Anthropic cache_control 节点   │
│     prompt_cache_key 路由；~15 RPM/前缀溢出（OpenAI 201）               │
└─────────────────────────────────────────────────────────────────────────┘
```

**关键认知**：L3 不是「优化项」，是 opencode zen free 的 **隐式账户边界**。
panda 审计 `affinity_hit=true → 99.5%` 证明：**同一 Webshare egress 是 L4 命中的前提**。

### 2.1 L0 — ClaudeCode 信封层（本地已部分处理，未闭环）

| 动态源 | 机制 | 现状 | 99+ 处理 |
|--------|------|------|----------|
| `x-anthropic-billing-header:cch=*` | 进入 system→上游 | ✅ strip | golden 持续回归 |
| `metadata.user_id` 每轮变 | 部分不入 ChatRequest | ⚠️ 待审计 | L0 剥离清单扩展 |
| ToolSearch 解冻 tools | tools 数组突变 | ❌ 未处理 | **TRF §4.3** |
| 并行 tool call 后断点丢失 | Anthropic 4-BP 上限 | ❌ 未处理 | **BBM §4.4** |
| ClaudeCode 动态 system 段 | 与静态合并 | ⚠️ | **S1/S2 分块** |
| ccswitch 展示层 vs 上游模型 | 非 cache 问题 | 已知 | 对账时分离 |

压测铁证（`docs/06-panda-pressure-test-plan.md`）：

- 本地 `prefix_32k_unique=1` 时，远端仍可能发散 → **L0 仍有未剥离动态内容**
- V4.113 + shared workspace → **94.42%** → L0 可修好
- `--exclude-dynamic-system-prompt-sections` → **6.07%** → 禁止默认开启

### 2.2 L1 — ICP（Identical Cacheable Prefix）

**定义**：对 USK 会话，第 `t` 轮 Cache-Body 的第 `t-1` 轮前缀字节 **完全相等**。

```text
ICP(t) := serialize(cacheable_prefix_bytes[0..boundary_t])
∀ t>1: ICP(t-1) == prefix(ICP(t))
```

**禁止**：

- 历史 `reasoning_content` 写入 Cache-Body（TMCC v1 致命伤）
- `compact_*` 改写已冻结前缀（`free-model-client-rs context compactor` 标记会改 session_scope）
- zen-proxy `ZEN_COMPACTOR_MODE=enforce` 作用于 flash/free（当前已分流，需 **Compaction Firewall** 锁死）

### 2.3 L2 — Opencode Session Header

`zen/client.rs` 已实现 cache-friendly session（V4.98+）：

```433:444:repos/free-model-client-rs/src/zen/client.rs
pub fn zen_headers(api_key: &str, body: &serde_json::Value) -> Vec<(String, String)> {
    vec![
        // ...
        ("x-opencode-session", stable_session_id(api_key, body)),
    ]
}
```

**分裂 BUG**：`zen-proxy session_pin` 用 `client_id`，**不是** `stable_session_id(body)`。
**分裂 BUG**：`opencode_headers.rs` 另有一套简化 session（legacy proxy 路径）。

99+：**删除双实现**，USK 单点计算后注入 headers。

### 2.4 L3 — Webshare Egress Pin

Redis 结构：

```text
zprs:pin:{upstream_model}:{USK} → node_id     TTL 24h sliding
zprs:egress:{node_id} → observed_exit_ip      审计用
zprs:rpm:{USK}:{minute_bucket} → count        ≤12 留余量（OpenAI ~15 RPM 上限）
```

dispatch 顺序：

```text
nginx hash(auth) → Redis pin(USK) → affinity(USK) → shard → budget
```

affinity 主键 **禁止** 含 `p{prefix_hash}`（当前 `build_affinity_key` 的结构性错误）。

### 2.5 L4 — Provider Shard

| 上游族 | 机制 | 必做 |
|--------|------|------|
| DeepSeek | 磁盘 KV，`prompt_cache_hit_tokens` | `prompt_cache_key=USK` |
| Anthropic/big-pickle | tools→system→messages + BP | 显式 `cache_control` × ≤4 |
| OpenAI 兼容 | 自动 + 路由键 | `prompt_cache_key` + 可选 `prompt_cache_retention` |

全仓现状：**零处** `prompt_cache_key` / 生产 `cache_control` 注入。

---

## 3. CCP — Cache Control Plane（控制平面）

70 分版把逻辑散落在 pin/affinity/canonical；99+ 引入 **CCP** 作为横切子系统。

### 3.1 组件

```text
┌──────────────── CCP (Redis + audit) ────────────────┐
│ USK Registry      会话→icp_scope→frozen_tools_epoch │
│ Pin Table         zprs:pin:*                        │
│ Prefix Ledger     USK → (prefix_32k_hash, turn_seq) │
│ Drift Detector    比较相邻轮 hash；告警+自愈          │
│ RPM Governor      防止 L4 shard 溢出                  │
│ BP Manager        Anthropic 4 断点槽位分配            │
│ Reasoning Sidecar USK+msg_idx → reasoning text      │
└─────────────────────────────────────────────────────┘
          ▲                    │
          │            ┌───────┴────────┐
    ICP Builder        │ free-model-    │
    (kernel)           │ client-rs      │
                       └────────────────┘
```

### 3.2 USK 精确定义

```text
USK = "usk_v1:" + H16(
    zen_api_key_id,
    upstream_model,
    public_model,
    source_client_bucket,
    icp_scope
)

icp_scope =
  if estimated_tokens < 10_000: "normal"
  else: "icp:p32k:" + prefix_32k_hash_stable

prefix_32k_hash_stable =
  H32(canonical_serialize(cacheable_body)[0:32KiB])
  // tools_epoch_id 仅保留为观测/冻结信号，不进入 provider prompt_cache_key
```

**注意**：`session_scope` 含 compactor 标记时会变（`zen/client.rs:375`）→ compactor 触发必须 **fork 新 USK** 并记录，不能静默替换。

### 3.3 Drift Detector 自愈策略

| 检测 | 条件 | 动作 |
|------|------|------|
| `D1` 相邻轮 prefix_32k 变 | hash_t ≠ hash_{t-1} 且无新 user 消息 | 告警 + 阻断 deploy；查 ICP 违规 |
| `D2` pin_hit 但 R2<50% | L3✓ L1✗ | 查 Cache-Body 是否被 enrich/compaction 污染 |
| `D3` pin_miss 连续 3 次 | L3✗ | 强制 `dispatch_sticky`；检查 Redis |
| `D4` cache_creation ≈ full prompt | Anthropic tools 区突变 | 查 TRF/ToolSearch |
| `D5` 本地/远端 prefix 发散 | runner 双端 hash | 查 L0 动态信封 |

### 3.4 Warm-up 三态机（避免「部署后 5 分钟」误判）

```text
State COLD:    turn≤2 或 restart 后 10 分钟内
               允许 cache_creation 高；R2 门槛 ≥60%

State WARM:    turn 3..10
               R2 门槛 ≥85%；pin_hit≥90%

State STEADY:  turn>10 且 30min 无 restart
               R2 门槛 ≥95%；pin_hit≥98%
```

`cache_quality_acceptance.py` 必须带 `--warmup-state` 参数，禁止用 COLD 窗口宣布成功（V4.107 教训）。

---

## 4. ICP Pipeline 2.0（生产唯一入口）

### 4.1 流水线阶段

```text
Ingress ChatRequest/AnthropicRequest
  → L0 Strip (billing, dynamic system isolation)
  → Tool History Canonicalize (existing)
  → [Compaction Firewall: SKIP for flash/free/big-pickle production]
  → TRF: freeze tools registry for USK
  → S1/S2 System Split
  → Cache-Body assemble (NO historical reasoning)
  → ICP hash + USK mint
  → L4 hints: prompt_cache_key / cache_control
  → zen_headers(USK) + fetch_zen_stream
  → Quality-Body stream map (thinking → client)
  → Reasoning Sidecar write (async, never mutates Cache-Body)
```

**硬接线**：`openai.rs` / `anthropic.rs` **删除**手写 `zb` JSON，统一 `prepare_upstream_request` + ICP phases。

### 4.2 S1/S2 System Split（学 OpenCode #14743）

```text
S1 稳定（cache_control 断点 1）:
  - provider 指令模板
  - 全局 AGENTS 规则
  - 剥离 billing/cwd/时间

S2 动态（cache_control 断点 2 或 messages 尾部）:
  - cwd / 项目路径 / 环境变量
  - per-turn user.system 追加内容
```

Anthropic 顺序：**tools(BP0) → S1(BP1) → S2 → messages…**

### 4.3 TRF — Tool Registry Freeze

**问题**（Claude Code 官方 issue #53132/#63930）：ToolSearch 解冻 → `tools` 数组变化 → **整段前缀作废**。

**TRF 策略**：

1. USK 首见时，冻结 **完整 tools 数组**（`tools_epoch` + 内容 hash）
2. 后续轮次：**语义兼容**则强制使用冻结 bytes（已有 `tools_semantically_compatible`，需加强到 schema 级）
3. ToolSearch / 动态 MCP：
   - **禁止**把解冻后的 schema 写入 `tools` 数组
   - 转为 **messages 内 tool_result 虚拟块** 或 defer stub（与 OpenCode `defer_loading` 同思路）
4. 工具名 canonicalize 回 ClaudeCode 注册名（已有 `synthesis::tool`）

### 4.4 BBM — Breakpoint Budget Manager（Anthropic ≤4 BP）

| 槽位 | 锚点 | TTL |
|------|------|-----|
| BP0 | tools 末 | 1h（若支持）或 5min |
| BP1 | S1 末 | 同上 |
| BP2 | 上一轮对话末（滚动） | 5min ephemeral |
| BP3 | 预留 / 长文档 | 按需 |

并行 tool call 重轮时（#63930 Mode B）：**先推进 BP2 再追加 messages**，避免 4 槽被单轮大量 block 挤掉历史断点。

### 4.5 TMCC 2.0 — Reasoning Sidecar（非双轨糊弄）

| 存储 | 键 | 写入时机 | 读取 |
|------|-----|----------|------|
| Redis/内存 | `rsn:{USK}:{assistant_idx}` | 流式结束后 | **仅** Quality 响应路径 |
| Cache-Body | — | **永不写入** reasoning_content | — |

`enrich_messages_with_reasoning` 改为：

```text
if policy == QualityRetryCurrentTurnOnly:
    enrich only last assistant index
else:
    no-op for Cache-Body path
```

`anthropic_buffered` 路径仍含 `disabled-thinking retry`（`anthropic.rs:2303`）→ **必须**纳入 TMCC 2.0 清扫清单。

### 4.6 Compaction Firewall

| 层 | 模型 | 策略 |
|----|------|------|
| zen-proxy `v4/context.rs` | flash/free | **observe/warn only**（已分流，加 feature flag 锁） |
| free-model `compact_*` | flash/free/mimo | `model_disables_input_compaction` ✅ 保持 |
| 任何 compaction | 若触发 | **新 USK** + audit `icp_fork_reason` |

---

## 5. 四跳对账（NewAPI 40.9% 争议终结）

### 5.1 三种口径

```text
R1 = Σ(cache_read) / Σ(prompt_tokens)              // Postgres 常见 ~63%
R2 = Σ(cache_read) / Σ(cache_read + cache_miss)    // 用户 UI ~40.9%
R3 = Σ(cache_read) / Σ(total_tokens)               // 含 output，误导
```

**99+ 验收以 R2 为主**，R1/R3 仅作诊断。

### 5.2 Join 链路

```text
ClaudeCode rid
  ↔ ccswitch SQLite (可选)
  ↔ NewAPI logs (x-newapi-request-id)
  ↔ zen-proxy audit (external_request_id)
  ↔ provider_cache_observation (tracing)
```

已有工具：`zen-proxy-rs/scripts/collect_test_record.py` 的 `request_map` → 扩展为日常巡检。

### 5.3 NewAPI 层风险

- 流式 usage 帧合并（V4.107 已修 zen-proxy 侧）
- NewAPI 是否重写 `prompt_tokens` 仅含 miss 部分 → 导致 R1≠R2
- channel 69 计费名过滤（用户截图「Claude Code · 真实消耗」）

---

## 6. 分模型 × 分路径 SLO 矩阵

### 6.1 deepseek-v4-flash

| 路径 | R2 | pin_hit | 备注 |
|------|-----|---------|------|
| `/v1/messages` stream | ≥97% | ≥98% | ClaudeCode 主路径 |
| `/v1/messages` buffered | ≥90% | ≥95% | 慢路径单独观测 |
| `/v1/chat/completions` | ≥95% | ≥98% | NewAPI 直连 smoke |

### 6.2 big-pickle

| 路径 | R2 | BP 策略 |
|------|-----|---------|
| anthropic stream | ≥95% | BBM 全开 |
| anthropic non-stream | ≥93% | 探针隔离 |

### 6.3 mimo-v2.5

| 路径 | R2 | 路由 |
|------|-----|------|
| 全路径 | ≥85% | **仅** USK pin；**禁用** prefix affinity |

历史：affinity 175/183 但 token hit 9% → **证明 L3≠L1，不能只看 affinity**。

---

## 7. 可观测性 — Cache Forensics Kit

### 7.1 Audit 必填字段（缺一则 99+ 不验收）

```json
{
  "usk": "...",
  "icp_scope": "...",
  "icp_turn_seq": 12,
  "prefix_4k_hash": "...",
  "prefix_32k_hash": "...",
  "prefix_drift": false,
  "icp_violation": null,
  "session_pin_hit": true,
  "affinity_hit": true,
  "selected_node_id": "...",
  "observed_exit_ip": "...",
  "prompt_cache_key": "...",
  "thinking_policy": "claude_code_production_default_enabled",
  "cache_read_input_tokens": 0,
  "cache_miss_input_tokens": 0,
  "cache_creation_input_tokens": 0,
  "provider_cache_observation": "accepted",
  "warmup_state": "steady",
  "gateway": "newapi",
  "external_request_id": "..."
}
```

### 7.2 现有资产复用

| 资产 | 用途 |
|------|------|
| `log_provider_cache_observation` | 已有 prefix hash；接入 CCP |
| `panda_pressure_runner.py` | 本地/远端双端 prefix 对比 |
| `cache_quality_acceptance.py` | R1/R2/R3 + `--strict` + pin/drift/coverage |
| `cache_join_report.py` | audit 归因 D1–D5 + 可选 NewAPI join |
| `post_deploy_audit_check.sh` | 拉 audit + 抽样 `usk` 覆盖率 |
| `post_deploy_ccp_probe.sh` | 大 body 探测 + 最新 audit 字段检查 |
| `deploy-panda-tmcc.sh` | release 构建 + nginx 粘性 + 三实例滚动 |
| `collect_test_record.py` | NewAPI↔ZenProxy join |

### 7.3 归因决策树（on-call）

```text
R2 < 90% ?
├─ prefix_32k_unique > 1 → 修 L0/L1 ICP
├─ pin_hit < 90%         → 修 L3 Redis/nginx
├─ pin_ok ∧ R2 low       → 查 reasoning enrich / compaction
├─ cache_creation ≈ prompt → 查 TRF/tools 突变
└─ 仅 NewAPI 低 Zen 高   → 修 usage 合并 / 口径
```

### 7.4 已知 telemetry 陷阱：`affinity_hit` 在 pin 命中时恒为 false

`pool/manager.rs` 在 `session_pin` 命中时走 `dispatch_sticky` 并返回 `session_pin_hit: true`，但内层 `DispatchResult.affinity_hit` 固定为 `false`，**不会**调用 `try_acquire_affinity`。

因此 audit 中 **`affinity_hit=0%` 与 `session_pin_hit≈96%` 可同时成立**，不代表 affinity 未部署，也不代表路由失败。
**禁止**单独用 `affinity_hit` 判断部署是否生效；应看 `usk` schema 覆盖率、pid→exe sha256、R2 与 `provider_cache_observation`。

### 7.5 运维脚本

| 脚本 | 用途 |
|------|------|
| `tri_cache_report_v2.py` | NewAPI + ZenProxy + ccswitch 三方 R1 对账 |
| `deploy_schema_forensics.py` | audit schema 代际（`usk` 键有无） |
| `deploy-panda-tmcc.sh` | 运维部署（≠ 验收通过） |

---

## 8. 实施路线（6 Sprint，依赖严格）

### Sprint F0 — Forensics + 止血（1–2 天）

- [x] F0.1 audit 全字段 + `provider_cache_observation` 写入 JSONL（`RequestTelemetry` + `v4/provider.rs`）
- [x] F0.2 **禁用** Cache-Body 历史 reasoning 回填（`ReasoningEnrichMode::CacheBody`）
- [x] F0.3 `cache_quality_acceptance.py`：R1/R2/R3 + `--strict` + pin/drift/coverage
- [x] F0.4 NewAPI↔audit join 日报脚本（`ops/cache_join_report.py`）
- **门槛**：能解释用户 40.9% = R2；能定位 D1–D5 ✅（join 归因树已可用）

### Sprint F1 — ICP + TRF（4–5 天）

- [x] F1.1 统一 `prepare_upstream_request` → `prepare_icp_upstream_request`
- [x] F1.2 S1/S2 + canonical JSON（`message_to_cache_upstream_json`）
- [x] F1.3 TRF 冻结 + tools epoch（`apply_tools_epoch` / `CCP_TRF_STRICT`）
- [ ] F1.4 Compaction Firewall 锁配置 — 仅 lite 路径 enforce，flash 仍 warn
- **门槛**：受控压测 `prefix_32k_unique=1` — 待 panda 压测复验

### Sprint F2 — CCP + L3（3–4 天）

- [x] F2.1 USK 单点（`free-model-client-rs/src/ccp/mod.rs`）
- [ ] F2.2 Redis pin + RPM governor — pin 已接 Redis/内存；**RPM governor 未实现**
- [x] F2.3 affinity 改 USK；移除 `p{prefix_hash}`（`v4/provider.rs::build_affinity_key`）
- [x] F2.4 Prefix Ledger + Drift Detector（`detect_prefix_drift` + audit 字段）
- **门槛**：`pin_hit≥98%`，`affinity_hit≥95%` — **待生产暖机**

### Sprint F3 — L4 Provider（3–4 天）

- [x] F3.1 `prompt_cache_key`（`CCP_PROMPT_CACHE_KEY`，默认 on）
- [ ] F3.2 Anthropic BBM + `cache_control` — flag 存在，**未接线**
- [x] F3.3 Reasoning Sidecar + retry `CurrentTurnOnly`（TMCC 2.0 主路径）
- [ ] F3.3b buffered 路径 disable-thinking 清扫 — 待扫尾
- **门槛**：受控 shared-prefix R2≥94% — 待压测

### Sprint F4 — 生产灰度（3–5 天）

- [x] F4.1 panda 全量滚动发布（`ops/deploy-panda-tmcc.sh`，stamp `20260703-115429`）
- [x] F4.1b 部署时注入 `CCP_*` env（实例 env 文件 append）
- [ ] F4.2 STEADY 态 24h audit — **进行中，待用户流量**
- [ ] F4.3 用户同任务 A/B（5min vs 12min）
- **门槛**：STEADY R2≥95% — **未达标**

### Sprint F5 — 99+ 满分验收（2 天）

- [ ] F5.1 三模型工具矩阵 9/9
- [ ] F5.2 mimo 独立矩阵 ≥85%
- [ ] F5.3 重启冷启动 10 轮恢复 ≥90%
- [ ] F5.4 plan.md 验收清单 C1–C5/Q1–Q5/L1–L4 全勾
- [x] F5.0 本地 `cargo test` 全绿 + 验收脚本可运行

---

## 9. Feature Flags（灰度与回滚）

| Flag | 默认 | 作用 |
|------|------|------|
| `CCP_ICP_ENABLED` | **on** | ICP 总开关 |
| `CCP_PROMPT_CACHE_KEY` | **on** | L4 OpenAI `prompt_cache_key` |
| `CCP_ANTHROPIC_BP` | **on**（未接线） | BBM `cache_control` |
| `CCP_SESSION_PIN_REDIS_URL` | 空 → 回退 `GLOBAL_BUDGET_REDIS_URL` | L3 Redis pin |
| `CCP_REASONING_SIDECAR` | **on** | off = 回退历史 reasoning enrich（紧急） |
| `CCP_TRF_STRICT` | **on** | 工具冻结严格模式 |

实现：`free-model-client-rs/src/ccp/mod.rs::CcpFlags::from_env()`；zen-proxy `main.rs` 启动时 `session_pin::configure`。

回滚：`python ~/.cursor/anti-lazy/scripts/rollback.py` + 二进制备份（`deploy-panda-tmcc.sh` 写入 `/opt/zen-proxy-rs/backups/`）。

---

## 10. 风险登记

| 风险 | 概率 | 影响 | 缓解 |
|------|------|------|------|
| opencode 改 free tier cache 策略 | 中 | 高 | provider_cache_observation 持续监控 |
| ToolSearch 行为随 ClaudeCode 版本变 | 高 | 高 | TRF + golden 跟版 |
| Webshare IP 池污染/轮换 | 中 | 高 | egress 审计 + 节点健康分 |
| Redis 单点 | 低 | 高 | 主从；降级内存 pin 但告警 |
| 15 RPM 上限 | 中 | 中 | RPM governor 排队 |
| mimo 无 miss tokens | 高 | 中 | 用 estimated_total 辅助口径 |

---

## 11. 与 70 分 TMCC / ICP v1 差异

| 维度 | 70 分 | 99+ |
|------|-------|-----|
| 链路层数 | 3 | **5** |
| 控制面 | 无 | **CCP** |
| 工具策略 | tools epoch | **TRF + ToolSearch 隔离** |
| Anthropic | 提 cache_control | **BBM 四槽管理** |
| thinking | 双轨概念 | **Sidecar + buffered 清扫** |
| 验收 | 压测 | **三态机 + 四跳 join + 归因树** |
| 遗留代码 | 未提 | **删除 opencode_headers 双实现** |
| IP 物理学 | 未提 | **L3 egress = cache 边界** |

---

## 12. 参考

### 官方 / 社区

- [Anthropic Prompt Caching Cookbook](https://platform.claude.com/cookbook/misc-prompt-caching)
- [Claude Code: Prompt caching is everything](https://claude.com/blog/lessons-from-building-claude-code-prompt-caching-is-everything)
- [Claude Code prompt caching docs](https://code.claude.com/docs/en/prompt-caching)
- [OpenAI Prompt Caching 201](https://developers.openai.com/cookbook/examples/prompt_caching_201)
- [DeepSeek Context Caching](https://api-docs.deepseek.com/guides/kv_cache)
- [LiteLLM Prompt Cache Routing](https://docs.litellm.ai/docs/tutorials/claude_code_prompt_cache_routing)
- [OpenCode #14743 system/tool stability](https://github.com/anomalyco/opencode/pull/14743)
- [Claude Code #53132 ToolSearch cache invalidation](https://github.com/anthropics/claude-code/issues/53132)
- [Claude Code #63930 parallel tools cache](https://github.com/anthropics/claude-code/issues/63930)

### 本地证据

- `docs/02-current-state.md` — V4.113 **94.42%**；mimo affinity≠hit
- `docs/06-panda-pressure-test-plan.md` — 本地/远端 prefix 发散
- `plan.md` — Sprint A–D + F0–F5 实施与验收清单
- panda 2026-07-03 审计（TMCC v1 后）— R2≈41–62%、`affinity_hit≈0%`
- panda 部署 **20260703-115429** — 磁盘 `572cba42…`；**运维 ✅ / 生效 ❌**（见 plan.md「部署 vs 生效」）
- panda 2026-07-03 **13:57 后** deepseek：ccswitch ~41%、NewAPI 54%、ZenProxy 56%；audit **6.4%** 含 `usk`

---

## 附录 A — 代码差距完整清单（G1–G18）

| ID | 差距 | 严重度 | 状态（2026-07-03） |
|----|------|--------|-------------------|
| G1 | `prepare_upstream_request` 未接入 | P0 | ✅ → ICP |
| G2 | 历史 reasoning 回填 | P0 | ✅ CacheBody |
| G3 | 无 `prompt_cache_key` | P0 | ✅ |
| G4 | 无 `cache_control`/BBM | P0 | ⏳ flag only |
| G5 | session_pin 内存-only | P0 | ✅ Redis+内存 |
| G6 | pin key ≠ zen session | P0 | ✅ `zen_session_id` from USK |
| G7 | affinity 含漂移 `p*` | P0 | ✅ USK routing key |
| G8 | audit 字段缺失 | P0 | ✅ 已写字段；**生产 JSONL 待暖机出现** |
| G9 | TMCC 与 cache 未联合 | P0 | ✅ Sidecar 路径 |
| G10 | `opencode_headers` 双实现 | P1 | ⏳ legacy `proxy.rs` 仍用 |
| G11 | ToolSearch 解冻 tools | P1 | ⏳ TRF epoch，未隔离 ToolSearch |
| G12 | buffered 路径 disable-thinking 残留 | P1 | ⏳ |
| G13 | compactor 改 session_scope | P1 | ⏳ |
| G14 | 无 RPM governor | P1 | ❌ |
| G15 | 无 NewAPI join 日常化 | P1 | ✅ `cache_join_report.py` |
| G16 | acceptance 仅 R1 口径 | P1 | ✅ R1/R2/R3 |
| G17 | 无 warmup 状态机 | P2 | ✅ cold/steady |
| G18 | collector `affinity_hit` 硬编码 false 路径 | P2 | ✅ dispatch 回填 |

---

## 附录 B — 99+ 验收勾选项（摘自 plan.md 扩展）

> **运行方式**：`python3 ops/cache_quality_acceptance.py /path/to/audit.jsonl --strict`
> 部署后：`bash ops/post_deploy_audit_check.sh`（需 `rows_with_usk > 0` 后再信 strict）

### Cache
- [ ] C1 R2≥95%（STEADY，三模型分算）
- [ ] C2 deepseek 50k–65k 桶 R2≥97%
- [ ] C3 pin_hit≥98%，affinity≥95%（mimo 豁免 affinity）
- [ ] C4 prefix_drift <0.1%
- [ ] C5 thinking_manifest 翻转=0
- [ ] C6 TRF 工具突变=0（audit `tools_epoch_drift`）
- [ ] C7 本地/远端 prefix_32k_unique=1（受控会话）

### Quality
- [ ] Q1 thinking disabled=0（生产）
- [ ] Q2 reasoning 回传≥99%
- [ ] Q3 provider_missing_reasoning <0.5%
- [ ] Q4 工具参数完整≥99%
- [ ] Q5 工具矩阵 9/9

### Latency
- [ ] L1 同任务 ≤1.2× opencode
- [ ] L2 50k+ P50 首字 ≤3.5s（STEADY）
- [ ] L3 冷启动 10 轮恢复 R2≥90%
- [ ] L4 disabled-thinking 重试=0%
