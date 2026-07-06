# Zen Free Model Suite — Cache/Quality/Latency 99+ 实施计划

更新时间：2026-07-03 15:00 CST  
状态：**代码 F0–F4 已落地；panda 运维部署已完成；TMCC 2.0 生产生效 ❌ 未证实；F5 验收 ❌ 未通过**

## 部署 vs 生效（必读，禁止混用）

| 维度 | 状态 | 证据 |
|------|------|------|
| **运维部署**（脚本/二进制/进程） | ✅ **成功** | `deploy-panda-tmcc.sh` exit 0；磁盘二进制 `572cba42…`；`@1/@2/@3` active；`:4000/health` ok |
| **生产生效**（CCP/ICP 在全流量体现） | ❌ **未证实** | 13:57 后 deepseek audit **140 行中仅 9 行（6.4%）** JSON 含 `usk` 键；**131 行仍为旧 schema**（无 `usk`/`session_pin_hit` 等字段） |
| **缓存 99+ 验收** | ❌ **未通过** | ccswitch ~**41%**；NewAPI R1 ~**54%**；ZenProxy R1 ~**56%**（窗口 13:57 CST 至今） |

**禁止**将「部署脚本跑通」表述为「TMCC 2.0 已上线」或「部署完全成功」。  
正确表述：**运维部署成功，功能生效与验收均未达标。**

### 三方缓存（2026-07-03 13:57 CST 至今，deepseek-v4-flash）

| 层级 | 样本 | 命中率 | 备注 |
|------|------|--------|------|
| ccswitch | `proxy_request_logs` | **~41%** | 用户体感；`cache_read_tokens` / `input_tokens` |
| NewAPI ch69 | 314 条（154 ok / 160 type=5） | **54.12%** | `logs.other.cache_tokens` ÷ `prompt_tokens` |
| ZenProxy audit | 140 条 | **55.78%** | 与 NewAPI 差 1.7pp，统计口径基本一致 |

### 部署后仍像旧版的原因（已核实 / 待补证）

1. **audit schema 代际**：13:57 后流量在 11:54 部署**之后**，但 93.6% 行无 CCP 字段 → 新 telemetry **未覆盖全流量**（待补证：三实例 `/proc/$pid/exe` sha256）。
2. **`affinity_hit=0%` 为指标假象**：96% 走 session pin 短路，`dispatch_sticky` 固定 `affinity_hit=false`（见 `pool/manager.rs`）；**不能**据此判断 affinity 未部署。
3. **`session_pin_hit≈96%`**：L3 pin 在工作（多用 `x-opencode-session`，非 USK）。
4. **`provider_cache_observation`**：135/140 为 `no_cache_signal` → Provider 未报 cache hit，前缀/L4 仍失效。
5. **NewAPI 50% 失败**（type=5）拉低体感。

### 待补证命令

```bash
# 三实例是否均跑 572cba42
ssh panda 'for p in 4001 4002 4004; do
  pid=$(curl -sf http://127.0.0.1:$p/health | python3 -c "import sys,json; print(json.load(sys.stdin)[\"pid\"])")
  echo port=$p pid=$pid sha=$(sha256sum /proc/$pid/exe | awk "{print \$1}")
done'

python3 ops/deploy_schema_forensics.py   # audit schema 代际
python3 ops/tri_cache_report_v2.py       # 三方 R1 对账
```


| 维度 | 门槛 |
|------|------|
| Cache（三模型，生产 claude-code 流量） | token 加权 cache_read ≥ **95%**（底线 90%），逼近官方 99.1% |
| Thinking | 会话内 **全程最高强度**，禁止生产路径 `thinking: disabled` |
| Quality | reasoning 回传 ≥99%，工具参数完整 ≥99%，三模型工具矩阵 ≥9/9 |
| Latency | 同任务 wall time ≤ **1.2×** opencode 直连 |

## 根因（panda 审计 2026-07-02 ~ 07-03）

- deepseek：`affinity_hit=false` → cache 中位 **0%**；`true` → **99.5%**
- big-pickle：affinity 命中率 **0%**
- mimo：affinity **有害**（true→18.6%，false→77.4%）
- `reasoning_content` 历史回填 → 破坏 cache 前缀；affinity_key 含漂移 `p{prefix_hash}`
- TMCC v1 部署后 R2≈41–62%、`affinity_hit≈2.6%`（审计 263 行，部署 CCP 前）

---

## Sprint A — 基础设施 + 节点粘性

### A1. nginx session 粘性
- [x] `ops/zen-balancer-sticky.conf`：`hash $http_authorization consistent`
- [x] **panda 已部署** 2026-07-03：`/etc/nginx/sites-available/zen-balancer`

### A2. Redis Session Pin（zen-proxy-rs）
- [x] `src/pool/session_pin.rs`：`zprs:pin:{upstream_model}:{session_id} → node_id`
- [x] Redis 后端 + 内存回退（`CCP_SESSION_PIN_REDIS_URL` / `GLOBAL_BUDGET_REDIS_URL`）
- [x] dispatch 顺序：session pin → in-memory affinity → shard
- [x] 模型分族：`mimo-v2.5` 仅 pin，禁用 prefix affinity

### A3. big-pickle affinity 修复
- [x] `AFFINITY_MIN_BODY_BYTES` 对 claude-code 降至 16KB
- [x] affinity_key 改 **USK 路由键**（移除 `p{prefix_hash}`）

---

## Sprint B — TMCC + Reasoning Passback

### B1. Thinking Manifest（TMCC）
- [x] `src/thinking_manifest.rs`：ClaudeCode 生产路径保持 thinking enabled
- [x] ClaudeCode 生产路径删除所有 `thinking: disabled` 注入
- [x] 探针流量（health/channel-test）隔离，不计 SLO

### B2. Reasoning Passback（TMCC 2.0）
- [x] `Message.reasoning_content` 字段
- [x] **Cache-Body 不回填**历史 reasoning（`ReasoningEnrichMode::CacheBody`）
- [x] retry 仅 `CurrentTurnOnly` enrich
- [x] Anthropic `thinking` block → `reasoning_content`

### B3. 删除有害 thinking disable 分支
- [x] ClaudeCode 流/非流 disable-thinking 重试 → enrich 重试

---

## Sprint C — ICP + CCP（F1/F2）

### C1. ICP upstream body
- [x] `src/ccp/mod.rs`：USK、ICP scope、`prompt_cache_key`
- [x] `prepare_icp_upstream_request` + `prepare_upstream_request` 统一入口
- [x] `message_to_cache_upstream_json`（无历史 reasoning）

### C2. Tools Epoch / TRF
- [x] tools epoch 冻结（`CCP_TRF_STRICT`）
- [ ] ToolSearch 不解冻到 `tools` 数组 — 待 golden 跟版

### C3. Prefix Drift
- [x] `detect_prefix_drift` + audit `prefix_drift` / `prefix_32k_hash`

### C4. Session 身份
- [x] `resolve_session_identity` → `zen_session_id` from USK
- [x] `zen_session_headers` 合并至上游（FMC proxy 路径）

---

## Sprint D — 验收 + 部署

### D1. 本地测试（2026-07-03 第二次构建）
- [x] `free-model-client-rs`: **132 passed**（kernel_golden + lib）
- [x] `zen-proxy-rs`: **199** 单元 + **44** e2e passed

### D2. Ops 脚本
- [x] `ops/cache_quality_acceptance.py` — R1/R2/R3、`--strict`
- [x] `ops/cache_join_report.py` — D1–D5 归因
- [x] `ops/post_deploy_audit_check.sh` / `post_deploy_ccp_probe.sh`
- [x] `ops/deploy-panda-tmcc.sh` — CCP env 注入 + 滚动重启

### D3. 生产验收
- [ ] `cache_quality_acceptance.py --strict` 全绿 — **待暖机**
- [ ] 用户同任务 A/B（5min vs 12min）

---

## panda 部署记录

### 第一次 TMCC（2026-07-03 09:49 CST）

| 项 | 值 |
|----|-----|
| 旧 SHA256 | `74984571…` |
| 新 SHA256 | `3835f0e4…` |
| 结果 | TMCC v1；affinity 仍≈0% |

### 第二次 CCP/TMCC 2.0（2026-07-03 11:54 CST）— 运维部署 ✅ / 生产生效 ❌

| 项 | 值 |
|----|-----|
| 脚本 | `ops/deploy-panda-tmcc.sh` |
| stamp | `20260703-115429` |
| 旧 SHA256 | `3835f0e438edaae24b5192a458187edc4f9a173218ebdf8f9f9c5214ed73b68c` |
| 新 SHA256（磁盘） | `572cba42aca1370ee63be560a2b3416391cec3033fcc07b2acba69b0b3ced4eb` |
| 备份 | `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260703-115429.pre-tmcc-3835f0e438ed` |
| nginx | `hash $http_authorization consistent` |
| 实例 | `zen-proxy-rs@1/2/3` → 4001/4002/4004 **active** |
| smoke | `4000/health` ok，version `0.2.0` |
| **运维结论** | ✅ 包替换、进程重启、健康检查通过 |
| **生效结论** | ❌ 见上文「部署 vs 生效」；**未验收通过** |

### 部署后生产取证（13:57 CST 至今，deepseek）

| 指标 | 值 |
|------|-----|
| ZenProxy audit 行数 | 140 |
| 含 `usk` 键（新 schema） | **9（6.4%）** |
| 无 `usk` 键（旧 schema） | **131（93.6%）** |
| ccswitch R1 | **~41%** |
| NewAPI R1 | **54.12%** |
| ZenProxy R1 | **55.78%** |
| `session_pin_hit` | ~96%（pin 路径正常） |
| `affinity_hit` | 0%（**pin 短路导致指标恒 false，不可信**） |
| `provider_cache_observation` | 135/140 `no_cache_signal` |
| strict 验收 | **FAIL** |

**验收命令**

```bash
# 暖机后
bash ops/post_deploy_audit_check.sh
python3 ops/cache_quality_acceptance.py /tmp/panda-audit-post.jsonl --strict
python3 ops/cache_join_report.py /tmp/panda-audit-post.jsonl
```

---

## 验收清单（99+ 满分）

### Cache
- [ ] C1 三模型 token 加权 cache ≥95%（R2，STEADY）
- [ ] C2 deepseek 50k–65k 桶 ≥97%
- [ ] C3 pin≥90% / affinity≥98%（mimo 豁免 affinity）
- [ ] C4 prefix_drift <5%（脚本 `--max-prefix-drift-pct`）
- [ ] C5 thinking_manifest 翻转/会话 = 0

### Quality
- [ ] Q1 生产路径 thinking disabled 次数 = 0
- [ ] Q2 reasoning_content 回传成功率 ≥99%
- [ ] Q3 provider_missing_reasoning_content <0.5%
- [ ] Q4 工具参数完整率 ≥99%
- [ ] Q5 三模型 ClaudeCode 工具矩阵 ≥9/9

### Latency
- [ ] L1 同任务 ≤1.2× opencode
- [ ] L2 50k+ P50 首字 ≤3.5s（cache≥95%）
- [ ] L3 冷启动 10 轮恢复 R2≥90%
- [ ] L4 disabled-thinking 重试占比 = 0%（生产）

---

## 文件变更索引

| 仓 | 主要文件 |
|----|----------|
| free-model-client-rs | `src/ccp/mod.rs`, `src/canonical/mod.rs`, `src/proxy/mod.rs`, `src/proxy/openai.rs`, `src/proxy/anthropic.rs`, `src/thinking_manifest.rs` |
| zen-proxy-rs | `src/pool/session_pin.rs`, `src/pool/manager.rs`, `src/v4/provider.rs`, `src/collector/mod.rs`, `src/main.rs` |
| ops | `deploy-panda-tmcc.sh`, `zen-balancer-sticky.conf`, `cache_quality_acceptance.py`, `cache_join_report.py`, `post_deploy_audit_check.sh`, `post_deploy_ccp_probe.sh` |
| docs | `docs/cache-95plus-architecture.md`（v2.1 实施记录） |

## 待办（P0）

1. **补证**：三实例 `/proc/$pid/exe` sha256 是否均为 `572cba42…`；若一致而 audit 仍 93% 旧 schema → 查构建物/写入路径。
2. **修复** `affinity_hit` 在 pin 命中时恒为 false 的 telemetry 误导（`pool/manager.rs`）。
3. F3.2 Anthropic BBM 接线；确认 `CCP_*` env 是否被 systemd 加载。
4. 生效后复跑 `tri_cache_report_v2.py` + `--strict` 验收。

## 待办（P1）

1. 删除/收敛 `opencode_headers.rs` 与 USK 双实现（legacy `proxy.rs`）
2. RPM governor（15 RPM 排队）
