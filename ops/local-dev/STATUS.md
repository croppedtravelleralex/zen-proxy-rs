# 本地验收状态同步（2026-07-06）

> 本文档记录「本地 ZenProxy 矩阵验收」的已实现、真实结果和当前结论。所有百分比以本地 audit/provider R2 为准；CCS UI 与 NewAPI 面板只能作为对账，不作为单独达标证据。

## 一、目标与约束

目标链路：

```text
ClaudeCode → cc-switch :15721 → local-zen-* provider → 本地 zen-proxy :14000 → Webshare → opencode
```

验收门槛：

| 模型 | 目标 |
|---|---:|
| deepseek-v4-flash | 90%+，冲刺 95% |
| mimo-v2.5 | 85%+ |
| big-pickle | 85%+ 或专项策略 |

硬约束：

- 不在 panda 编译。
- panda 部署只能走 GitHub release，不用 scp 更新二进制。
- 本地矩阵不改 ClaudeCode / OpenCode / cc-switch 代码，只做 ZenProxy/free-model-client 侧适配。
- CCS 可安全重启，但必须恢复健康；不能停掉 Windows 侧 CCS。

## 二、已实现

### 2.1 本地工具链

| 文件 | 作用 |
|---|---|
| `01_preflight.sh` | 拉取 panda `nodes-prod.json`、测 Webshare→opencode、检查 release 构建 |
| `run-local-zenproxy.sh` | 启动本地 `127.0.0.1:14000` |
| `start-local-zenproxy-from-token.sh` | 用临时 token 文件重启本地 ZenProxy，避免把 token 写进仓库 |
| `03_cache_roundtrip.sh` | Anthropic `/v1/messages` 两轮 cache 探针 |
| `run_ccswitch_project_matrix.py` | 真实 Windows ClaudeCode→cc-switch→本地 ZenProxy 四项目矩阵 |
| `run_opencode_native_matrix.py` | opencode 原生四项目对照矩阵 |
| `cache_gate.sh` | 从 `.local-dev/audit/` 跑 R2 gate |

数据目录（已加入 `.gitignore`）：`.local-dev/`、`ops/local-dev/local.env`、`__pycache__/`。

### 2.2 代码修改

| 文件 | 已做 |
|---|---|
| `repos/free-model-client-rs/src/proxy/mod.rs` | 放宽 ClaudeCode+tools 的 provider invalid retry 判定：可见 tool history 或风险 tool schema 也进入 reasoning enrich fallback |
| `repos/zen-proxy-rs/src/v4/provider.rs` | Mimo `/v1/messages` 大输入稳定性策略：10k+ empty output retry 上限压到 2；10k+/50k+ retry budget 上限分别压到 30s/20s |
| `repos/free-model-client-rs/scripts/panda_pressure_runner.py` | Windows ClaudeCode 不再传空 `--setting-sources` / `--settings`，避免本机 smoke 假失败 |
| `ops/local-dev/run-local-zenproxy.sh` | 允许外部 `PROXY_API_KEY` 覆盖 `local.env`，用于和 CCS managed token 对齐 |

### 2.3 本地验证

已通过：

```text
python3 -m py_compile ops/local-dev/run_ccswitch_project_matrix.py
python3 -m py_compile ops/local-dev/run_opencode_native_matrix.py
python3 -m py_compile repos/free-model-client-rs/scripts/panda_pressure_runner.py
free-model-client-rs: cargo test provider_invalid_
zen-proxy-rs: cargo test mimo_
zen-proxy-rs: cargo build --release
```

## 三、真实 ClaudeCode→CCS→本地 ZenProxy 四项目矩阵

run dir：

```text
.local-dev/runs/ccswitch-project-matrix-20260706-095440
```

12/12 case 已执行。`deepseek-v4-flash × personal-cleanup` wrapper timeout，但 stdout 中已有 `is_error=false` 的 result。

| model | case | exit | elapsed_s | CCS rows | audit rows | provider audit R2 | p50 TTFT | p90 TTFT |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| deepseek-v4-flash | tide | 0 | 635.9 | 18 | 18 | 69.49% | 6276 | 17021 |
| deepseek-v4-flash | mirofish | 0 | 188.2 | 8 | 8 | 12.84% | 8433 | 11261 |
| deepseek-v4-flash | personal-cleanup | timeout | 1800.1 | 4 | 4 | 52.33% | 16274 | 18659 |
| deepseek-v4-flash | outlook-register | 0 | 126.3 | 11 | 11 | 58.88% | 6054 | 9030 |
| mimo-v2.5 | tide | 0 | 276.4 | 56 | 58 | 84.66% | 5416 | 7958 |
| mimo-v2.5 | mirofish | 0 | 135.3 | 8 | 8 | 73.87% | 7122 | 7452 |
| mimo-v2.5 | personal-cleanup | 0 | 278.2 | 4 | 4 | 73.79% | 4647 | 7937 |
| mimo-v2.5 | outlook-register | 0 | 258.5 | 27 | 27 | 76.51% | 6002 | 10067 |
| big-pickle | tide | 0 | 640.7 | 51 | 52 | 67.95% | 5391 | 10711 |
| big-pickle | mirofish | 0 | 119.9 | 8 | 8 | 2.05% | 9226 | 13145 |
| big-pickle | personal-cleanup | 0 | 346.9 | 6 | 6 | 55.35% | 7673 | 13256 |
| big-pickle | outlook-register | 0 | 402.6 | 19 | 19 | 76.10% | 10698 | 13583 |

聚合结果：

| model | cases | timeouts | elapsed_s | CCS read/input | CCS read/(input+read+creation) | provider audit R2 |
|---|---:|---:|---:|---:|---:|---:|
| deepseek-v4-flash | 4 | 1 | 2750.5 | 73.53% | 42.37% | 50.89% |
| mimo-v2.5 | 4 | 0 | 948.4 | 123.47% | 55.25% | 79.88% |
| big-pickle | 4 | 0 | 1510.0 | 80.44% | 44.58% | 60.63% |

结论：

- 三个模型均未达到 provider audit R2 85%，更没有达到 95%。
- Mimo 最接近，但聚合 provider R2 仍只有 79.88%，且 CCS denominator 口径只有 55.25%。
- DeepSeek provider R2 只有 50.89%，并出现一个 wrapper timeout。
- BigPickle provider R2 60.63%，其中 MiroFish 只有 2.05%，说明同一模型不同项目差异非常大。

## 四、opencode 原生对照矩阵

opencode 不经过 panda / ZenProxy / CCS。分块 run dir：

```text
.local-dev/runs/opencode-native-project-matrix-20260706-1142-full
.local-dev/runs/opencode-native-project-matrix-20260706-1204-deepseek-personal
.local-dev/runs/opencode-native-project-matrix-20260706-1208-deepseek-outlook
.local-dev/runs/opencode-native-project-matrix-20260706-1240-mimo-full
.local-dev/runs/opencode-native-project-matrix-20260706-1258-bigpickle-full
```

| model | case | CCS exit | CCS s | CCS chars | audit R2 | opencode exit | opencode s | opencode chars | speed winner | fuller output |
|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| deepseek-v4-flash | tide | 0 | 635.9 | 188 | 69.49% | 0 | 896.8 | 10690 | CCS | opencode |
| deepseek-v4-flash | mirofish | 0 | 188.2 | 5088 | 12.84% | 0 | 161.8 | 4285 | opencode | CCS |
| deepseek-v4-flash | personal-cleanup | timeout | 1800.1 | 2102 | 52.33% | 0 | 143.6 | 1451 | opencode | CCS |
| deepseek-v4-flash | outlook-register | 0 | 126.3 | 6353 | 58.88% | timeout | 1800.1 | 0 | CCS | CCS |
| mimo-v2.5 | tide | 0 | 276.4 | 3770 | 84.66% | 0 | 338.8 | 1552 | CCS | CCS |
| mimo-v2.5 | mirofish | 0 | 135.3 | 4224 | 73.87% | 0 | 134.5 | 3076 | opencode | CCS |
| mimo-v2.5 | personal-cleanup | 0 | 278.2 | 1345 | 73.79% | 0 | 500.1 | 1252 | CCS | CCS |
| mimo-v2.5 | outlook-register | 0 | 258.5 | 7620 | 76.51% | 0 | 123.6 | 6145 | opencode | CCS |
| big-pickle | tide | 0 | 640.7 | 6275 | 67.95% | 0 | 605.0 | 7709 | opencode | opencode |
| big-pickle | mirofish | 0 | 119.9 | 6193 | 2.05% | timeout | 1800.0 | 0 | CCS | CCS |
| big-pickle | personal-cleanup | 0 | 346.9 | 1911 | 55.35% | 0 | 302.6 | 1134 | opencode | CCS |
| big-pickle | outlook-register | 0 | 402.6 | 6154 | 76.10% | 0 | 67.0 | 5666 | opencode | CCS |

聚合：

| model | CCS elapsed_s | opencode elapsed_s | CCS timeouts | opencode timeouts | CCS chars | opencode chars |
|---|---:|---:|---:|---:|---:|---:|
| deepseek-v4-flash | 2750.5 | 3002.3 | 1 | 1 | 13731 | 16426 |
| mimo-v2.5 | 948.4 | 1097.1 | 0 | 0 | 16959 | 12025 |
| big-pickle | 1510.0 | 2774.6 | 0 | 1 | 20533 | 14509 |

质量判断：

- DeepSeek：原生 Tide 输出是完整审计报告；反代 Tide 只输出“等待工作流完成”，属于明显质量失败。MiroFish/Outlook 反代可用；personal 反代 wrapper timeout，原生稳定完成。
- Mimo：反代四项均完成，输出通常更完整；原生没有显著质量优势。Mimo 的主要问题不是输出质量，而是 provider R2 卡在 79.88%、无法达 85/95。
- BigPickle：原生 Tide 更好；原生 MiroFish timeout。反代 BigPickle 输出可用，但缓存稳定性差，尤其 MiroFish provider R2 只有 2.05%。

## 五、根因结论

本轮矩阵证明：

1. **CCS/NewAPI 字段透传不是当前低缓存的主因。** 本地矩阵直接读取 provider audit R2，仍只有 DeepSeek 50.89%、Mimo 79.88%、BigPickle 60.63%。
2. **USK/CCP identity 稳定不等于 provider raw body cache 命中。** DeepSeek Tide 中同一 session/node/prefix/tools 仍出现 90%+ 与 0-10% 交替，说明 provider 真实可缓存段仍被 ClaudeCode growing tool history、角色序列或动态内容影响。
3. **Mimo 稳定性优化有用但不等于缓存达标。** 新策略降低 empty_output 长尾重试风险；真实矩阵里 Mimo 无错误，但 R2 仍未达 85。
4. **BigPickle 当前不适合作为 ClaudeCode 日常开发主力模型。** 它在部分项目能输出可用报告，但 provider R2 和项目间波动不可接受。

## 六、不能上线宣称的内容

禁止宣称：

- 三模型缓存已达到 85% / 95%。
- NewAPI/CCS 显示字段统一后就能自然提升缓存。
- synthetic 8k roundtrip 通过即可代表日常开发。
- 小窗口或单项目 Mimo 84%+ 就代表整体达标。

可以宣称：

- 本地四项目真实矩阵已经跑完。
- 当前实现提高了可观测性、DeepSeek 400 fallback 覆盖和 Mimo empty_output 长尾控制。
- 真实 provider audit R2 仍未达标，不能作为 cache fix 发布到 panda。

## 七、下一步

P0：

- 把 provider raw body/cacheable segment 稳定性作为主目标，而不是继续只改 USK 或 UI 字段。
- 在 audit 中增加同一 `prompt_cache_key` 下 raw cacheable segment hash、provider read/miss、ClaudeCode turn index 的对照。

P1：

- 对 ClaudeCode 工具历史做 provider cache_control 分段：稳定系统提示/tools/schema 与动态 tool_result/assistant history 分离，避免动态段污染可缓存前缀。
- DeepSeek/BigPickle provider 400 fallback 继续保留，但不能用 fallback 代替 raw cache 修复。

P2：

- BigPickle 改为专项策略：要么走 native/opencode-like path 重新验收，要么从 ClaudeCode 日常开发模型中降级。
- Mimo 继续保留大输入 retry cap，并补充 50k/200k 长输入 FRT 分位数验收。

上线门槛：

- 本地四项目矩阵 provider audit R2 至少 Mimo/DeepSeek 85%+，BigPickle 有明确替代策略。
- CCS/NewAPI/ZenProxy 三方同一窗口统计口径一致。
- panda 只通过 GitHub release 短窗口部署验证，不在 panda 编译。
