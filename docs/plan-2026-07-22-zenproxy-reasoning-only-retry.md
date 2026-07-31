# Zenproxy / 渠道 69 排查与修复计划（全量待办）

- 日期：2026-07-21 ~ 2026-07-22（**本文三处结论已被 07-27 全面诊断证伪，见下方勘误**）
- 范围：CloseAPI new-api **渠道 69**（`ocrs`）→ panda Zenproxy → WebShare → OpenCode
- 文档角色：本轮排查的**全量待办台账**
- **2026-07-27 全面诊断报告**：[docs/diagnosis-2026-07-27-channel69-comprehensive.md](./diagnosis-2026-07-27-channel69-comprehensive.md)（基于 per-request audit 数据，含完整根因矩阵和实施路线）
- 相关入口：[docs/README.md](./README.md) · [ops-backlog.md](./ops-backlog.md) · 根目录 [plan.md](../plan.md)

---

## 勘误（2026-07-27 全面诊断后发现）

本文档的三条结论被新数据证伪。新报告基于 zen-proxy-rs audit 日志 7d 19,622 条 per-request 记录（62 字段 + 4 嵌套对象），非上一轮的 metrics 聚合口径：

|原结论|新数据|裁决|
|---|---|---|
|TTFB 慢在 reasoning，中间 2.4s 花在思维链|`first_content_ms − first_chunk_ms` = **322ms**|**错**|
|10 秒是重试等待，每天浪费 26.5 分钟|4,294 条 empty_output 的 `retry_count` = **100% 为 0**|**错**|
|缓存命中率低|7 天 `cache_tokens/prompt_tokens` 稳定 **77–85%**|**错**|

根因：上一轮只用了 metrics（`by_model`/`by_outcome`/`by_body_bucket` 是三个互斥计数器），无法交叉分析。本轮的审计日志才是真数据。

新文档已给出完整根因矩阵、实施路线和「明确不做」清单。本文档的发布清单（T40–T47）和验收命令仍然有效。

---

## 0. 一句话结论

本轮其实是 **两个独立问题**：

| # | 现象 | 根因 | 状态 |
| --- | --- | --- | --- |
| A | `do request failed`（约 132s） | new-api 容器经 `172.22` 访问 `172.17.0.1:4000` 被 **UFW 拦截** | **已修复**（渠道改 `http://172.19.0.1:4000`） |
| B | `reasoning_only` 500 | OpenCode 偶发只出 thinking；Zenproxy **SSE 完成后漏重试** | **代码已写**（WSL `250043d`），**panda 未发布** |

首字慢：主要是 OpenCode 上游 TTFT，不是 WebShare / Zenproxy 调度。

---

## 1. 全量待办总表

> 状态：`done` / `in_progress` / `todo` / `blocked` / `cancelled` / `deferred`

### 1.1 排查与分析 — 已完成

| ID | 事项 | 状态 | 证据/备注 |
| --- | --- | --- | --- |
| T01 | 确认渠道 69 链路：new-api → nginx `:4000` → zen-proxy-rs@1/2/3 → WebShare → `opencode.ai/zen` | done | panda 实测 |
| T02 | 区分 `do request failed` vs `reasoning_only` | done | 独立根因 |
| T03 | 定位 `do request failed` = `dial tcp 172.17.0.1:4000: connection timed out` | done | new-api docker logs |
| T04 | 确认宿主机 `:4000` 正常、容器连 `172.17` 超时、`172.19.0.1:4000` 可达 | done | curl/wget |
| T05 | 确认 UFW BLOCK：`SRC=172.22.0.4 DST=172.17.0.1 DPT=4000`（约 17:20 起） | done | dmesg / ufw.log |
| T06 | 确认 new-api 双网卡：`172.19`（compose）+ `172.22`（grok2api_default） | done | docker inspect |
| T07 | 确认 `host.docker.internal` → `172.17.0.1`，docker0 常 `NO-CARRIER/DOWN` | done | |
| T08 | TTFT 分层：WebShare/Zenproxy 快，慢在 OpenCode 生成等待 | done | 审计 timings |
| T09 | Zenproxy 三实例 health：`dead=0` `fuse=false`；解释 `dispatch`=空闲可调度节点数 | done | |
| T10 | 近一周 empty_output 约 9%–15%；今天非新异常 | done | audit JSONL |
| T11 | empty 近 3 天 **100% streaming**；non-stream empty≈0 | done | |
| T12 | 同 prompt 再试最终成功约 **93.6%**；典型 `eo→ok` | done | journal prompt_hash |
| T13 | 空输出 p50≈3.4s，90%&lt;10s，撞不上 mid-stream 10–45s 门槛 | done | |
| T14 | 源码：SSE `completed_upstream` 漏 reasoning_only 重试；non-stream/buffered 已有 | done | `anthropic.rs` |
| T15 | `should_retry_with_disabled_thinking` 曾恒 `false` | done | 方案 B 依据 |

### 1.2 运维侧即时修复 — 已完成

| ID | 事项 | 状态 | 证据/备注 |
| --- | --- | --- | --- |
| T20 | 渠道 69 `base_url` → `http://172.19.0.1:4000` | done | 用户已改；DB 已确认 |
| T21 | 渠道测试通过（19:55 左右） | done | new-api SYS testing logs |

### 1.3 方案 A/B 代码 — 本地已完成，线上未发布

| ID | 事项 | 状态 | 证据/备注 |
| --- | --- | --- | --- |
| T30 | 方案 A：`should_retry_stream_completed_reasoning_only` + SSE 完成后 enrich 重试 | done（本地） | WSL commit `250043d` |
| T31 | 方案 B：`thinking_disabled_retry_body`；stream/non-stream/buffered 末次禁用 thinking | done（本地） | 同 commit |
| T32 | 单测 `stream_guard_retries_completed_reasoning_only_output` | done（本地） | |
| T33 | 单测 `thinking_disabled_retry_body_forces_disabled_thinking` | done（本地） | |
| T34 | anthropic proxy 相关单测通过 | done（本地） | 2026-07-22 WSL |

**当前缺口**：panda `/opt/zen-free-model-suite-src` 仍在 `a5cb722`（2026-07-15 二进制），**不含** `250043d`。

### 1.4 构建与发布 — 未做（下一步主线）

| ID | 事项 | 状态 | 备注 |
| --- | --- | --- | --- |
| T40 | WSL release 构建 `zen-proxy-rs` linux amd64 | **todo** | `/home/lenovo/zen-free-model-suite` |
| T41 | 登记 panda-deploy inventory（zenproxy） | **todo** | skill 下尚无 zen 条目 |
| T42 | 产物进 Git（`deploy/panda/bin/`），**禁止 scp 正式发布** | **todo** | |
| T43 | push 分支到 GitHub（当前 detached/`codex/cache-lane-85plus` @ `250043d`） | **todo** | 确认是否已 push remote |
| T44 | panda `git pull` 对齐到含 `250043d` 的 commit | **todo** | `/opt/zen-free-model-suite-src` |
| T45 | 备份旧二进制并安装到 `/opt/zen-proxy-rs/zen-proxy-rs` | **todo** | |
| T46 | 滚动重启 `zen-proxy-rs@1/2/3` | **todo** | |
| T47 | 健康检查 `:4000/:4001/:4002/:4004` 并贴真实输出 | **todo** | |

### 1.5 验收 — 未做（发布后）

| ID | 事项 | 状态 | 通过标准 |
| --- | --- | --- | --- |
| T50 | 三实例 + nginx health | **todo** | `status=ok` `dead=0` `fuse=false` |
| T51 | 日志出现完成后 reasoning-only 重试 | **todo** | `retrying after completed reasoning-only` / enrich / final disable thinking |
| T52 | 发布后 1–2h empty_output 率对比 | **todo** | 相对发布前 ~14% 明显下降；理想 ~1%–3% |
| T53 | 渠道 69 不再大量 `reasoning_only` 500 | **todo** | new-api logs 抽样 |
| T54 | 回归：正常流式 + cache/thinking 主路径未整体关闭 | **todo** | 短问 deepseek-v4-flash |

### 1.6 文档与收尾

| ID | 事项 | 状态 | 备注 |
| --- | --- | --- | --- |
| T60 | 本全量待办写入 newapi `docs/` | **done** | 本文件（本次扩写） |
| T61 | `docs/README.md` 入口链接 | **done** | 已有 |
| T62 | 同步到 zen-free-model-suite `docs/` | **done** | 已对齐扩写版；纳入 suite git 仍待 commit |
| T63 | 更新根 `plan.md` 指向本轮待办 | **done** | 2026-07-22 |
| T64 | 写入 `ops-backlog.md` 运维待办条目 | **done** | 见该文件顶部「Zenproxy / 渠道 69」 |
| T65 | CHANGELOG / 发布说明（commit、sha256、验收输出） | **todo** | 发布后 |
| T66 | 废弃正式路径 scp deploy 说明，改为 pull-only | **todo** | |

### 1.7 相邻加固（建议另开，非本发布阻塞）

| ID | 事项 | 状态 | 备注 |
| --- | --- | --- | --- |
| T70 | 评估 new-api 是否必须挂 `grok2api_default`；能卸则卸，减少双网卡路由漂移 | **todo** | 与 T20 互补 |
| T71 | UFW：明确允许「compose 网关访问宿主机 :4000」或文档规定只用 `172.19.0.1` | **todo** | 防再改回 `172.17` |
| T72 | 其它仍写 `172.17.0.1` 的渠道/脚本巡检 | **todo** | |
| T73 | `host.docker.internal` 解析到不可达 docker0 的风险说明 | **todo** | |

### 1.8 明确不做 / 延后

| ID | 事项 | 状态 | 原因 |
| --- | --- | --- | --- |
| X01 | 全局关闭 deepseek thinking | cancelled | 伤 cache/质量，与 TMCC 冲突 |
| X02 | 把 reasoning_only 当成功返回空正文 | cancelled | 破坏 Claude Code 对话 |
| X03 | 只靠换 WebShare 当主修复 | cancelled | 非主因 |
| X04 | panda 上 `cargo build` | cancelled | 部署铁律 |
| X05 | WinSCP/scp/rsync 当正式发布 | cancelled | 部署铁律；改 Git 产物 |
| X06 | reasoning_only 触发节点短冷却 | deferred | A/B 上线后再评估 |
| X07 | 改 OpenCode 上游本身 | cancelled | 不可控；用重试消化 |

---

## 2. 问题 A 详述（已修复）：`do request failed`

### 2.1 现象

```text
do request failed: Post "http://172.17.0.1:4000/v1/messages":
dial tcp 172.17.0.1:4000: connect: connection timed out
```

`use_time` ≈ 132–136s（TCP SYN 超时）。

### 2.2 根因链

1. 渠道 base_url 曾为 `http://172.17.0.1:4000`（docker0 / `host.docker.internal`）
2. new-api 同时在 `172.19` 与 `172.22`（grok2api）
3. 约 17:20 起，访问 `172.17.0.1:4000` 从 **172.22** 出站
4. UFW 只放行 `br-2cd6`（newapi）或源 `172.19` 的 `:4000`，**不放行 172.22** → BLOCK
5. Zenproxy 本身全程正常（宿主机 curl `:4000` 200）

### 2.3 修复

- 渠道 69 → `http://172.19.0.1:4000`（已验证）
- 残留加固见 T70–T73

---

## 3. 问题 B 详述（代码已写，待发布）：`reasoning_only`

### 3.1 现象

```text
status_code=500, upstream returned no assistant content or tool call (class=reasoning_only)
```

Zenproxy：

```text
empty_output_class="reasoning_only" finish_reason=Some("stop")
reasoning_chars>0 content_chars=0
attempts_used=1 used_enrich_reasoning_retry=false
```

### 3.2 根因

- 上游：deepseek-v4-flash 偶发只 thinking
- Zenproxy：非流式 / buffered 会 enrich 重试；**SSE 主路径**仅在空闲等待超过 10–45s 才 mid-stream 重试；上游 2–4s 就 `stop` 时直接 500

### 3.3 方案（已实现于 `250043d`）

- **A**：SSE `completed_upstream` 后若 reasoning_only → enrich 重试（最多 3）
- **B**：enrich 仍失败 → 末次强制 `thinking: {type:disabled}`

### 3.4 仓库坐标

| 字段 | 值 |
| --- | --- |
| WSL monorepo | `/home/lenovo/zen-free-model-suite` |
| Remote | `https://github.com/croppedtravelleralex/zen-proxy-rs.git` |
| 分支 | `codex/cache-lane-85plus` |
| 修复 commit | `250043d` — `fix: retry reasoning-only stream output before failing` |
| panda 源码 | `/opt/zen-free-model-suite-src`（仍 `a5cb722`） |
| panda 二进制 | `/opt/zen-proxy-rs/zen-proxy-rs`（2026-07-15） |

---

## 4. 发布清单（T40–T47 执行时填）

| 字段 | 值 |
| --- | --- |
| commit | `250043d` + 后续 deploy 提交 |
| 二进制 sha256 | （构建后填） |
| 重启 | `systemctl restart zen-proxy-rs@1 zen-proxy-rs@2 zen-proxy-rs@3` |
| 健康 | `curl -sS http://127.0.0.1:400{0,1,2,4}/health` |

部署铁律：本地/WSL 构建 → Git push → panda **仅** `git pull` + 装二进制 + restart；**禁止** panda `cargo build`；**禁止** scp 正式发布。

---

## 5. 验收命令（发布后贴输出）

```bash
for p in 4000 4001 4002 4004; do echo -n ":$p "; curl -sS -m 3 http://127.0.0.1:$p/health; echo; done
systemctl is-active zen-proxy-rs@1 zen-proxy-rs@2 zen-proxy-rs@3

journalctl -u 'zen-proxy-rs@*' --since '30 min ago' --no-pager \
  | grep -E 'completed reasoning-only|reasoning-enrichment retry for reasoning-only|disabling thinking for final reasoning-only|thinking disabled as last resort' \
  | tail -40

journalctl -u 'zen-proxy-rs@*' --since '30 min ago' --no-pager \
  | grep 'empty_output_class="reasoning_only"' | tail -20
```

---

## 6. 项目内其它待办来源（已检查）

| 文档 | 与本轮关系 | 仍开放项摘要 |
| --- | --- | --- |
| [ops-backlog.md](./ops-backlog.md) | 运维总待办；本轮已追加 Zenproxy 条目 | 社群 Bot P0/P1、公告 P1、连抽/预算等（与 Zenproxy 无关） |
| [community-reward-bot-plan.md](./community-reward-bot-plan.md) | 无直接关系 | 文末 P0/P1/P2 实施顺序；多数 P0 已在 ops-backlog 标 DONE，后台统计等仍为 P1 |
| [plan-2026-07-19-closeapi-major-update.md](./plan-2026-07-19-closeapi-major-update.md) | 大更新已完成 | 无阻塞本轮 |
| 根 [plan.md](../plan.md) | 原为大更新摘要 | 应改为同时指向本文件 |
| zen-free-model-suite `docs/` | 应有同步副本 | T62 |

---

## 7. 进度日志

| 时间 | 更新 |
| --- | --- |
| 2026-07-21 | 排查 TTFT、`do request failed`、`reasoning_only`；确认 UFW + SSE 漏重试 |
| 2026-07-21 | 用户改渠道 69 → `172.19.0.1:4000`，连通性恢复 |
| 2026-07-21 22:55 | WSL 落地 A/B，`250043d`；单测通过 |
| 2026-07-22 | 初版待办文档落盘 |
| 2026-07-22 晚 | **扩写全量台账**：已做/未做/相邻加固；对齐 ops-backlog / plan.md / suite docs；确认 panda 仍未发布 `250043d`；文档待办 T60–T64 完成 |

---

## 8. 下一步（按优先级）

1. **T40–T47**：构建 → Git 产物 → panda pull → 装二进制 → 重启 → 健康检查  
2. **T50–T54**：发布后验收 empty_output 率与重试日志  
3. **T62–T66**：文档收尾与 CHANGELOG  
4. **T70–T73**：网络/UFW 加固（不阻塞发布）  
5. **X06**：上线稳定后再评估节点短冷却  
