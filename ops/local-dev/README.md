# 本地 Cache 验收（Webshare IP 白名单）

在**不动 panda 生产**的前提下，用本机 IP（Webshare 白名单）跑完整 zen-proxy → Webshare → opencode 链路。

**状态同步（已实现 / 未完成 / 两套链路说明）见 [`STATUS.md`](./STATUS.md)。**

## 前提

1. Webshare 控制台已授权本机公网 IP（与 `curl ifconfig.me` 一致，当前应为 `203.10.99.11`）。
2. WSL 出口 IP 与授权 IP 一致（Windows 代理勿劫持 WSL 出网）。
3. `ssh panda` 可用（仅用于拉取 `nodes-prod.json`，不上传二进制）。

## 快速开始

```bash
cd /home/lenovo/zen-free-model-suite

# 1. 预检：拉 nodes、测 Webshare→opencode
bash ops/local-dev/01_preflight.sh

# 2. 启动本地 zen-proxy（默认 127.0.0.1:14000）
bash ops/local-dev/run-local-zenproxy.sh

# 3. 另一终端：多轮 smoke + cache gate
bash ops/local-dev/run_cache_matrix.sh
```

## cc-switch / ClaudeCode

> **警告**：仅跑 `run_full_acceptance.sh` **不会**改变 cc-switch。你在 `D:\SelfMadeTool\*` 等目录手动开的 ClaudeCode **仍走生产 NewAPI**，除非先改 cc-switch。详见 [`STATUS.md`](./STATUS.md) 第四节。

临时把 provider `base_url` 指到本地（**测前改、测后恢复**）：

```text
base_url: http://127.0.0.1:14000
api_key:  local-dev-proxy
```

四项目真实矩阵可以用脚本自动切换/恢复 cc-switch provider：

```bash
python3 ops/local-dev/run_ccswitch_project_matrix.py --smoke
python3 ops/local-dev/run_ccswitch_project_matrix.py
```

脚本会备份 `C:\Users\Lenovo\.cc-switch\settings.json` 与 `cc-switch.db`，按模型切换
`local-zen-deepseek` / `local-zen-mimo` / `local-zen-bigpickle`，跑完自动恢复原 provider。

模型：`deepseek-v4-flash` / `mimo-v2.5` / `big-pickle`

**固定同一 workspace 目录**跑完全部轮次（不要 per-request 换目录）。

opencode 原生对照矩阵（不经过 CCS / ZenProxy / panda）：

```bash
python3 ops/local-dev/run_opencode_native_matrix.py --smoke
python3 ops/local-dev/run_opencode_native_matrix.py
```

默认模型映射：

| public model | opencode model | variant |
|---|---|---|
| `deepseek-v4-flash` | `opencode/deepseek-v4-flash-free` | 默认 |
| `mimo-v2.5` | `opencode/mimo-v2.5-free` | 默认 |
| `big-pickle` | `opencode/big-pickle` | `max` |

**重要**：ClaudeCode 主路径是 Anthropic `/v1/messages`，不是 OpenAI chat/completions。本地实测：

| 路径 | 状态 |
|------|------|
| `POST /v1/messages` | ✅ 200（ClaudeCode 应用此路径） |
| `POST /v1/chat/completions` | ❌ 502 stream truncated（勿用此路径验收 cache） |

缓存探针请用：

```bash
bash ops/local-dev/03_cache_roundtrip.sh   # Anthropic 两轮前缀增长
bash ops/local-dev/run_cache_matrix.sh     # 多轮 smoke（已加 --force 跳过 chat preflight）
```

## 验收门槛（本地 gate）

```bash
bash ops/local-dev/cache_gate.sh
# 或严格模式
python3 ops/cache_quality_acceptance.py .local-dev/audit/requests-$(date +%Y-%m-%d).jsonl --strict
```

| 模型 | R2 底线 |
|------|---------|
| deepseek-v4-flash | 90%（冲刺 95%） |
| mimo-v2.5 | 85% |
| big-pickle | 85% 或专项策略 |

仅当本地 gate 通过后再 GitHub release → panda 生产复测。

## 与生产的差异

| 项 | 本地 | 生产 |
|----|------|------|
| 出口 IP | 本机 203.10.99.11 | panda 43.156.233.219 |
| L3 cache 分区 | **不同 IP 桶** | 生产桶 |
| 代码路径 | 本地 release 构建 | 已部署二进制 |
| NewAPI 层 | **手动 ClaudeCode 仍走生产**；仅 env 注入的自动化脚本可跳过 | channel 69 |

本地证明的是 **同一内核 + 同一 Webshare 池 + 真实工具链** 下 R2 能否达标；上线后仍需 panda 短窗口 smoke，但不应再出现「未经验证就全量替换」。

## 数据目录

- `.local-dev/nodes-prod.json` — 从 panda 拉取，**勿提交 git**
- `.local-dev/audit/` — 本地 audit JSONL
- `.local-dev/runs/` — pressure runner 产物
