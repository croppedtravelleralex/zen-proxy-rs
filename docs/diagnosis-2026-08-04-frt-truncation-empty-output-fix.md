# zen-proxy-rs-test（:4011）截断 / empty_output / 502 修复（2026-08-04）

> 链路：`ClaudeCode / Pi → closeTest → NewAPI ch109/189 → panda :4010 → zen-proxy-test :4011`
> **约束**：所有修复仅在 ZenProxy 侧（`zen-proxy-rs` + 内嵌 `free-model-client-rs`）；不改 NewAPI、Pi agent、Claude Code 等端侧。

## 现象（修复前）

| 现象 | 典型根因 |
|------|----------|
| 输出说到一半停 / recap 后无正文 | `provider_missing_reasoning` 风暴 + text-only 重试压扁 tool history |
| 截断旁出现 `[prior assistant tool call summarized] tool=Bash bytes=…` | FMC `flatten_tool_history_for_text_only_retry` 注入的**上游请求**摘要串，被模型复述进可见输出 |
| 502 `upstream returned no assistant content` | `max_tokens=384000` 被上游拒（上限 **131072**）→ 重试空转 → empty_output |
| 503 `proxy_pool_exhausted` | `empty_output` 误把节点 bury 进 dead，dispatch 池萎缩 |
| 首字 20–65s | 冷 prefill、大会话重试空转、FRT 计量口径 |

## 已部署版本（panda `:4011`）

| 包名 | 时间（CST） | SHA256 | 要点 |
|------|-------------|--------|------|
| `test-20260804-sanitize-v1` | ~10:47 | `c2e9b876…` | audit 清理 |
| `test-20260804-frt-v2` | ~11:16 | `8efa2cc2…` | reasoning→text 桥接、sticky 重试、大上下文少换节点、thinking 计入 FRT |
| `test-20260804-frt-v5` | **16:07** | **`ed6ba3dadb27c82b1af1bc4ca1c017401421cafbfc7fb2adf8a5ccf54834198f`** | DSML 泄漏未修 |
| `test-20260804-frt-v6` | **18:01** | **`4fc2e0aad2bd411cbbb4814ce722b2d1e453098dd7619f13f72be258404827b5`** | **当前线上**（DSML holdback + invoke XML 修复） |

`frt-v3` / `frt-v4` 未单独长期保留；`frt-v4` 含已废弃的流式 `[thinking…]` 注入，**勿再部署**。

### frt-v5 代码变更摘要

**FMC（`repos/free-model-client-rs`）**

| 区域 | 改动 |
|------|------|
| `translate.rs` | `UPSTREAM_MAX_OUTPUT_TOKENS = 131072`；`clamp_upstream_max_tokens` 写上游前压限 |
| `translate.rs` | 大会话 fold：≥200k tokens 且 ≥400 条消息才 fold；摘要用稳定 fingerprint，去掉每轮变化的 signals |
| `proxy/mod.rs` | **删除** `[prior assistant tool call summarized]` / `[prior tool result summarized]` 标记；text-only flatten 仅保留工具名或原文 preview |
| `proxy/mod.rs` | `provider_missing` 恢复 **EnrichReasoning → CompatToolUse → TextOnly**，不再首跳 TextOnly |
| `proxy/anthropic.rs` | 上游 EOF 时 partial text 补 `end_turn`；流结束 reasoning→text 桥接（沿用 frt-v2） |

**zen-proxy-rs（`repos/zen-proxy-rs`）**

| 区域 | 改动 |
|------|------|
| `pool/manager.rs` | `EmptyOutput` **不再** `dispatch.remove` + `dead.bury`；视为请求形态/上游软失败，节点留在 dispatch |
| `v4/provider.rs` | `before_dispatch` 错误写入 audit（去掉 request_id 门控）— 若 frt-v5 构建已包含 |

### 部署后健康（2026-08-04 16:09 CST）

```json
{
  "status": "ok",
  "version": "0.2.0",
  "git_hash": "4c7e213f55ce",
  "build_time": "2026-08-04 16:07:09 +0800",
  "pools": { "dispatch": 100, "dead": 0, "ratelimited": 0, "active": 1 }
}
```

ExecStart：`/opt/zen-proxy-rs/zen-proxy-rs.test-20260804-frt-v5`

## 验收与压测

### 构建 + 部署脚本

```bash
bash ops/local-dev/run_frt_v5_pipeline.sh test-20260804-frt-v5
# 日志：.local-dev/frt-v5-pipeline.log
```

### Pi 并行压测（须在 **Windows** 跑，WSL 无 `pi.cmd`）

```powershell
cd \\wsl.localhost\HermesUbuntu\home\lenovo\zen-free-model-suite
python ops/local-dev/run_piagent_parallel_rpm_test.py `
  --workers 8 --target-rpm 35 --duration-minutes 30 --shutdown-grace-s 300 `
  --run-dir .local-dev/runs/piagent-supervise-win
python ops/local-dev/analyze_pi_matrix_run.py .local-dev/runs/piagent-supervise-win
```

监督采样（可选）：`python ops/local-dev/monitor_pi_supervise.py <run_dir> --interval-s 120`

**注意**：在 WSL 用 `python3` 跑同一脚本会秒失败（1051 次 `ok=0`、elapsed≈0ms），**不是真实压测**。

### 用户侧验收

1. Claude Code / Pi 长会话：不应再出现 `prior assistant tool call summarized` 截断串。
2. journal 不应再出现 `max_tokens is too large: 384000`。
3. 截断若仍发生：抓 `request_id` + audit 窗口，区分 upstream length / empty_output / 客户端展示。

## 关联文档

- [502 empty_output 初诊（2026-08-01）](diagnosis-2026-08-01-zen-proxy-test-502-empty-output.md) — 部分结论已被本修复覆盖
- [ch109 503 池恢复（2026-08-02）](diagnosis-2026-08-02-ch109-503-ratelimited-pool-recovery.md)
- [改进待办 `04-improvement-backlog.md`](../repos/free-model-client-rs/docs/04-improvement-backlog.md)

## 待观察

- [x] 用户侧（Claude Code / Pi）：部署后多轮调用 **无截断、无空输出体感**（2026-08-04 用户确认）
- [x] frt-v6 上线（DSML holdback + invoke 修复）
- [ ] **502 限流误分类** → frt-v7：`StreamMetrics` 识别 SSE `rate_limit_error` 为 429
- [ ] frt-v5/v6 上线 **满 2h** 后 audit：502 / empty_output / 503 vs 基线
- [ ] 大会话 `cache_read` 是否因推迟 fold 更稳定
- [ ] 真实 Windows Pi 30min 压测 `truncation_like` / `semantic_ok` 率

## 部署后统计（frt-v5 → 16:49 CST，约 43 分钟，ch109 研发路径）

> 口径：zen-proxy audit `gateway_channel_id=109`；窗口起点 = 部署完成 `16:07:09`。
> 明细 JSON：`.local-dev/frt-v5-post-deploy-audit.json`、`.local-dev/frt-v5-post-deploy-stats.json`

| 指标 | p50 | p95 | p99 | 备注 |
|------|-----|-----|-----|------|
| **FRT**（audit `ttft_ms`） | 4.0s | 20.1s | 44.8s | n=2359 |
| **总延迟**（audit `latency_total_ms`） | 7.5s | 36.4s | 66.3s | n=2379 |
| **FRT**（NewAPI `other.frt`） | 3.2s | 15.7s | 42.5s | type2 n=1771 |
| **use_time**（NewAPI，秒） | 7s | 35s | 65s | type2 n=1835 |

| 质量 | 数值 |
|------|------|
| 请求数（audit） | 2379 |
| 成功率 `outcome=success` | **97.8%**（2327） |
| `empty_output` | 26（1.09%），其中 `empty_output_class=empty` → **502×20** |
| `client_gone` | 26（用户侧中断，非网关截断） |
| NewAPI `type=5` 错误 | **24** / 1859（1.29%） |
| NewAPI `stream_anomalies` | 92 |
| **缓存命中率**（audit `cache_read/(prompt+cache_read)`，成功请求 token 加权） | **43.56%** |
| 单请求 cache p50 / p95 | 45.8% / 50.0% |

**说明**：NewAPI `cache_tokens/prompt_tokens` 在本窗口会 >100%（记账口径），**不以 NewAPI 面板百分比作为 cache 验收**；以上 cache 以 audit `cache_read_input_tokens` 为准。

## 能否替换生产（:4001/4002/4004，channel 69）

| 维度 | 结论 |
|------|------|
| **截断 / 空输出体感** | ✅ frt-v5 已解决用户反馈的主痛点；可进入 **单实例 canary** |
| **错误率** | ⚠️ 43min 内仍有 24 次 type5 + 20 次 502 empty；需 **2h 窗口 type5→0 或接近 0** 再全量 |
| **缓存** | ⚠️ 全窗口 token 加权 **43.6%**，低于 fix3 基线 **71%** 与 tier **85%** 目标；短窗口混合负载（大量 &lt;50k 冷会话）不能证明达标 |
| **路径差异** | ❌ 当前仅验证 **ch109 → :4011 test**；生产为 **ch69 → 三实例**，不能直接等同 |
| **推荐节奏** | 1）继续 ch109 **2h 验收窗口**；2）**单生产实例 canary**（同 frt-v5 二进制）；3）gate 通过后再滚动其余实例 |

**结论**：**可 canary，暂不建议立即全量替换生产三实例。**

## 2026-08-04 17:28–17:30 DSML 工具串泄漏（Claude Code 截断体感）

### 现象

Claude Code 出现 2 次异常输出：正文里混入 `<噶>…<invoke name="Bash">…</invoke>` 类串后 **截断**；NewAPI 显示首字 / 总耗时 **~50–67s**（ch189 / 326k cache 大会话）。

### ZenProxy audit 对齐（panda `:4011`）

| 时间 (CST) | FRT | 总延迟 | cache_read | completion | retry | `first_tool_call_ms` | `raw_tool_format` |
|------------|-----|--------|------------|------------|-------|----------------------|-------------------|
| **17:28:33** | 35.8s | 47.4s | 326912 | **702** | 1 | **0** | `""` |
| **17:29:57** | 28.7s | 31.9s | 326912 | **242** | 0 | **0** | `""` |

- `first_tool_call_ms=0`：**未向 Claude Code 下发任何 `tool_use` 块**，工具意图以 **纯文本** 返回。
- `text_chars` 863 / 909：DSML 标记串进了可见正文。
- `completion=242` 与 NewAPI 截图 **完全一致**。

### 根因链

1. **上游 DeepSeek** 在 **text delta** 里输出 **DSML 工具格式**（`<｜DSML｜invoke name="Bash">` / `<invoke name=…>`），而非结构化 `tool_calls`。
2. **FMC stream guard** 仅在检测到 `invoke name=` 后才 ** withhold** 后续 chunk；**更早的片段已下发** → 客户端看到乱码前缀（如 `<噶>`，实为 `<｜DSML｜` 等被截断/乱码）。
3. DSML 重试走 `compat_tool_use_retry_body`（关 thinking + enrich），**每轮仍要等上游长 reasoning** → FRT 25–36s。
4. 重试耗尽后，`final_stream_error` + 非空 `text` 会走 **“partial text + length stop”** 路径 → **把 DSML 垃圾当正文收尾**，Claude Code 显示截断残句而非 Bash 工具卡。

**不是** NewAPI 30min 客户端超时；是 **DSML 泄漏 + 无 tool_use + 慢重试** 叠加。

### 本地修复（待部署 `frt-v6`）

`repos/free-model-client-rs/src/proxy/dsml_guard.rs` + `anthropic.rs`：

- 扩展 DSML 检测（`<invoke name`、`</invoke>`、`<command>` 等）。
- **流式 holdback**：在完整 marker 出现前 **不向下游 emit** 可疑前缀（含跨 chunk 的 `<invoke`）。
- DSML 失败时 **不再** 把 DSML 正文当 partial text flush 给客户端。
- 重试条件：buffer 内任意 DSML 痕迹即触发 compat 重试。

验收：同 MineCraft 大会话再跑 Bash / Agent 工具；audit 应 `first_tool_call_ms>0` 或明确 error，**不应再出现 invoke XML 正文**。

### frt-v6 修复（已部署 18:01 CST）

1. **流式 holdback**：`<invoke` / `<subagent_type>` 等跨 chunk 前缀 **不向下游 emit**。
2. **invoke XML 修复**：检测 DSML 泄漏后，尝试从 `<invoke name="Agent|Bash">` + `<command>` / `<description>` / `<prompt>` / `<subagent_type>` **合成 `tool_use`**（`complete_tool_call` 补全缺字段）。
3. DSML 失败时 **不再** flush 垃圾正文给客户端。
4. 扩展泄漏标记：`subagent_type`、`prompt` 等。

部署：`test-20260804-frt-v6`，SHA256 `4fc2e0aad2bd411cbbb4814ce722b2d1e453098dd7619f13f72be258404827b5`，`ops/local-dev/run_frt_v6_pipeline.sh`。

### frt-v6 代码变更摘要

**FMC**

| 模块 | 改动 |
|------|------|
| `proxy/dsml_guard.rs` | 扩展 DSML 标记；`take_emittable_text` 流式 holdback |
| `proxy/dsml_repair.rs` | 从泄漏的 `<invoke name="…">` XML 合成 `tool_use` |
| `proxy/anthropic.rs` | DSML 检测后优先 repair；失败则 compat 重试；禁止 flush DSML 正文 |

## 2026-08-04 18:14–19:24 502 `upstream returned no assistant content or tool call`（ch189 / claude）

> CloseAPI 通用日志：`status_code=502, upstream returned no assistant content or tool call`；首字「不适用」、耗时 3–5s。  
> 排查脚本：`ops/local-dev/audit_502_window.py`；journal：`zen-proxy-rs-test` @ 18:14 / 19:21 CST。

### 结论（非真·空输出）

| 维度 | 说明 |
|------|------|
| **真实根因** | ch189 **上游 Rate Limit（429）**；大会话 **~211k estimated tokens**，首包前被拒 |
| **为何显示 502 empty** | FMC 耗尽 3 次限流重试后下发 SSE `rate_limit_error`，但 ZenProxy `StreamMetrics` **未识别 error 事件** → `has_assistant_output=false` → 误标 **`empty_output`** → HTTP **502** + 上述文案 |
| **为何成串 502** | 同一 `prompt_hash`（同一会话）在 v4 `call_with_retry` **每 3–5s 重打**；Claude + Pi 并发加剧 ch189 压力 |
| **与 DSML 截断** | **不同问题**；DSML 为 17:28 长 FRT + invoke 正文泄漏 |

### audit 指纹（19:18–19:25 CST）

| empty_class | 条数 | 典型 status | 特征 |
|-------------|------|-------------|------|
| **`empty`** | 9 | **502** | `frt_ms=0`，`prompt=0`，`lat≈3–5s`，`retry_count=1` |
| **`reasoning_only`** | 7 | 200 | `prompt≈91k/138k`，`reasoning_chars>0`，`completion=0` |

`empty` 类 502 与 journal **限流** 时间戳对齐（如 19:21:36 / 19:21:47 / 19:21:59）。

### journal 证据（frt-v6）

```
upstream provider rate limited the request
prompt_hash_hex=2ea535cbd045d0ca | eb7d583eebc18151
estimated_total_tokens=211351 | 211719
ClaudeCode stream guard retrying after pre-output upstream rate limit
```

并行请求中 **91k/138k** 会话有时仅产出 **thinking**（`reasoning_only`、HTTP 200），因 ZenProxy **`has_assistant_output` 不计 thinking**，仍记 `outcome=empty_output`。

### 与 2026-08-01 test 502 的差异

| | 08-01 初诊 | 08-04 19:21 窗口 |
|--|------------|------------------|
| 主因 | `max_tokens=384000` 拒收、微探测空流 | **ch189 限流** + 误分类 |
| 会话规模 | 混合 | **~211k** 单会话反复重试 |
| 耗时 | 混合 | **稳定 3–5s** |

### 待修（frt-v7 候选）

1. **P0**：`StreamMetrics` 解析 SSE `type=error`（`rate_limit_error` → **429** / `upstream_429`，勿记 empty_output）
2. **P1**：同 `prompt_hash` 限流后 **冷却**，避免 v4 每 3s 重打同一 211k 会话
3. **P2**：Claude Code 将 **thinking 计入有效输出**，避免 reasoning_only 记 empty_output
4. **运维**：MineCraft 大会话降并发；Claude + Pi 同时打 330k cache 易触发 ch189 限流

### 待观察

- [ ] frt-v7：限流 → 429 分类修复后，CloseAPI **502 empty 串**应明显下降
- [ ] ch189 大会话并发压测：429 率 vs 真 empty_output 率
