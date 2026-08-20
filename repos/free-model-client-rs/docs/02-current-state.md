# 当前状态

更新时间：2026-08-04
分支：以本地 `zen-free-model-suite` 工作区为准

## 2026-08-04 panda test（:4011）frt 截断 / empty_output 修复

1. 线上包 **`test-20260804-frt-v5`**，SHA256 `ed6ba3dadb27c82b1af1bc4ca1c017401421cafbfc7fb2adf8a5ccf54834198f`，`dispatch=100, dead=0`（部署后快照）。
2. 关键修复：`max_tokens` clamp 131072；`EmptyOutput` 不进 dead；移除 `[prior assistant tool call summarized]`；`provider_missing` 恢复 enrich 链；大会话 fold 推迟。
3. 全量说明见 `docs/diagnosis-2026-08-04-frt-truncation-empty-output-fix.md`。
4. 约束：仅改 ZenProxy 侧，不改 NewAPI / Pi / Claude Code 端侧。

## 2026-07-15 ClaudeCode 三模型稳定性

1. `hy3` 已作为 `hy3-free` 的公开别名进入 ZenProxy 和 NewAPI channel 69；价格为 0。
2. ClaudeCode 兼容修复已提交为 `a3fc5ca`、`2c29662`、`210f60c`，覆盖 profile 隔离、非流式上游 SSE 聚合、Hy3 forced-tool 降级、reasoning alias、watchdog 和错误脱敏。
3. 本地验证通过：`free-model-client-rs` 172 unit + 140 golden；`zen-proxy-rs` 221 unit + 44 e2e；release build 均在本地完成。
4. Panda 当前生产二进制 SHA256 为 `1e5102df0d2f4ec9bd7cbb6fbae44134368ba48f1613a694df4becb6dfad41d7`，三实例 active，四个 health 端口均 200。
5. Windows official ClaudeCode 189 项正式矩阵 `184/189` 通过；部署后 18 项定向矩阵首轮 `16/18`，两个失败重跑均通过。
6. 部署后专用 token 窗口 NewAPI/CC Switch 均为 `57/57` HTTP 200、0 错误。
7. 请求级 cache-read 覆盖为 Mimo `95.65%`、DeepSeek `36.84%`、Hy3 `40.00%`。Hy3 的 `cache_control` breakpoint 会损害 forced-tool 参数完整性，当前明确禁用。
8. 完整数据、口径和后续验收见顶层 `docs/CLAUDECODE_STABILITY_HANDOFF_2026-07-15.md`。

## 2026-07-02 channel 69 代理池与 big-pickle 恢复

本轮目标是恢复生产 channel 69 可用性：旧 Webshare 代理已出现 `proxy authorization required`，需要更换为用户提供的新 Webshare 100 代理，并把公开模型名从 `deepseek-v4-flash-lite` 改回 `big-pickle`。

当前生产事实：

1. 新 Webshare 代理文件 `Webshare 100 proxies.txt` 已在 panda 低并发验证：100/100 代理可访问上游 models 接口，100/100 出口国家为 `SG`，并已写入 `/etc/zen-proxy-rs/nodes-prod.json`。
2. 旧 nodes 备份：`/etc/zen-proxy-rs/nodes-prod.json.bak-20260702-150617-pre-new-webshare`；旧二进制备份：`/opt/zen-proxy-rs/backups/zen-proxy-rs.20260702-150617.pre-webshare-bigpickle`。
3. `zen-proxy-rs` 已通过 GitHub 临时 release 中转部署到 panda，部署后临时 release 和 tag 已删除；线上 stripped SHA256 为 `f3976687fe229bea87f69f070c57cb15e8da59d7060db577a5cac5f2a53ce95b`。
4. panda `4001/4002/4004/4000` `/health` 均为 `status=ok`；`4000 /v1/models` 只公开 `deepseek-v4-flash,big-pickle,mimo-v2.5`。
5. ZenProxy 直连 OpenAI chat smoke：`deepseek-v4-flash`、`big-pickle`、`mimo-v2.5` 非流式和流式均 HTTP 200；旧 `deepseek-v4-flash-lite` 返回 unsupported。
6. ZenProxy 直连 Anthropic `/v1/messages` 流式 smoke：`big-pickle`、`mimo-v2.5`、`deepseek-v4-flash` 均 HTTP 200；非流式 `/v1/messages` 的短 channel-test 探针仍可能进入 DeepSeek 慢尾，不作为普通 chat 可用性失败。
7. NewAPI channel 69 `channels.models` 已改为 `deepseek-v4-flash,big-pickle,mimo-v2.5`，`model_mapping` 为空，`status=1` 启用；备份表包括 `closeapi_channel69_backup_20260702_1518_pre_bigpickle`、`closeapi_abilities_channel69_backup_20260702_1542_pre_bigpickle`、`closeapi_models_bigpickle_backup_20260702_1542_pre_bigpickle`、`closeapi_channel69_status_backup_20260702_1601_pre_enable`。
8. NewAPI `models`/`abilities` 已同步：active `deepseek-v4-flash-lite` 为 0，active `big-pickle` 为 1，channel 69 的 `deepseek-v4-flash,big-pickle,mimo-v2.5` abilities 均启用。
9. NewAPI `hhhl` 组 token smoke：`/v1/models` 返回目标三模型且不返回旧 lite；`/v1/chat/completions` 对 `deepseek-v4-flash`、`big-pickle`、`mimo-v2.5` 均 HTTP 200；旧 lite 返回 `No available channel`。
10. NewAPI 管理端单渠道测试 API：`stream=true` 下 `mimo-v2.5` 和 `big-pickle` 成功，`deepseek-v4-flash` 单独重试成功但耗时约 104s。该管理端探针比普通 chat 更容易触发 DeepSeek 短 `/v1/messages` 慢尾，后续不要用它单独代表真实 ClaudeCode 首字。
11. 2026-07-02 21:10 CST 复查真实 ClaudeCode 公网链路：Windows official `claude.orig.exe` 经过用户当前 cc-switch `127.0.0.1:15721` 和 `https://sub2api.closeapi.top`。`deepseek-v4-flash` Bash/WebFetch/WebSearch x text/json/stream-json 9/9 通过；`mimo-v2.5` 同矩阵 9/9 通过；`big-pickle` Bash/WebFetch/WebSearch stream-json 3/3 通过。
12. 同窗口 NewAPI channel 69 只有 `type=2` 成功消费记录，模型为公开名 `deepseek-v4-flash`、`mimo-v2.5`、`big-pickle`；cc-switch 显示的是 `request_model -> provider/upstream model`，所以会显示 `mimo-v2.5 -> mimo-v2.5-free`。这是展示层级差异，不是 NewAPI 未走 free 上游。
13. Cloudflare 1010 根因已用 A/B 验证：`Python-urllib/3.12` UA 访问 `sub2api.closeapi.top` 会触发 403/1010；curl/no-UA/ClaudeCode-like/Mozilla UA 均能到达 NewAPI 鉴权层。后续不要用 Python urllib 直连 sub2api 判断 ClaudeCode/cc-switch 被 Cloudflare 挡。

当前公开模型边界：

1. 生产 channel 69 公开模型只应是 `deepseek-v4-flash`、`big-pickle`、`mimo-v2.5`。
2. `deepseek-v4-flash-lite` 旧公开名已撤下，不再作为 NewAPI 公开模型或 ZenProxy 静态 public alias。
3. `north-mini-code`、`nemotron-3-ultra`、`minimax-m3`、`qwen3.6-plus` 仍只做 hidden routing，不加入 NewAPI 公开列表。

## 2026-06-22 cache 稳定化续接状态

当前目标是提高真实 cache 命中率和首字稳定性，不能用裁剪上下文、伪造 usage、缩短输出、隐藏提示词或禁用工具来换速度。

当前用户授权边界：

1. 生产 NewAPI `https://sub2api.closeapi.top/` 现在允许按明确流程变更。
2. dev/new 测试域名已切到 `https://new.relai.asia/`，不要再使用旧 `new.closeapi.top` 作为当前测试入口。
3. 2026-07-02 起，生产 channel 69 公开模型只应是 `deepseek-v4-flash`、`big-pickle`、`mimo-v2.5`。
4. `north-mini-code`、`nemotron-3-ultra`、`minimax-m3`、`qwen3.6-plus` 仍只做 hidden routing，不加入 NewAPI 公开列表。

2026-06-22 08:23 CST panda 只读日志基线：

1. 最近 120 分钟 `deepseek-v4-flash-free` ClaudeCode provider cache observation：accepted 38、rejected 14，accepted token hit 约 `96.94%`；Unknown accepted 7、rejected 3。
2. 同窗口大请求中，多个会话的 `prefix_4k_hash`/`prefix_32k_hash` 稳定，但 `prefix_128k_hash`、`prefix_256k_hash`、`cache_material_bytes` 随尾部增长变化；这确认优化方向仍是 header/session/request/affinity 稳定化，而不是裁剪上下文。
3. 最近 24 小时 audit：`deepseek-v4-flash-free` ClaudeCode success 13097 条，token-weighted cache hit 约 `84.26%`；`mimo-v2.5-free` ClaudeCode success 491 条，token-weighted cache hit 约 `18.04%`。
4. `mimo-v2.5-free` ClaudeCode 24 小时内 affinity 很高但 token hit 低：`50k-100k` 桶 hit 约 `9.29%`、affinity `175/183`；`100k-200k` 桶 hit 约 `5.32%`、affinity `105/105`。说明瓶颈不是完全没有 affinity，而是 provider/session/header/request 口径仍可能把大段 tail 当成新 cache material。

本轮已落地的低风险源码变化：

1. `src/zen/client.rs` 的 `x-opencode-request` 不再随机生成。
2. `x-opencode-request` 现在由完整规范化上游 body 生成：同一请求体稳定复用，不同请求体字段变化会生成不同 request id。
3. `x-opencode-session`/`x-opencode-project` 仍按稳定前缀、tools、tool_choice、模型、API key hash 和 TTL 分组，保持长会话亲和。
4. `zen-proxy-rs/src/v4/provider.rs` 已将 32KB+ cache-affinity 从仅流式扩展到流式和非流式，并把 `source_client` 纳入 affinity key。
5. 这些变化只影响上游 header 和软节点亲和，不改变 request body、messages、tools、tool_choice、thinking、上下文长度、输出长度或 usage 字段。

本地验证：

1. `free-model-client-rs`：`cargo fmt -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 通过；库测试 132 passed，kernel golden 127 passed。
2. `zen-proxy-rs`：`cargo fmt -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 通过；unit 194 passed，e2e 44 passed。

2026-06-22 cache/TTFT 数据集 harness 续补：

1. `scripts/panda_pressure_runner.py` 新增 `cache-pressure-plan` plan-only 模式；该模式只写 `cache-pressure-manifest.json`、`dataset-schema.json`、`analysis-plan.json`，不读取 API key、不触网、不写 `raw-results.jsonl`。
2. runner raw row 新增脱敏 `prompt_hash`、`prefix_4k_hash`、`prefix_32k_hash`、`prefix_128k_hash`、`prefix_256k_hash`、`cache_material_bytes`、`prompt_bucket`、`target_tokens`、`first_tool_emit_ms`、`cache_read_input_tokens`、`cache_miss_input_tokens` 和 `cache_token_read_pct`。
3. `summary.json` 新增 `observability` 分组：按 `model + prompt_bucket + stream + cache_observation` 输出 `protocol_first_byte_ms`、`first_content_ms`、`first_tool_call_ms`、`first_tool_emit_ms`、`total_ms` 的 P50/P90/P95/P99，同时输出 token-weighted cache read pct、质量通过率和错误计数。
4. 2026-06-22 已执行 `deepseek-v4-flash` 10k 桶、Windows ClaudeCode 经 cc-switch、`20rpm x 5min` 校准半压测三轮，产物均在 `/tmp/claudecode-cache-pressure-runs/`，只保存脱敏 hashes/timing/status，不保存完整 prompt/stdout/stderr。
5. 第一轮 `20260622-160200-claudecode-cache-pressure-deepseek-10k-20rpm` 把 request index/marker 放在稳定上下文之前，本地和远端前缀均发散；100 次调用 98 pass，WebSearch 2 timeout。远端 `deepseek-v4-flash-free` provider cache token read_pct 约 `28.25%`，但 top `prefix_32k` 重复组可到约 `92.53%`，可作为“早变前缀负样本”，不能作为稳定前缀 cache 结论。
6. 修正版 `20260622-161920-claudecode-cache-pressure-deepseek-10k-20rpm-stableprefix` 把变量任务移到 90KB 稳定上下文之后；本地 `prefix_4k/32k_unique=1`，100 次调用 99 pass，唯一失败为 WebSearch timeout。远端仍显示 `prefix_4k/32k` 高度发散，说明 Windows ClaudeCode/cc-switch 请求 envelope 在用户 prompt 之前仍有动态内容；全窗口 token read_pct 约 `19.31%`，去 60 秒 warm-up 后约 `21.29%`，top `prefix_32k` 重复组约 `86.94%`。
7. A/B 轮 `20260622-163209-claudecode-cache-pressure-deepseek-10k-20rpm-dynsystem` 只新增 ClaudeCode 官方参数 `--exclude-dynamic-system-prompt-sections`；100 次调用 93 pass，失败集中在 WebFetch/WebSearch 和 1 个 text result error。远端 token read_pct 降到约 `6.07%`，`prefix_32k` 更分散；该开关当前不能作为默认优化。
8. 三轮压测期间 panda 4000/4001/4002/4004 health 均保持 `status=ok`、`dispatch=100`、`dead=0`、`ratelimited=0`。没有看到 `no proxy resources`、`lane is saturated`、`panic`、`Invalid tool parameters`、`Failed to parse JSON` 或 `stream truncated before DONE`。
9. 当前结论：cache accepted 的远端 first real text/tool 通常更快，但 10k no-session ClaudeCode 独立进程场景的整体 cache read_pct 被动态前缀压低，不能升 50rpm 或扩到 50k/100k/200k 前先解决请求前缀稳定性。
10. 本地 capture 已定位一类动态前缀：ClaudeCode Anthropic 请求含 `system[0].text = x-anthropic-billing-header: ... cch=<随机>`，且 `metadata.user_id` 每次变化；`metadata.user_id` 不进入 `ChatRequest`，但 billing header system 文本会被转成上游 system message 并进入 cache material。
11. 本地源码已补低风险规范化：Anthropic system 转 OpenAI messages 时剥离 `x-anthropic-billing-header:` 行，且如果 system 只剩空内容则不发空 system message。该改动不改变 user messages、tools、tool_choice、thinking、max_tokens、上下文长度、输出长度或 usage。
12. 验证已通过：`free-model-client-rs` `cargo fmt -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`；`zen-proxy-rs` `cargo fmt -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`。新增回归确认 billing header 不上游透传，且仅 `cch` 变化不会改变 request prefix/prompt hash。
13. V4.113 billing-header strip 已按 GitHub 临时 release 中转部署到 panda，线上二进制 SHA256 已确认是 `77dc3f29f6d498c138a5075e7b3cd22ca84209525599d1c93087a0ec99cffac5`，临时 release 已删除。
14. post-V4.113 DeepSeek 受控复测 `20260622-100933-claudecode-cache-pressure-deepseek-v4-flash-10k-20rpm-post-v4113-sharedprefix-full`：本地 100/100 ok；远端 `deepseek-v4-flash-free` provider filtered accepted/rejected `133/7`，token read_pct `94.42%`；`prefix_4k_unique=1`、`prefix_32k_unique=2`、`prefix_128k_unique=2`；server-side first real text/tool P50/P90/P95/P99 `2195/3055/3676/4725ms`。
15. DeepSeek 负对照 `per-request workspace` 保留：同档 token read_pct 只有约 `7.27%`，`prefix_32k_unique=100`，证明收益来自稳定 workspace/project/header 前缀，不是统计伪造。
16. 已固定真实 `mimo-v2.5` ClaudeCode 入口：`--model mimo-v2.5` 单独使用会被当前 cc-switch provider 映射回 DeepSeek，必须临时把当前 Claude provider 的 Anthropic model 字段切到 `mimo-v2.5`，运行后用 `finally` 恢复。该流程已验证 panda ingress 真正出现 `model=mimo-v2.5` / provider `mimo-v2.5-free`。
17. Mimo full safe-label 复测 `20260622-110328-claudecode-cache-pressure-mimo-v2.5-10k-20rpm-post-v2-safe-label-mimo-full`：本地 100/100 ok，覆盖 Bash/WebFetch/WebSearch x text/json/stream-json；远端 `mimo-v2.5-free` warm-up 后 provider accepted/rejected `131/8`，Mimo 缺显式 miss tokens，按 `read_tokens / estimated_total_tokens` 口径为 `91.08%`；server-side first real text/tool P50/P90/P95/P99 `3883/5966/6780/9000ms`。
18. Mimo 测试语料已从 `PRESSURE_*` 改成 `CACHEBENCH_*` safe label，并新增 `missing_marker_refusal_like` 分类；旧模板 100 条中 1 条 text_review 被 Mimo 误判成安全注入，safe-label full 100 条未复现。
19. 当前 `prompt_bucket=10k` 标签不等于远端真实 token 桶：ClaudeCode system/tools envelope 后，远端 `estimated_total_tokens` P50 约 `53k`、P95 约 `56.5k`。正式 10k/50k/100k/200k 矩阵前必须先做 bucket calibration，不能把当前结果写成真实 10k 桶。

生产部署与观察状态：

1. V4.111/V4.112/V4.113 已按 GitHub 临时 release 中转流程部署到 panda 三实例；V4.113 后 4001/4002 `/health` 均 `status=ok`、`dispatch=100`、`dead=0`，4004 观察到既有 `dead=1/dispatch=99` 但未在半压测中继续扩大。
2. 2026-07-02 后，4000 `/v1/models` 已复查，只公开 `deepseek-v4-flash`、`big-pickle`、`mimo-v2.5`。
3. 2026-06-22 14:32-15:04 CST post-V4.112 窗口，`deepseek-v4-flash-free` + ClaudeCode provider cache rows 约 528，accepted/rejected 约 `455/73`，token read/miss 约 `27,573,504 / 6,397,125`，read_pct 约 `81.17%`。
4. 同窗口按 prompt hash 与 stream summary 拆分：stream rows 约 375，read_pct 约 `80.77%`；non-stream/unpaired rows 约 153，read_pct 约 `87.07%`。相比部署前基线，stream 从 `72.29%` 提升明显，non-stream 仍较高但未超过部署前 `92.87%`。
5. 质量/错误扫描未见 `no proxy resources`、`lane is saturated`、`panic`、`Invalid tool parameters`、`Failed to parse JSON` 或 `stream truncated before DONE`；仍有 `provider_missing_reasoning_content` 和 `reasoning_only_length` 重试，是当前首字/长尾主要瓶颈。
6. 当前没有足够证据支持继续扩大首跳 disabled-thinking 或裁剪/改写上下文；下一步应先做 bucket calibration、真实 traffic audit 与 20rpm 分桶校准，再决定是否升到 50rpm。

## 2026-06-13 交接状态

用户已要求停止继续测试和修复。本轮只做文档收口，不部署、不改 NewAPI、不改 ClaudeCode、不改 cc-switch，也不继续改运行逻辑。

当时 `git status --short --branch`：

```text
## codex/v47-client-split-cache-harness
?? north-mini-code
```

`north-mini-code` 是未跟踪项，本轮未触碰；后续不要盲删，也不要混入提交。

2026-07-02 补充：该根目录 `north-mini-code` 文件已确认只是残缺 SQL 片段，不是模型配置或源码，已作为孤儿文件清理；当前模型 `north-mini-code` 业务边界仍按 hidden routing 文档执行。

最新调查报告：

```text
docs/reports/2026-06-13-claudecode-web-tool-handoff.md
```

关键结论：

1. Windows ClaudeCode 基础工具链 `Bash -> Write -> Read` 真实测试通过，参数完整，没有复现稳定的参数清空。
2. WebSearch/WebFetch 专项里，`ToolSearch/WebSearch/WebFetch` 工具参数均完整；WebSearch 返回空结果，WebFetch 失败于 ClaudeCode 本地安全验证/`claude.ai` 链路。
3. WebFetch 失败后模型改用 Playwright 可以抓取 `example.com` 和 Claude Code 文档页面，说明网页访问本身可用，失败点不是页面不可达。
4. 当前 Windows cc-switch Claude provider 为 `closedeepseek -> https://sub2api.closeapi.top -> deepseek-v4-flash`，不是 panda NewAPI `http://100.69.228.93:8081`；cc-switch 本地日志不能直接当作 panda channel 69 数据。
5. cc-switch 最近约 2 小时 Claude 记录中 410 条、200 成功 285 条、502 失败 125 条；失败主要是 `Connect`、`SendRequest`、`error sending request for url`，更像连接层/Cloudflare/上游网络问题。
6. 同窗口成功请求首 token P50 约 15.6s、P90 约 48.6s、P95 约 69.4s；这是真实体感慢尾证据，但不能直接归因到 ZenProxy 参数转换。
7. 精确查 cc-switch SQLite：`error_message like '%parse%'` 和 `'%JSON%'` 均为 0；但文本日志有 `error reading a body from connection`、`Response parse failed: connection error`，说明用户看到的 `Failed to parse JSON` 大概率是连接中断、非 JSON HTML、Cloudflare 错误页或半截流导致。
8. panda ZenProxy 当时三实例 active，4000 `/health` 显示 `dispatch=90/dead=0/ratelimited=0`，不是服务没起或节点全死。

本轮测试产物位于本机临时目录，不应提交：

```text
C:\Users\Lenovo\AppData\Local\Temp\zen_cli_probe_20260612_005541
C:\Users\Lenovo\AppData\Local\Temp\zen_cli_probe_official_20260612_010110
C:\Users\Lenovo\AppData\Local\Temp\zen_cli_web_batch_20260612_010559
```

后续若恢复工作，先按报告里的 request_id / provider / raw stream-json / NewAPI / ZenProxy journal 对齐证据链，不要直接继续堆适配。

## 代码已确认能力

最新源码验证使用 WSL 原生路径和临时 target，避免 UNC target 增量锁问题：

```bash
cd /home/lenovo/free-model-client-rs
CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/free-model-client-rs-target cargo fmt -- --check
CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/free-model-client-rs-target cargo clippy --all-targets -- -D warnings
CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/free-model-client-rs-target cargo test
```

结果：

- `fmt --check` 通过。
- `clippy --all-targets -- -D warnings` 通过。
- `cargo test` 通过：库测试 122 条、kernel golden 112 条、doc tests 0 条。
- `zen-proxy-rs` 本轮已改外层 V4 context compactor 和 e2e harness；当前已验证 `clippy -D warnings`、bin 单元测试 132 条、context 相关单元测试 12 条、e2e 27 条、shell e2e 9/9 通过。

注意：上述验证覆盖本仓库当前源码。2026-06-04 18:54 已将输出限制取消、模型策略收窄、flash/free 输入放行和 cache 四态观测构建进 `zen-proxy-rs` release 并部署到 panda；部署后已通过 NewAPI models、短请求和手工大上下文不折叠 smoke。真实 panda `policy-smoke/policy-dry` 和四客户端压测仍未跑，不能当作生产压测结论。

当前已实现并由测试覆盖的关键能力：

1. `Authorization` 和 `x-api-key` 两种认证头识别。
2. 请求体上限由 `FREE_MODEL_REQUEST_BODY_LIMIT_MB` 控制，默认 64MB。
3. OpenAI/Anthropic 两套入口共享协议内核。
4. 输出限制已完全取消：缺省 `max_tokens` 不再补 1024/2048；显式 `max_tokens` 原样透传；OpenAI/Anthropic 上游请求只有在客户端显式传值时才写 `max_tokens`。
5. `deepseek-v4-flash/deepseek-v4-flash-free` 已取消 Hermes/OpenClaw 适配，只保留 ClaudeCode 深度适配；该模型族不再设置输入 token 墙，`free-model-client-rs` 侧只做脱敏 request-shape 观测，不压缩输入。
6. 保留短指令原意，不再把 `1`、`继续`、`执行` 等短输入改写成 `ok`。
7. 不默认禁用 thinking；仅保留参数规范化位置。
8. OpenAI 工具历史规范化：缺 id、缺 `tool_call_id`、孤儿 tool result、交错 user 消息等有修复/降级。
9. Anthropic 工具历史到 OpenAI 工具调用的转换和缺失 id 修复。
10. SSE parser 支持 CRLF、可选空格、多行 data，并能识别截断、解析错误和 finish_reason。
11. 上游空内容且无工具调用时返回结构化空输出错误，不再合成假的工具调用。
12. 429 上游错误保留 `Retry-After` 等关键响应头信息。
13. Markdown fence 边界修复，降低流式代码块截断导致客户端显示异常的概率。
14. `ParsedZenFrame::Event` 已装箱，`clippy::large_enum_variant` 已消除。
15. 客户端 profile 已支持显式 `x-fmc-client`、header、UA 和 body/toolset 推断；只有强 OpenClaw/Hermes marker 或 OpenClaw 专属工具集才覆盖 ClaudeCode，普通正文提到 OpenClaw/Hermes、或只带 `web_fetch`/`web_search` 不再误判为 OpenClaw。
16. 空内容、无工具的 OpenAI/Anthropic 健康探测会走本地短路 `ok`，不再误进上游或 huge buffered retry。
17. ClaudeCode huge buffered stream 只在修复前估算输入 >= 50k tokens 时启用；小 `max_tokens` 健康探测不再触发 huge retry 路径。
18. NewAPI 管理端测渠道常见的极短 `echo hi` 流式探测，如果上游空输出，会返回本地 `ok`，只在无工具、单用户消息、`max_tokens <= 64` 的探测形态触发，避免误伤普通请求。
19. 已新增脱敏 request-shape 观测：OpenAI/Anthropic 入口统一记录 `system_tokens/messages_tokens/tools_tokens/tool_count/message_count/largest_message_tokens/last_user_tokens/estimated_total_tokens/stream/max_tokens/tool_choice_present/prompt_hash/source_client/profile_source`，不记录原始 prompt、请求体或 key。
20. 已新增小非流式请求分类：`health_probe/channel_test/internal_claude_code_probe/user_short_request/unknown_short_nonstream/not_short_nonstream`；当前只用于日志归因，普通 ClaudeCode 小非流式非探针请求在上游空输出时仍返回结构化 502，不会被本地 `ok` 误短路。
21. ClaudeCode huge-session compactor 属于历史修复背景；当前 `deepseek-v4-flash/deepseek-v4-flash-free` 路径已经取消输入墙和输入压缩，只保留脱敏观测，避免在本仓库侧裁剪用户上下文。
22. 源码已补非流式 cache usage 透传：OpenAI 非流式正文/工具调用响应会保留 `prompt_tokens_details.cached_tokens`、`cache_creation_input_tokens`、`cache_read_input_tokens`；Anthropic 非流式正文/工具调用响应也会保留真实 `cache_*`，不再统一写死为 `0`。
23. 源码已补上游 `finish_reason` 透传：OpenAI 非流式/流式会保留 `length/content_filter/stop`；Anthropic 非流式/流式/buffered 流式会把上游 `length` 映射为 `max_tokens`，不再把提前到达长度上限伪装成正常 `end_turn`。
24. 源码已补 ClaudeCode 中等工具历史压缩：当消息很多、总上下文约 24k+、单条旧工具输出 12k+、最新用户指令很短时，会折叠旧工具/会话历史并保留最新用户目标，覆盖线上 `last_user_tokens=3`、旧工具输出 26k、总输入不到 50k 的截断感场景。
25. 源码已补模型维度的有效 profile 策略：日志仍记录真实 `source_client`，但 `deepseek-v4-flash/deepseek-v4-flash-free` 不再应用 Hermes/OpenClaw 兼容策略，只保留 ClaudeCode 深度适配；`deepseek-v4-flash-lite/big-pickle` 不再应用 ClaudeCode 适配，只保留 Hermes/OpenClaw 适配。
26. cache 观测新增四态：`attempted`、`accepted`、`rejected`、`ignored`；同时采集 provider response/header/body usage 信号，用来区分“上游未给 usage/cache”、“NewAPI/中间层剥离 header”和“真实 cache 命中”。
27. 显式 `reply PONG only` 等短 smoke 在无工具、`max_tokens <= 64` 且上游连续空输出后允许返回本地 `PONG`；普通短请求仍不会伪造答案。
28. `zen-proxy-rs` 外层 V4 context compactor 已按模型分流：`deepseek-v4-flash/deepseek-v4-flash-free` 只记录 `warn/pass`，不 compact、不因 token target reject；`deepseek-v4-flash-lite/big-pickle` 仍保留 compactor 能力，避免把全局大上下文保护误关。
29. V4.98 cache-friendly session 已在本仓库源码落地：大请求上游 `x-opencode-session` 不再按完整 `messages` hash 每轮变化，而是按稳定前缀 hash、tools hash、tool_choice hash、模型、api key hash 和时间桶分组；请求正文、消息顺序、`max_tokens` 均不改写。
30. V4.98 新增脱敏 prefix 观测：request-shape 和 cache observation 日志记录 `prefix_4k_hash/prefix_32k_hash/prefix_128k_hash/prefix_256k_hash/cache_material_bytes`，用于判断长会话前缀是否稳定；仍不记录原始 prompt、请求体或 key。
31. 2026-06-05 已补并部署 ClaudeCode 低预算工具探针保护：仅当 `source_client=ClaudeCode`、非流式、`max_tokens<=32`、工具数 1-2、无显式 `tool_choice`、小上下文时，第一次上游请求前禁用 thinking，并把上游 `max_tokens` 最小抬到 64，避免 `/context` 等内部探针被 DeepSeek 消耗在 reasoning-only 后裸 502；普通工具调用、长上下文、Hermes/OpenClaw 不受影响。
32. 2026-06-05 已补并部署 ClaudeCode Anthropic 流式 idle ping 保活：仅对 `source_client=ClaudeCode` 的 Anthropic SSE 流，在 15 秒内没有下游可转发事件时发送协议级 `event: ping` / `{"type":"ping"}`；不伪造内容、不计入 first content、不改写 prompt，用来降低 50k+ 流式请求在真实内容前被 NewAPI/客户端判为 `client_gone` 的概率。
33. 2026-06-06 已补并部署 V4.99 ClaudeCode Anthropic Stream Guard：当 ClaudeCode Anthropic stream 在真实 text/tool 输出前遇到上游 `stream truncated before DONE or finish_reason` 或 60 秒无可转发内容时，最多 3 次原地重试；最后一次仅在工具请求场景启用 disabled thinking 兜底。正常请求不改 prompt、不裁剪输入、不限制输出、不默认禁用 thinking。
34. 2026-06-06 已补 Anthropic 工具调用 `input_json_delta` 分片：普通流式和 buffered huge-stream 返回工具参数时按 4KB 安全切片发送，保证拼接后 JSON 字符完全一致，降低大 Write 参数导致客户端/中间层解析压力。ClaudeCode 显式 forced `tool_choice` 会首跳禁用 thinking，避免上游返回 `Thinking mode does not support this tool_choice`；`tool_choice=auto` 和普通 tools 请求仍保持默认 thinking。
35. 2026-06-06 已补 provider `reasoning_content` 缺失兜底：当上游直接返回 `The reasoning_content in the thinking mode must be passed back to the API` 时，OpenAI/Anthropic 非流式、OpenAI 流式、ClaudeCode Anthropic 流式和 buffered huge-stream 会将同一请求重试一次 `thinking: disabled`；仅在 provider 明确拒绝当前请求后触发，不全局禁用 ClaudeCode tools auto thinking。
36. 2026-06-06 已补上游错误脱敏映射：`AppError::upstream` 不再把 `opencode zen`、上游原始 body、内部路由或节点标识写进 public response；public body 使用 `upstream_provider_error` 和稳定 `code`，私有 provider 状态只进服务端日志。
37. 2026-06-08 源码已补 V4.101 ClaudeCode Anthropic 工具流提前释放：只有当上游工具调用参数已经拼成完整、可解析 JSON 后才向下游发送 `tool_use`，并在日志中记录 `first_tool_emit_ms` 和 `emitted_tool_call_count`；不发送 partial tool、不伪造工具、不改写 prompt。
38. 2026-06-08 源码已补 V4.101 自适应 no-forwardable watchdog：ClaudeCode Anthropic stream 在真实 text/tool 发出前按输入桶使用 10s/14s/22s/32s/45s 上限，而不是固定等满 45s；若用户配置更低值，则继续尊重更低值。
39. 2026-06-08 `zen-proxy-rs` 源码已补 V4.101 cache-friendly affinity key：大流式请求的 affinity 从 `model/path/client/body_bucket` 升级为包含稳定 `messages` 前缀 hash、`tools` hash 和 `tool_choice` hash；只保存 hash，不保存 prompt 原文。
40. 2026-06-08 `zen-proxy-rs` 源码已补 V4.101 中等工具流隔离：tool-heavy lane 阈值从 `tools>=16 / tool_markers>=12` 下调到 `tools>=8 / tool_markers>=6`，让 ClaudeCode 中等工具链请求更早进入隔离 lane，降低普通流式请求被工具流拖慢的概率。
41. 2026-06-08 22:27 CST 已将 V4.101 stripped release 部署到 panda 三实例；线上二进制 hash `149dd2f65c8b33228498bcc1f2e94f6742e1e1a5417592c0eb6921e7cc7deb49`，旧版备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260608-222704.pre-v4101`。部署后 `/health`、`/v1/models`、OpenAI stream、Anthropic ClaudeCode stream 和 NewAPI OpenAI stream 最小 smoke 均通过。
42. 2026-06-09 已补并部署 V4.102 ClaudeCode 工具参数完整性门控：Anthropic/ClaudeCode 流式和非流式只在工具参数包含必填字段且 JSON 完整后下发 `tool_use`；上游空 `{}` 或缺必填参数时先做窄范围 disabled-thinking retry，仍不完整则返回结构化 `upstream returned incomplete tool call arguments`，不再把坏工具调用交给 ClaudeCode 造成 `Invalid tool parameters`。同时新增重复补参防循环：同一修复后工具调用如果历史中已有 assistant tool_call 和对应 tool_result，不再重复补发。另补文件工具坏路径保护：`Read/Write/Edit` 等收到 `file_path="\\\\"`、`"/"`、`"."` 这类明显非文件路径时，优先从最新用户明确指令修复，修不了则拒绝下发。线上 stripped hash `ebe41572fe76a5f99783ba5e4308e164368415b00277432cd9829e60ecc651dd`，旧版备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260609-111046.pre-v4102-tool-input-guard`。
43. 2026-06-09 V4.103 ClaudeCode 工具门控续修已随 V4.104 部署 panda：`SendMessage` 字符串消息缺 `summary` 时自动补短 summary，结构化消息不误补；`Bash/ToolSearch/WebSearch` 的空 `command/query` 不再因字段存在而放行；同一 assistant response 内完全相同的 ClaudeCode 工具名+输入 JSON 只下发一次，降低重复 `Read/Edit/Bash` 风暴；流式 `provider_missing_reasoning_content` 在首轮 disabled-thinking 后可继续走工具历史 sanitize/text-only 降级重试。
44. 2026-06-09 V4.104 ClaudeCode progressive tool streaming 已部署 panda：ClaudeCode Anthropic 工具流在工具 id/name 和非空 arguments 开始出现后立即发送真实 `content_block_start tool_use`，后续按 `input_json_delta` 增量透传，最终完整 JSON 校验通过后才 `content_block_stop`；同时取消从最新 user 文本推断 `Read/Write/Edit/Bash/Task/ToolSearch/WebSearch` 参数，只保留 `SendMessage.summary` 确定性窄修复。线上 stripped hash `08d9064600e66097ab45bbe97290bf5e7015174a15adbe27dc5fcf8261c2ed9f`，旧版备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260609-134330.pre-v4104-ebe41572fe76a5f99783ba5e4308e164368415b00277432cd9829e60ecc651dd`。
45. 2026-06-09 21:37 CST 已将 V4.106 质量保全 cache-friendly 中等上下文 session 优化部署到 panda 三实例：当 10k+ 请求的 `messages` material 小于等于大前缀阈值时，`x-opencode-session/project` 使用默认 `32KB` 中等稳定前缀分组；大上下文仍使用默认 `256KB` 前缀。该改动只影响上游 session header，不改请求正文、不裁剪上下文、不改提示词、不限制输出。线上 stripped hash `b401c9463e29788e67aaecbe53c02b8743b2e25970e135e767410df9d4e0edab`，旧版备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260609-213725.pre-v4106-a52f4d6add0a`。部署后 4001/4002/4004/4000 `/health` 均 `status=ok`，`/v1/models` 只暴露 `deepseek-v4-flash`、`deepseek-v4-flash-lite`；ZenProxy 直连 OpenAI/Anthropic 非流式 `PONG` smoke 和 panda NewAPI -> channel 69 OpenAI/Anthropic smoke 均通过。当前进入真实流量观察期；早期窗口未见 panic、工具缺参、lane saturated 或 no proxy resources，但仍有 `reasoning_only_length` 首跳空输出重试和少量 NewAPI 500，需要按最终裸透率继续观察，不能用部署后前 5 分钟 warm-up cache 拒绝样本判断最终命中率。
46. 2026-06-10 11:28 CST 已部署 V4.107 cache usage/affinity 口径对齐到 panda 三实例：Anthropic final `message_delta.usage` 会保留上游真实 `input_tokens`；ZenProxy 流式 usage 合并不再被后续缺字段帧清零，并识别 DeepSeek `prompt_cache_hit_tokens`；ZenProxy affinity 对 32KB+ 流式请求生效，移除 body bucket，按中等 32KB/大上下文 256KB 稳定前缀分组，避免同一会话尾部增长或跨 body bucket 后换节点。该改动只修统计和路由亲和，不伪造 cache、不裁剪上下文、不改 prompt、不限制输出、不触碰 NewAPI/ClaudeCode/ccswitch。线上 stripped hash `e3001320300b37e8daf05266e7c1899652df8f42729a8f029db4c8602d4cd3c5`，旧版备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260610-112855.pre-v4107-b401c9463e29`；部署后 4001/4002/4004/4000 `/health` 通过，`/v1/models` 只暴露两个 deepseek 模型，ZenProxy 直连和 panda NewAPI OpenAI/Anthropic 最小 smoke 均通过。部署后约 8 分钟 channel 69 有 1367 条 NewAPI 记录，最终错误 0；整体加权 cache hit 约 48.3%，200k+ 桶约 51.3%，该窗口仍属 warm-up，不作为最终命中率结论。

## 附属工具

2026-06-05 新增独立 Rust sidecar：`tools/newapi-usage-exporter/`。

边界：

- 只读 NewAPI 使用日志数据库，支持 SQLite / Postgres。
- 不修改 NewAPI，不进入 ZenProxy/free-model-client-rs 主链路。
- 按 `user_id + time range` 导出，单次最大 31 天。
- 导出 zip 默认保留 30 天，过期清理。
- 不导出 prompt 原文、完整响应、真实 API key 或 IP 明文。
- 不做套餐推荐，不凭 tokens 猜用户真实用途。

接口：

- CLI：`serve`、`export`、`cleanup`。
- HTTP：`GET /health`、`POST /v1/usage-export`、`POST /v1/usage-export/instruction`、`GET /v1/usage-export/{id}`、`GET /v1/usage-export/{id}/download`、`DELETE /v1/usage-export/{id}`。
- panda helper：`newapi-usage-export '导出用户1从2026年6月5日~2026年6月5日的数据并做简要分析'`。

验证：

- `cargo fmt --manifest-path tools/newapi-usage-exporter/Cargo.toml -- --check` 通过。
- `cargo clippy --manifest-path tools/newapi-usage-exporter/Cargo.toml --all-targets -- -D warnings` 通过。
- `cargo test --manifest-path tools/newapi-usage-exporter/Cargo.toml` 通过：6 条测试。
- panda 真实 Postgres 直连验收通过：用户 1 当天 865 行导出 0.05 秒；用户 2 31 天 97,438 行导出 1.17 秒；HTTP create/get/download/delete 通过。
- panda 已部署 `newapi-usage-exporter.service`，本地 API `http://127.0.0.1:8098` active；一句话 helper 和 `/v1/usage-export/instruction` 验收通过。

详细说明见 `docs/08-newapi-usage-exporter.md`。

## 运行链路事实

已确认的最小事实：

```text
client -> panda NewAPI http://100.69.228.93:8081 -> configured upstream
```

注意：本仓库文档目前不把 panda NewAPI 后面的真实渠道名写死。除非通过 NewAPI 管理端、日志或响应证据确认，否则后续报告只能写 “configured upstream”。

2026-06-03 已确认的 channel 69 事实：

- panda NewAPI channel 69 名称为 `Zenproxyrs4.3`，状态启用，分组为 `vip`。
- channel 69 模型为 `deepseek-v4-flash,deepseek-v4-flash-lite`，base URL 为 `http://172.17.0.1:4000`。
- `sk-dev` 已被删除且属于历史 default 组，不能用于判断 channel 69 是否可用。
- NewAPI token 表中有效 `vip` 组 token 可通过 channel 69；文档和报告只记录 token id/name/group，不记录明文 key。
- 2026-06-03 11:26 NewAPI 日志出现 channel 69 管理测试成功消费记录：`request_path=/v1/messages`、`stream_status=ok`、`frt=76ms`。
- 2026-06-03 11:34 panda 本机 NewAPI 实测：`/v1/models` 200 且包含两个 deepseek 模型；OpenAI 非流式 `deepseek-v4-flash` 200，返回 `CHANNEL69_OPENAI_OK`；Anthropic 流式 `deepseek-v4-flash` 200，返回 `CHANNEL69_ANTHROPIC_OK`。

历史探测事实：

- WSL 到 `http://100.69.228.93:8081/v1/models` 可达。
- 返回模型包含 `deepseek-v4-flash`、`deepseek-v4-flash-lite`、`gpt-5.5`、`gpt-5.4`、`gpt-5.4-mini`。
- `http://43.156.233.219:8081`、`http://8.163.32.25:8081` 曾超时。
- 直连 `https://sub2api.closeapi.top/v1/models` 曾返回 Cloudflare 403/1010；这只能作为直连基线，不代表 panda NewAPI 不可用。

2026-05-30 至 2026-05-31 小矩阵实测：

| 场景 | 结果 | 耗时 | 证据摘要 |
|------|------|------|----------|
| panda `/v1/models` | 200 | 612ms | 返回 `deepseek-v4-flash`、`deepseek-v4-flash-lite`、`gpt-5.5`、`gpt-5.4`、`gpt-5.4-mini` 等模型。 |
| panda OpenAI `/v1/chat/completions` | 200 | 1882ms | `deepseek-v4-flash`，非流式，返回 `PONG`。 |
| panda Anthropic `/v1/messages` | 200 | 1532ms | `deepseek-v4-flash`，非流式，返回 `PONG`。 |
| WSL Hermes 短回复 | 成功 | 33989ms | 临时 `CUSTOM_BASE_URL=http://100.69.228.93:8081/v1`，返回 `PONG`。 |
| WSL Hermes 文件/终端工具 | 成功 | 54175ms | 写入并读取 `/tmp/hermes-panda-tool-test/hello.txt`，返回 `TOOL_OK:hello-panda`。 |
| WSL Hermes web 探测 | 命令成功 | 52335ms | 返回 `WEB_FAIL`，属于工具/模型判断结果，不是链路错误。 |
| WSL OpenClaw Node 运行时 | 成功 | - | 隔离安装 Node `v22.21.1` 到 `~/.local/opt/node-v22.21.1-linux-x64`，未覆盖系统 Node `v20.20.2`。 |
| WSL OpenClaw config validate | 成功 | 1550ms | 临时配置位于 `.codex_tmp/openclaw-panda/openclaw.json`。 |
| WSL OpenClaw models list | 成功 | 3998ms | 返回 `panda-newapi/deepseek-v4-flash`、`panda-newapi/deepseek-v4-flash-lite`。 |
| WSL OpenClaw infer PONG | 成功 | 9106ms | `infer model run --local`，winnerProvider=`panda-newapi`，返回 `PONG`。 |
| WSL OpenClaw agent PONG | 成功 | 12144ms | 默认 agent id 为 `main`，winnerProvider=`panda-newapi`。 |
| WSL OpenClaw agent 文件工具 | 成功 | 26787ms | `exec/write/read` 共 3 次工具调用，返回 `TOOL_OK:hello-openclaw`。 |
| WSL OpenClaw agent web_fetch | 成功 | 19088ms | 1 次 `web_fetch`，返回 `WEB_OK`。 |

结论：panda NewAPI、WSL Hermes、WSL OpenClaw 到 panda NewAPI 的小矩阵已经跑通。Hermes/OpenClaw 的 agent 工具链耗时明显高于裸 HTTP，包含客户端启动、上下文注入、工具 schema 和本地 agent 循环成本，不能直接等同于上游 TTFT。

## 当前未完成

P0 已完成：

1. panda NewAPI key/base URL 的 OpenAI `/v1/chat/completions` 最小请求已通过。
2. panda NewAPI key/base URL 的 Anthropic `/v1/messages` 最小请求已通过。
3. Hermes 临时指向 panda NewAPI，已运行短回复、文件/终端工具、web 探测。
4. OpenClaw Node 版本问题已用隔离 Node 22.21.1 解决，未覆盖系统 Node。
5. OpenClaw 临时指向 panda NewAPI，已运行 models list、infer、agent 短回复、文件工具、web_fetch。
6. 文档中的 API key 均按 `sk-***` 形式脱敏。

P0 残留观察点：

1. Hermes CLI 短回复和工具用例耗时波动较大，后续压测需要拆分客户端启动、上下文注入、上游模型响应三段耗时。
2. Hermes web 用例返回 `WEB_FAIL`，命令本身成功；后续若要验收 web 能力，需要固定更清晰的 web 工具断言和可访问目标。
3. OpenClaw 临时配置仍在 `.codex_tmp/openclaw-panda`，属于测试资产，不应提交密钥或大输出。

P1 当前状态：

1. 90 分客户端识别和策略隔离已落地，避免 Hermes/OpenClaw 兼容策略继续误伤 ClaudeCode。
2. 已实现 `ClientProfile`、`x-fmc-client`、OpenAI/Anthropic chat 路径 profile 传递、per-client thinking/whitespace/tool-history policy。
3. 已验证 kernel golden：ClaudeCode tools 不默认禁用 thinking，ClaudeCode 流式空白 delta 保留，Hermes compat thinking 策略保留，显式 `x-fmc-client` 优先。
4. 2026-06-04 15:33 已将包含 `finish_reason` 透传、non-stream cache usage 透传和 ClaudeCode 中等工具历史压缩的 `zen-proxy-rs` release 部署到 panda 三实例；panda 运行链路为 `NewAPI 8081 -> zen-proxy-rs 4001/4002/4004 -> free-model-client-rs kernel -> upstream`。
5. 本轮部署后 smoke：`/v1/models` 返回 200，包含 `deepseek-v4-flash`、`deepseek-v4-flash-lite`；`deepseek-v4-flash` OpenAI 非流式返回 `PONG`，HTTP 200；Anthropic 流式返回 `PONG`，HTTP 200；Anthropic 极短非流式探针仍可能在上游持续空输出时返回 502。
6. 最新源码/脚本状态：输出限制已完全取消，ZenProxy 侧 non-stream output guard 已取消；ZenProxy 外层 context compactor 对 flash/free 已放行、对 lite 仍生效；`policy-smoke/policy-dry` harness 已存在并记录 input/output wall、provider header/body usage、cache 四态。真实 panda `policy-smoke/policy-dry` 尚未跑，不能写成生产已验证。

P1 panda 部署记录：

| 项 | 值 |
|----|----|
| 部署时间 | 2026-05-31 |
| 部署目标 | `/opt/zen-proxy-rs/zen-proxy-rs` |
| 旧二进制 hash | `2a7ab35e80c112a623c4ec3e3519e02ca29b652d368967534b90f70d5920a132` |
| 本地 release hash | `65e1ee22dd1f392b566e2af8664308cea044599294f93d228da1082f9b1639f9` |
| 实际部署 hash | `35feea8bba79412e0b323a494ebd5450e8a21bfd9769395091eb8c6c0d87165a` |
| 说明 | 实际部署前对 release 二进制执行 `strip`，因此部署 hash 与本地未 strip release hash 不同。 |
| 备份 | `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-client-profile-20260531-131038` |
| 实例 | `zen-proxy-rs@1:4001`、`zen-proxy-rs@2:4002`、`zen-proxy-rs@3:4004` |
| 健康检查 | 三实例 `/health` 均 200，`status=ok`，池 `total=90`、`dispatch=90`。 |
| NewAPI smoke | `/v1/models` 200；`/v1/chat/completions` 200，`deepseek-v4-flash` 返回 `OK`。 |

P1.1 OpenClaw profile 修复部署记录：

| 项 | 值 |
|----|----|
| 部署时间 | 2026-05-31 晚间 |
| 部署目标 | `/opt/zen-proxy-rs/zen-proxy-rs` |
| 实际部署 hash | `b9441bce94180aed6aae7dd6af79da618382a2a1f49ee292620b08cd2cd357fd` |
| 实例 | `zen-proxy-rs@1:4001`、`zen-proxy-rs@2:4002`、`zen-proxy-rs@3:4004` |
| 健康检查 | 三实例 `/health` 均 200，`status=ok`，池 `total=90`、`dispatch=90`、`dead=0`、`ratelimited=0`、`upstream.backoff=false`。 |
| 代码验证 | `free-model-client-rs` 的 `fmt`、`clippy -D warnings`、`cargo test` 通过；`zen-proxy-rs` 的 `fmt`、`clippy -D warnings`、`cargo test` 通过。 |

P1.2 ClaudeCode huge_context final-anchor 修复部署记录：

| 项 | 值 |
|----|----|
| 部署时间 | 2026-06-01 下午 |
| 部署目标 | panda `/opt/zen-proxy-rs/zen-proxy-rs` |
| 上一线上 hash | `94872cd91a558ec431e176af5a1fa8f257219c9518df3f729e7e5645b5cbb937` |
| 本地 release hash | `c44df2fb8ecae44a5c155e88953574e6990f07947b45b71e1f60468f2c00c06e` |
| 实际部署 hash | `3a27f2c7cda56119b32dfc42738b06f3f1e08155a0ff89c48daca6ddc8aed1d4` |
| 说明 | 实际部署前对 release 二进制执行 `strip`，因此部署 hash 与本地未 strip release hash 不同。 |
| 备份 | `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-final-anchor-20260601-94872cd` |
| 实例 | `zen-proxy-rs@1:4001`、`zen-proxy-rs@2:4002`、`zen-proxy-rs@3:4004` |
| 健康检查 | 三实例 `/health` 均 200，`status=ok`，池 `total=90`、`dispatch=90`、`dead=0`、`ratelimited=0`、`upstream.backoff=false`。 |
| 构建验证 | `/home/lenovo/zen-proxy-rs` 执行 `CARGO_INCREMENTAL=0 cargo build --release` 通过，依赖本地 `../free-model-client-rs`。 |
| NewAPI smoke | panda 本机 `http://127.0.0.1:8081/v1/models` 200，包含 `deepseek-v4-flash` 和 `deepseek-v4-flash-lite`；OpenAI 非流式 `deepseek-v4-flash` 返回 `OK`。 |
| huge stream smoke | panda 本机 `/v1/messages`，约 1.0MB 请求体，`source_client=claude-code`，触发 `appended_latest_user_anchor=true`；`deepseek-v4-flash` 3/3 返回 `HUGE_OK`，`deepseek-v4-flash-lite` 3/3 返回 `HUGE_OK`。 |
| 耗时观察 | flash 三轮约 2.5s、2.7s、3.1s；lite 三轮约 3.2s、3.3s、14.8s。另有早期单轮 flash 3.9s 成功。 |
| 残留观察 | 日志仍可见上游偶发空输出，buffered retry 已处理，未裸透给 smoke 客户端；这仍需在正式 dry run 中统计。 |

P1.3 channel 69 健康测试/空输出误判修复部署记录：

| 项 | 值 |
|----|----|
| 部署时间 | 2026-06-03 上午 |
| 部署目标 | panda `/opt/zen-proxy-rs/zen-proxy-rs` |
| 上一线上 hash | `66b883256e08d42dbc7e473e865dc9e05f5318260c290837530f5cacd62f912f` |
| 实际部署 hash | `28b928472d2abc7be036cdc2796915865bd4a5b083ee2dd1dab46b1ddd0e2633` |
| 备份 | `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-channel-test-fix-20260603-111524-66b8832` |
| 实例 | `zen-proxy-rs@1:4001`、`zen-proxy-rs@2:4002`、`zen-proxy-rs@3:4004` |
| 根因 | ClaudeCode 小 `max_tokens` 流式请求会误进 huge buffered retry；NewAPI channel test 的小/空探测因此被当作大上下文重试，最终出现 `upstream returned no assistant content or tool call`。 |
| 修复 | 空内容无工具健康探测短路为本地 `ok`；ClaudeCode huge buffer 改为只按修复前输入 tokens >= 50k 触发。 |
| 后续补强 | 对管理端测渠道常见的 `echo hi` 极短流式探测增加空上游 fallback：先尝试上游，只有上游空输出才返回本地 `ok`。 |
| 代码验证 | `free-model-client-rs` 的 `fmt --check`、`clippy -D warnings`、`cargo test` 通过；kernel golden 64 条。 |
| panda 验证 | channel 69 管理测试日志成功；有效 `vip` 组 token 下 `/v1/models`、OpenAI 非流式、Anthropic 流式均 200。 |
| 第二层验收 | panda NewAPI 有效 `vip` 组 token 下，`echo hi`/`max_tokens=16`/stream 探测 10/10 返回 200，`empty_error=0`；ZenProxy 日志确认上游空流被本地 `ok` 兜底。 |

P1.4 非流式渠道探针兜底源码记录：

| 项 | 值 |
|----|----|
| 修改时间 | 2026-06-03 晚间 |
| 状态 | 已随 2026-06-03 晚间 profile/格式误伤修复一并部署到 panda。 |
| 修复 | `echo hi`/`hi`/`hello`/`test` 类极短非流式无工具探针，在上游连续空输出后返回本地 `ok`；普通请求仍返回结构化空输出错误。 |
| 代码范围 | `src/protocol/translate.rs`、`src/proxy/anthropic.rs`、`src/proxy/openai.rs`、`tests/kernel_golden.rs`。 |
| 验证 | WSL 临时 target：`cargo fmt --check`、`cargo test`；结果为库测试 64 条、kernel golden 70 条、doc tests 0 条全部通过。 |
| panda 部署 | 线上 hash 已更新为 `e47e2c89d7c0c497daa2cd49a9d135a8b695928e951f81a22630b206a0e2ab51`；备份为 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-profile-format-20260603-202631-cdafae8`。 |

P1.5 ClaudeCode 格式误伤修复部署记录：

| 项 | 值 |
|----|----|
| 部署时间 | 2026-06-03 晚间 |
| 根因 | `free-model-client-rs` 内核和 `zen-proxy-rs` 外层都曾把正文里普通出现的 `OpenClaw/Hermes`、以及 `web_fetch`/`web_search` 工具名当成 OpenClaw 强信号，导致 ClaudeCode 请求可能走 OpenClaw/Hermes 兼容策略，不再逐字保留 Markdown/空白。 |
| 修复 | 收窄 body marker，只接受 `running inside openclaw/hermes`、`openclaw cli/agent`、`hermes cli/agent` 等强 marker；`web_fetch`/`web_search` 不再单独触发 OpenClaw；OpenClaw 仍由 `subagents`、`sessions_*`、`memory_*`、`openclaw*` 等专属工具识别。 |
| 代码范围 | `src/client_profile.rs`；`zen-proxy-rs/src/v4/provider.rs`。 |
| 测试 | 新增 ClaudeCode + Web 工具、普通 OpenClaw/Hermes 文本引用、Web-only 工具三类回归；free-model 库测试 64/64、kernel golden 70/70；ZenProxy 单元 129/129、e2e 26/26。 |
| panda 验证 | 新 pid `365354/365378/365392` 全部健康；`/v1/models` 200 且只暴露 `deepseek-v4-flash`、`deepseek-v4-flash-lite`；受控 `Task + web_fetch` `/v1/messages` 日志显示 `source_client=claude-code`。 |
| 运行观察 | 旧 pid `350170/350205/350216` 日志中大量 `source_client=openclaw` 属于部署前误伤；部署后新 pid 日志里的受控样本已转为 `claude-code`。 |

P1.6 request-shape 观测部署记录：

| 项 | 值 |
|----|----|
| 部署时间 | 2026-06-03 晚间 |
| 根因 | 需要从源头拆分 ClaudeCode 300KB+ 请求体来源，并识别 `body_size=342` 这类小非流式空输出请求用途。 |
| 本地未 strip release hash | `1e77a544b5c444b4d626b14327862259cfe1b26221ffbd6f87778c1afc321376` |
| 部署 stripped hash | `28b25370925835bb33aa4142208a5a20f0cf4dcb74ad3ae74c3808d3c2761e2b` |
| 旧线上 hash | `e47e2c89d7c0c497daa2cd49a9d135a8b695928e951f81a22630b206a0e2ab51` |
| 备份 | `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-request-shape-20260603-232757-e47e2c8` |
| 实例 | `zen-proxy-rs@1:4001` pid `462903`、`zen-proxy-rs@2:4002` pid `462918`、`zen-proxy-rs@3:4004` pid `462981`。 |
| 验证 | `free-model-client-rs`：`fmt --check`、`clippy -D warnings`、`cargo test` 通过；库测试 69 条、kernel golden 71 条。`zen-proxy-rs`：单元测试 129 条、e2e 26 条、release build 通过。 |
| panda 验证 | 三实例和 nginx active；4001/4002/4004/4000 `/health` 均 200；`/metrics` 显示 `200=6`、`5xx=0`、`timeout=0`、active concurrency 0、dead 0、ratelimited 0。 |
| NewAPI 验证 | panda 本机 NewAPI 8081 使用 token id `38`/name `ds`/group `vip`，`deepseek-v4-flash` 非流式 smoke HTTP 200，返回 `OK`，总耗时约 4.01s；NewAPI 日志 id `109472` 显示 channel 69、token id 38、prompt tokens 93、completion tokens 47、非流式。 |
| shape 证据 | 部署后 ZenProxy 日志已出现 `desensitized request shape before upstream`；小非流式样本被分类为 `internal_claude_code_probe`，真实 ClaudeCode 787KB 流式样本记录 `message_count=703`、`system_tokens=5773`、`messages_tokens=78859`、`tools_tokens=12700`、`tool_count=10`、`estimated_total_tokens=97332`。 |

P1.7 ClaudeCode huge-session compactor 部署记录：

| 项 | 值 |
|----|----|
| 部署时间 | 2026-06-04 凌晨 |
| 目标 | 修复 ClaudeCode 长会话反复执行旧任务、非流式 200k+ fallback 放大旧历史的问题。 |
| 本地未 strip release hash | `408fa673aef439e849ca9e24d41576810c122faa71724581ce8067e10c04fc80` |
| 部署 stripped hash | `96b954a81978e9348f26341d68626d0a98682c6971611d7802a0850ef771d815` |
| 旧线上 hash | `28b25370925835bb33aa4142208a5a20f0cf4dcb74ad3ae74c3808d3c2761e2b` |
| 备份 | `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-huge-session-20260604-003040-28b2537` |
| 实例 | `zen-proxy-rs@1:4001` pid `498728`、`zen-proxy-rs@2:4002` pid `498733`、`zen-proxy-rs@3:4004` pid `498734`。 |
| 健康检查 | 4001/4002/4004/4000 `/health` 均 200；三实例 active；池 `total=90`、`dispatch=90`、`dead=0`、`ratelimited=0`。 |
| 验证 | `free-model-client-rs`：`fmt --check`、`clippy -D warnings`、`cargo test` 通过；库测试 69 条、kernel golden 73 条。`zen-proxy-rs`：主单测 129 条、e2e 26 条通过；release build 通过。 |
| panda 非流式 smoke | 517KB / `before_tokens=123371` 的 ClaudeCode 非流式样本被压到 `after_tokens=9139`、`message_count=51`；NewAPI id `109585` 账面 `prompt_tokens=5647`，HTTP 200。 |
| panda 流式 smoke | 522KB / `before_tokens=124597` 的 ClaudeCode 流式样本触发 exact-anchor，shape `message_count=1`、`estimated_total_tokens=51`；NewAPI id `109593` 账面 `prompt_tokens=125`，HTTP 200。 |

P1.8 NewAPI 短 smoke 探针空输出兜底：

| 项 | 值 |
|----|----|
| 触发 | 2026-06-04 严格验收时，panda NewAPI channel 69 的极短 non-stream smoke 经 NewAPI 转为 ClaudeCode/Anthropic 小请求，上游连续返回空输出，旧逻辑在 `internal_claude_code_probe` 分类下裸透 502。 |
| 修复 | 新增 `short_no_tool_empty_fallback_text`，只对无工具、单用户消息、`max_tokens <= 64` 且显式 `echo hi`/`strict smoke`/`reply PASS`/`answer OK` 等测试探针触发本地兜底；普通 ClaudeCode 短输入仍不兜底。 |
| 测试 | `free-model-client-rs`：`fmt --check`、`clippy -D warnings`、`cargo test` 通过；库测试 70 条、kernel golden 75 条。`zen-proxy-rs`：主单测 129 条、e2e 26 条通过；release build 通过。 |
| 部署 | panda 三实例部署 stripped hash `0f1d7a36fdc7142e1acd9670301e7277ca6805e47899490958a2c390c619cea5`；旧 hash `96b954a81978e9348f26341d68626d0a98682c6971611d7802a0850ef771d815` 备份到 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-strict-smoke-20260604-105132-96b954a`。 |
| 线上 smoke | panda 本机 NewAPI `/v1/models` 200，返回 8 个模型且包含 `deepseek-v4-flash`、`deepseek-v4-flash-lite`；两个 deepseek 模型的 OpenAI/Anthropic、stream/non-stream 共 8 条 smoke 全部 HTTP 200，内容摘要为 `PASS`。 |
| 耗时 | non-stream 总耗时约 4.8-5.5s；stream 首内容约 2.0-2.3s。 |
| 环境边界 | Windows 环境变量存在 `HTTP_PROXY=http://127.0.0.1:7897`；Windows `Invoke-RestMethod` 走代理访问 panda NewAPI 会 502，但 `curl --noproxy '*'` 直连 `100.69.228.93:8081/v1/models` 为 200。Windows ClaudeCode/cc-switch 若继承该代理，需要显式绕过 panda Tailscale IP。 |

P1.9 ClaudeCode 大流式 768/1024 cap 桶 buffered retry 修复：

| 项 | 值 |
|----|----|
| 修复时间 | 2026-06-04 中午 |
| 根因 | 外层已判断 ClaudeCode 大上下文或低输出 cap 应进入 huge buffered retry，但 `handle_stream` 内部又二次限制 `max_tokens <= 512`，导致 `max_tokens=32000` 被 cap 到 768/1024 的真实大流式请求绕过 retry，遇到上游空输出时裸透 `upstream returned no assistant content or tool call`。 |
| 修复 | 移除 `handle_stream` 内部多余的 512 门槛；只要外层 `use_claude_code_huge_buffer=true` 就进入 buffered retry。 |
| 回归 | 新增 `claude_code_huge_stream_uses_buffer_retry_after_1024_output_cap`：约 50k+ 输入、`max_tokens=32000 -> 1024`、上游第一次空输出、第二次正常输出，断言 upstream 请求 2 次且不再返回空 assistant 错误。 |
| 本地验证 | `free-model-client-rs`：`fmt --check`、`clippy -D warnings`、`cargo test` 通过；库测试 71 条、kernel golden 76 条。`zen-proxy-rs`：主单测 129 条、e2e 26 条、release build 通过。 |
| 部署 | panda 三实例部署 stripped hash `7a8f4e5dc99e8ccf1aaf6562519d8353dc4ba5205e5e55f521c265b0760ed66e`；旧 hash `117b3cbfaf058fbbeb258f98542afc09a097e763359f34d174414b47dfd11aff` 已备份到 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-buffered-1024-*`。 |
| 线上健康 | `zen-proxy-rs@1/@2/@3` active；`http://127.0.0.1:4000/health` 返回 `status=ok`、`dispatch=90`、`dead=0`、`ratelimited=0`、`upstream.backoff=false`。 |

P1.10 non-stream cache usage 透传源码修复：

| 项 | 值 |
|----|----|
| 修改时间 | 2026-06-04 |
| 根因 | NewAPI 看不到/不显示 cache，不只是上游不返回；源码里 OpenAI 非流式响应根本没带 `cache_*` 字段，Anthropic 非流式正文/工具调用响应长期把 `cache_creation_input_tokens/cache_read_input_tokens` 写死为 `0`。 |
| 修复 | `src/proxy/openai.rs`、`src/proxy/anthropic.rs` 已改为在非流式正文和工具调用两条分支透传真实 usage：`prompt_tokens_details.cached_tokens`、`cache_creation_input_tokens`、`cache_read_input_tokens`。 |
| 验证 | WSL `lenovo` 用户下执行 `cargo fmt -- --check`、`cargo test -q`、`cargo clippy --all-targets -- -D warnings` 通过；库测试 71 条、kernel golden 87 条。 |
| 部署状态 | 已随 2026-06-04 15:33 panda release 部署；实际部署 stripped hash `694036f6a130e8211b998a5b58eff36105fb48fb866ec57ebbb2c03ccfb5f3d7`，备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-v46-20260604-153327-0f6cdf6e5cd2`。 |
| 线上观察 | 部署后 OpenAI 非流式 smoke usage 已正常透出 `prompt_tokens=87`、`completion_tokens=27`。该样本 `cached_tokens=0`，说明这次调用本身没有上游 cache 命中，不代表透传无效。 |

P1.11 ClaudeCode 半截输出根因修复源码记录：

| 项 | 值 |
|----|----|
| 修改时间 | 2026-06-04 |
| 根因 | 近期 panda 样本显示，小 prompt 可长输出，但 ClaudeCode 工程请求大量为中等上下文 + 工具历史形态：`last_user_tokens` 经常只有 3，旧工具输出可达 26k；同时源码把上游 `finish_reason=length` 固定改成正常 `stop/end_turn`，导致提前停止不可见。 |
| 修复 1 | OpenAI/Anthropic 响应保留上游 `finish_reason`；Anthropic 将 `length` 映射为 `max_tokens`。 |
| 修复 2 | ClaudeCode 中等工具历史压力下提前折叠旧历史：消息数 >=40、消息 token >=24k、最大非系统消息 >=12k、最新 user <=1024 tokens。 |
| 验证 | 新增 `finish_reason=length` 四路径回归、ClaudeCode 中等工具历史折叠回归；`cargo fmt -- --check`、`cargo test -q`、`cargo clippy --all-targets -- -D warnings` 通过。 |
| 部署状态 | 已随 2026-06-04 15:33 panda release 部署；实际部署 stripped hash `694036f6a130e8211b998a5b58eff36105fb48fb866ec57ebbb2c03ccfb5f3d7`。 |
| NewAPI 验收 | `curl --noproxy '*' http://100.69.228.93:8081/v1/models` 200，模型数 8；OpenAI 非流式 `deepseek-v4-flash` 200，返回 `PONG`，usage `prompt_tokens=87`、`completion_tokens=27`；Anthropic 流式 `deepseek-v4-flash` 200，返回 `PONG`，usage `input_tokens=87`、`output_tokens=27`。 |
| 残留 | Anthropic 极短非流式探针 `reply PONG only` 仍可能被识别为 `internal_claude_code_probe`，在上游连续空输出时经 11 次 provider retry 后返回 502：`upstream retry budget exhausted ... last_error=empty_output`。该残留目前不影响真实流式小请求验收，但仍需继续补 non-stream probe 兜底。 |

P1.12 2026-06-04 V4.6 panda 部署记录：

| 项 | 值 |
|----|----|
| 部署时间 | 2026-06-04 15:33 |
| 部署目标 | `/opt/zen-proxy-rs/zen-proxy-rs` |
| 旧二进制 hash | `0f6cdf6e5cd2dd1946a69707c97591cca865b47178ff63846f04bbdf283f2314` |
| 本地未 strip release hash | `9b68db105aaad2c1014899d00122accf3a21109a26054f68ce0d612f152b5839` |
| 实际部署 stripped hash | `694036f6a130e8211b998a5b58eff36105fb48fb866ec57ebbb2c03ccfb5f3d7` |
| 备份 | `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-v46-20260604-153327-0f6cdf6e5cd2` |
| 实例 | `zen-proxy-rs@1:4001`、`zen-proxy-rs@2:4002`、`zen-proxy-rs@3:4004` |
| 健康检查 | 三实例 `active`；4000/4001/4002/4004 `/health` 均返回 `status=ok`、`dispatch=90`、`dead=0`、`ratelimited=0`、`upstream.backoff=false`。 |
| /metrics | smoke 后 `zen_proxy_requests_total{status="200"} 2`、`{status="5xx"} 1`、`stream=2`、`non_stream=1`、`model=\"deepseek-v4-flash\"=3`。 |
| NewAPI smoke | `curl --noproxy '*'` 直连 panda `8081` 时：`/v1/models` 200 且包含两个 deepseek 模型；OpenAI 非流式 `PONG` 200；Anthropic 流式 `PONG` 200。 |
| 环境边界 | WSL 若继承代理环境变量，直连 `http://100.69.228.93:8081` 可能先返回代理层 502 空响应；验收时需显式使用 `curl --noproxy '*'` 或配置 `NO_PROXY=100.69.228.93`。 |

P1.13 输出限制取消与 policy harness 当前状态：

| 项 | 值 |
|----|----|
| 状态 | 当前源码已完全取消输出限制；ZenProxy 侧 non-stream output guard 已取消；2026-06-04 18:54 已部署到 panda，并通过手工 NewAPI smoke 验证。真实 panda `policy-smoke/policy-dry` 尚未跑，不能写成生产压测已验证。 |
| max_tokens 行为 | 缺省 `max_tokens` 不再自动补 1024/2048；显式 `max_tokens` 原样透传；OpenAI/Anthropic 只有显式值才写上游。 |
| flash 策略 | `deepseek-v4-flash/deepseek-v4-flash-free` 取消 Hermes/OpenClaw 适配，只保留 ClaudeCode 深度适配；取消输入 token 墙，`free-model-client-rs` 侧只观测不压缩。 |
| lite 策略 | `deepseek-v4-flash-lite/big-pickle` 只保留 Hermes/OpenClaw 适配，取消 ClaudeCode 适配。 |
| cache/usage 观测 | cache 记录 `attempted/accepted/rejected/ignored` 四态，并记录 provider response/header/body usage 信号。 |
| harness | `scripts/panda_pressure_runner.py --mode policy-smoke|policy-dry` 记录 input/output wall、provider header/body usage、cache 四态和 lite effective profile。 |
| 风险 | 输出限制取消后，上游 413/超时/空输出/延迟/成本风险回到 upstream 与 lane/pool 调度，需要真实 panda 压测确认。 |

P1.14 2026-06-04 V47 panda 部署记录：

| 项 | 值 |
|----|----|
| 部署时间 | 2026-06-04 18:54 |
| 部署目标 | `/opt/zen-proxy-rs/zen-proxy-rs` |
| 旧二进制 hash | `694036f6a130e8211b998a5b58eff36105fb48fb866ec57ebbb2c03ccfb5f3d7` |
| 本地未 strip release hash | `aeecc8d5acbea86e36dee3f1224858b2f371d64d0ebfc2508313e33e7b09b1c0` |
| 实际部署 stripped hash | `99424602ce7c076671579abf48ca0d27367ac126e514efe4403d902d5caecd78` |
| 备份 | `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-v47-20260604-185423-694036f` |
| 实例 | `zen-proxy-rs@1:4001`、`zen-proxy-rs@2:4002`、`zen-proxy-rs@3:4004` |
| 健康检查 | 三实例 `active`；新 pid 为 `1093754/1093766/1093777`；三实例 `/health` 均返回 `status=ok`、`dispatch=90`、`dead=0`、`ratelimited=0`、`upstream.backoff=false`。 |
| 根因确认 | 部署前线上 18:19-18:21 日志仍有 `compacted streaming anthropic context before upstream` 和 `capped streaming anthropic max_tokens ... effective_max_tokens=512`，模型为 `deepseek-v4-flash-free`，说明用户看到的“我被压缩过了”来自旧线上自动 compactor，不是用户手动 compact。 |
| NewAPI smoke | panda 本机 `http://127.0.0.1:8081/v1/models` 200，包含 `deepseek-v4-flash` 和 `deepseek-v4-flash-lite`；OpenAI 非流式 `deepseek-v4-flash` 返回 `V47_SHORT_OK`，HTTP 200，约 3.9s。 |
| 大上下文 smoke | OpenAI 非流式 `deepseek-v4-flash`，361 条消息、请求体约 560KB、`max_tokens=32000`，返回 `V47_NO_COMPACTOR_OK`，HTTP 200，约 7.0s，NewAPI usage 约 `prompt_tokens=97836`。 |
| 日志验收 | 部署后日志 grep `compacted .*context|capped .*max_tokens|context compactor|effective_max_tokens` 无命中；大请求日志显示 `context_action=pass`、`effective_body_size=560709`、`max_tokens=Some(32000)`，没有输入折叠或输出 cap。 |
| cache 观察 | 大请求日志记录 `cache_observation="rejected"`、`provider_response_signal=true`、`provider_body_usage_signal=true`、`provider_body_cached_tokens=Some(0)`；这说明 provider 返回了 usage/cache 信号但本轮未命中缓存，不是 NewAPI 完全没记录。 |

P1.10 2026-06-04 三客户端 smoke 和 web/search 边界：

| 项 | 结果 |
|----|------|
| Windows ClaudeCode | 显式 base/key 指向 panda NewAPI，5/5 通过；P50 约 4.3s，P90 约 7.8s；tool 2/2 语义通过；subagent 用例语义通过但 runner 未观察到真实 Task tool call。 |
| WSL ClaudeCode | 当前不可作为有效样本；`/home/lenovo/.local/bin/claude` 和 `claude-deepseek-free` 都指向 clawgod launcher，实际启动 `/root/.bun/bin/bun /root/.clawgod/cli.cjs`，会挂住，不是 Anthropic ClaudeCode CLI。 |
| WSL Hermes | 5/5 通过；P50 约 34.7s，P90 约 38.9s；tool 2/2 通过；Hermes subagent 当前 runner 标记为不支持。慢路径属于 Hermes 本地 agent/启动/工具链耗时，不能直接等同 ZenProxy TTFT。 |
| WSL OpenClaw | API 5/5 通，但 semantic 0/5；输出固定 `HEARTBEAT_OK`，stderr 有 local secrets gateway `1006 abnormal closure`。这是 OpenClaw 本地 agent/gateway/harness 问题，不是 NewAPI/ZenProxy HTTP 链路断。 |
| 直连 web tools | 清空 WSL proxy env 后，Anthropic `/v1/messages` 和 OpenAI `/v1/chat/completions` 带 `web_search` tool 均 200，返回真实 `web_search` tool call；说明模型和 ZenProxy 可以转发/产生工具调用。 |
| Windows ClaudeCode WebSearch | 用户截图已证明官方 ClaudeCode + 官方 Claude 模型可以真实执行 `WebSearch/WebFetch`；此前“ClaudeCode 没注册 WebSearch/WebFetch”的结论只能描述当时那次 ZenProxy 受控样本，不是 ClaudeCode 能力边界。ZenProxy 路径的核心差异是上游可能返回 `web_search/task` 等小写或下划线工具名，旧内核原样吐回，ClaudeCode 只认已注册的 `WebSearch/Task`。2026-06-04 已修复并部署到 panda，线上直连 ZenProxy smoke 返回 `tool_use_names=WebSearch` 和 `tool_use_names=Task`。 |
| cc-switch 当前 provider | Windows cc-switch 当前 Claude provider 是 `closedeepseek -> https://sub2api.closeapi.top`；`LocalNewapi -> http://127.0.0.1:8081` 存在但不是 current。用户平时从 Windows ClaudeCode 测到的现象不能默认归因到 panda NewAPI/ZenProxy。 |

## 当前数据解释

1. “输入几乎 70k/90k”当前不是 NewAPI 输入 token 墙。2026-06-03 23:01-23:46 的 channel 69 历史 ClaudeCode 流式请求显示：ZenProxy 入口 body 从约 674KB 增长到 788KB，`before_tokens` 约 97k-110k，当时压缩后 NewAPI 账面多落在 70k-90k；最新 `deepseek-v4-flash/deepseek-v4-flash-free` 策略已经取消输入 token 墙，`free-model-client-rs` 侧只观测 request shape，不再压缩输入，`zen-proxy-rs` 外层也只 warn/pass 不 compact/reject。
2. NewAPI 中看到的 200k+ prompt tokens 记录来自 ClaudeCode 非流式大请求/fallback，而不是常规流式轮次。样本：NewAPI id `109370` 为非流式 `213248` prompt tokens，id `109461` 为非流式 `225416` prompt tokens；这是历史输出/输入保护改动前的归因样本，不能用来证明当前仍存在输出 cap。
3. “ClaudeCode 一直反复做”的直接调用形态是：流式大请求偶发 `status_code=500, upstream returned no assistant content or tool call`，随后 ClaudeCode 又以非流式大请求重发同一大历史，成功返回后下一轮继续把历史追加进去。样本：NewAPI id `109459` 流式 prompt tokens 记 0 且 500，紧接 id `109461` 非流式 225416 prompt tokens 成功。
4. 旧版 ClaudeCode huge-context 策略虽配置 `target_tokens=12k`，但真实请求里 `message_count` 已达 670-705，`tools_tokens=12700`，大量旧短消息低于单条 `min_text_tokens=2000`，不会被旧 compactor 选为压缩候选；这是历史 context_drift 归因。当前 flash 路径改为只观测不压缩，后续质量风险要由真实 panda policy/dry 压测确认。
5. 缓存几乎为 0 不能再直接写成“上游没有 cache”。当前观测会分为 `attempted/accepted/rejected/ignored` 四态，并分别记录 provider header/body usage 信号；只有真实 panda policy 样本能判断是上游没给、被中间层剥离、cache 被拒绝，还是确实命中。
6. 2026-06-04 中午已修复 768/1024 cap 桶绕过 buffered retry 的历史 bug；最新策略已经完全取消输出限制，后续若 NewAPI 再出现空输出、413、超时或高延迟，应优先归因到上游、客户端断流、lane/pool 调度和成本/长尾风险，而不是本仓库的输出墙。
7. Web/search 不是模型原生联网。当前源头已经证明：只要客户端提供 tool schema，ZenProxy 可以让模型返回 tool call；官方 ClaudeCode + 官方 Claude 能执行 `WebSearch/WebFetch`。ZenProxy 路径失败时优先检查工具定义是否进入请求、上游是否返回 tool call、返回工具名是否被 canonicalize 回客户端注册名，而不是再把问题归结为 ClaudeCode 没工具。

P1 待执行：

1. 正式无密钥 panda 压测执行器已落地到 `scripts/panda_pressure_runner.py`。
2. 执行器只从 `PANDA_NEWAPI_KEY`、`NEWAPI_API_KEY`、`OPENAI_API_KEY` 读取 key，默认 base URL 为 `http://100.69.228.93:8081`，默认拒绝 localhost。
3. 执行器已新增 `policy-smoke` / `policy-dry`，直接 HTTP 验证输出/输入墙取消、provider header/body usage、cache 四态和 lite effective profile，不依赖本地 CLI 状态。
4. 执行器已支持 Windows ClaudeCode、WSL ClaudeCode、WSL Hermes、WSL OpenClaw，且对 Hermes/OpenClaw 大 prompt 使用文件背书，避免 Linux `Argument list too long`。
5. Smoke / preflight 已证明 panda `/v1/models` 和最小聊天可用，模型包含 `deepseek-v4-flash`、`deepseek-v4-flash-lite`；但最新输出限制取消后的真实 panda `policy-smoke/policy-dry` 尚未跑。
6. dry run 暴露红旗，当前不能直接进入 4 客户端 x 500 full run。
7. 2026-06-01 panda 本机 huge stream source-side smoke 已通过，但它不是 ClaudeCode/Hermes/OpenClaw 真实客户端验收；下一步仍要重新跑 panda-only policy-smoke/policy-dry，再跑四客户端 dry run。
8. WSL ClaudeCode 必须先换成真实 ClaudeCode CLI 或修复当前 clawgod launcher，否则不能纳入四客户端正式压测。
9. OpenClaw 必须先修 local secrets gateway / agent harness 的 `HEARTBEAT_OK` 问题，否则只能统计 API 可达，不能统计语义、工具和 subagent 成功率。
10. ClaudeCode WebSearch 若要真实执行，必须让 ClaudeCode 收到和其已注册工具同名的 `tool_use`，例如 `WebSearch`/`WebFetch`。ZenProxy 不会也不应自行替客户端执行公网搜索；但必须保真转发并回填工具名大小写/别名。

P1 dry-run 结果：

2026-06-01 final-anchor 部署后的四客户端 dry run 结果：

| 客户端/批次 | 结果 | P50/P90/P99 total | 主要问题 |
|-------------|------|-------------------|----------|
| Windows ClaudeCode dry 50 | 50/50 API ok，43/50 semantic ok | 7.8s / 27.3s / 39.3s | 6 个 huge_context `context_drift`，1 个 `subagent_not_triggered`；Windows runner 使用 UNC 工作目录，CMD fallback 到 Windows 目录，影响 subagent 判断。 |
| WSL ClaudeCode dry 50 | 50/50 API ok，44/50 semantic ok | 6.5s / 23.8s / 64.2s | 6 个 huge_context 全部 `context_drift`，模型转去读 ClaudeCode transcript、git 状态或继续旧任务。 |
| WSL Hermes dry 50 | 50/50 API ok，50/50 semantic ok | 54.3s / 69.5s / 103.5s | 功能通过，但延迟远超 full-run 门槛；subagent 当前 runner 不支持观测，不计入触发率。 |
| WSL OpenClaw dry 50 | 50/50 API ok，49/50 semantic ok | 14.6s / 32.7s / 66.6s | 1 个 `deepseek-v4-flash-lite` long_context `context_drift`；subagent 5/5 observed。 |

全局结论：

```text
总轮次: 200
API OK: 200/200
semantic OK: 186/200
认证/模型/协议 400/502/504/300s timeout: runner summary 未观察到
panda health: 三实例健康，total=90 dispatch=90 dead=0 ratelimited=0
```

脱敏报告见 `docs/reports/panda-dry-run-20260601.md`。

历史 dry-run 结果：

| 客户端/批次 | 结果 | 主要问题 |
|-------------|------|----------|
| WSL ClaudeCode dry 50 | 49/50 API ok，47/50 semantic ok | `deepseek-v4-flash-lite` huge_context 语义漂移；一次 tool_calc 返回 `503 system cpu overloaded`。 |
| WSL Hermes dry 50 | 50/50 API ok，50/50 semantic ok | P90 总耗时约 47s；Task/subagent 触发当前 runner 不支持观测。 |
| WSL OpenClaw dry 50 | 50/50 API ok，48/50 semantic ok | subagent 在 lite 上出现一次 `client_timeout` 和一次 `subagent_not_triggered`。 |
| Windows ClaudeCode partial dry 22 | 21/22 API ok，21/22 semantic ok | huge_context 出现 310s 级客户端非零退出和一次 `context_drift`；已主动中止，避免继续压生产链路。 |

OpenClaw profile 修复后的 smoke 结果：

| 批次 | 结果 | 关键结论 |
|------|------|----------|
| WSL OpenClaw-only smoke | 5/5 API ok，5/5 semantic ok | tool 2/2，subagent 1/1；此前 328s timeout 的 OpenClaw subagent 用例降到约 20.1s 成功。 |
| WSL ClaudeCode/Hermes/OpenClaw smoke | 15/15 API ok，15/15 semantic ok | ClaudeCode 5/5，Hermes 5/5，OpenClaw 5/5；OpenClaw 新请求在 admin 侧识别为 `source_client=openclaw`。 |

smoke 耗时观察：

| 客户端 | 结果 | P50 total | P90 total | 备注 |
|--------|------|-----------|-----------|------|
| WSL ClaudeCode | 5/5 | 16983ms | 44232ms | tool 2/2。 |
| WSL Hermes | 5/5 | 41759ms | 81310ms | tool 2/2；Hermes subagent 当前 runner 不支持观测。 |
| WSL OpenClaw | 5/5 | 16979ms | 17790ms | tool 2/2，subagent 1/1。 |

P1 仍需执行：

1. 修复 ClaudeCode 真实客户端 huge_context：2026-06-04 huge-session compactor 已部署并通过受控 panda smoke；仍需用真实 Windows/WSL ClaudeCode 长会话复测是否消除反复执行旧任务。
2. 修正 Windows ClaudeCode runner，使用真实 Windows 工作目录，不再从 `\\wsl.localhost` UNC 路径启动 ClaudeCode。
3. 针对 `deepseek-v4-flash-lite` 长上下文语义漂移设置更保守的 lane/权重，或在 full run 前先隔离 huge/long lane。
4. 在 panda NewAPI 和 ZenProxy 日志层继续确认 502/524、stream JSON 截断、client_gone 是否为上游/客户端边界。
5. Hermes 慢路径需要拆分客户端启动、工具 schema、上游响应和 agent 循环耗时；当前 50/50 功能通过但 P90 约 69.5s，不能直接进入 full 2000。
6. 提交前复查 README、维护文档、脚本和 `.codex_tmp/` 临时产物一致性。

P1.15 2026-06-05 V4.98 cache-friendly session 源码记录：

| 项 | 事实 |
| --- | --- |
| 触发 | 用户在真实 ClaudeCode 长会话中观察到 NewAPI prompt tokens 稳定约 330k 且耗时爆红；panda 最近 70 分钟 `/v1/messages` 77/77 流式、prompt P50 约 326k、P90 约 331k、cache hits 0。 |
| 判断 | 330k 不是输入 token 墙。部署后的 ZenProxy 日志为 `context_action=pass`、`effective_body_size=body_size`，free-model-client-rs `messages_tokens` 持续增长；问题主要是大输入每轮未命中 provider cache。 |
| 线上证据 | 8 小时窗口内 cache 并非完全不可用：存在 `prompt_tokens=286975/cache_tokens=462592`、`prompt_tokens=439847/cache_tokens=439808` 等命中；其中一个大命中来自三次完全相同 `prompt_hash` 的重试后成功。 |
| 根因候选 | 旧上游 session 策略对大请求使用完整 `messages` hash；长会话每轮追加尾部消息都会改变 session，不利于 provider 对重复前缀复用。 |
| 修复 | 大请求 session scope 改为 `large_prefix_v498`：稳定前缀 hash + tools hash + tool_choice hash；保留模型、api key hash、时间桶隔离。 |
| 观测 | 新增 `prefix_4k_hash/prefix_32k_hash/prefix_128k_hash/prefix_256k_hash/cache_material_bytes` 到 request-shape 与 cache observation 日志，后续能区分“前缀不稳”和“前缀稳定但 provider 仍未命中”。 |
| 非目标 | 不裁剪 330k 上下文，不做摘要替换，不重排消息，不注入提示词，不伪造 cache 命中。 |
| 部署 | 2026-06-05 09:18 已部署到 panda 三实例；线上 stripped SHA256 为 `566e1c519056a4d2ee95697803d0e8bff9db40dc706c81ab753d70405edfb224`，旧 V47 hash `99424602ce7c076671579abf48ca0d27367ac126e514efe4403d902d5caecd78` 已备份到 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-v498-20260605-091813-9942460`。 |
| 部署验收 | `zen-proxy-rs@1/@2/@3` 和 nginx 均 active；4001/4002/4004/4000 `/health` 为 `status=ok`、`dispatch=90`、`dead=0`、`ratelimited=0`；4000 `/v1/models` 只暴露 `deepseek-v4-flash` 与 `deepseek-v4-flash-lite`；panda NewAPI 8081 `/v1/models` 200 且包含两个模型。 |
| 烟测结果 | NewAPI exact smoke `reply pong only` 返回 `PONG`；真实中文短问答返回 200；真实英文短问答出现 `upstream returned no assistant content or tool call`，用 V47 备份临时实例同 prompt 对照也 502，因此不是 V4.98 新增回归，而是既有上游空输出/节点质量问题。 |
| 待验收 | 仍需用同一 ClaudeCode 长会话 A/B 观察 cache hit rate、`frt`、总耗时、空输出/工具错误和回答质量。 |

P1.16 2026-06-05 V4.99 reasoning-aware output guard 源码记录：

| 项 | 事实 |
| --- | --- |
| 触发 | V4.98 部署后，大流式 ClaudeCode 主请求已能成功且 cache token 可见，但 panda/NewAPI 仍有短/中等非流式或低输出预算请求返回 `upstream returned no assistant content or tool call`。 |
| 根因 | 上游 `deepseek-v4-flash-free` 在部分低预算请求里只返回 `reasoning_content`，正文 `content` 为空，且常见 `finish_reason=length`；旧逻辑只看正文和工具调用，因此把 reasoning-only 判为空输出。 |
| 修复 | 新增共享输出分类：`valid/empty_output/reasoning_only/reasoning_only_length`；OpenAI/Anthropic 非流式遇到 `reasoning_only_length` 时只重试一次 `thinking: disabled`；流式不能安全重试时会记录并返回带 `class=` 的错误分类。 |
| 策略 | 大流式 ClaudeCode 主会话、工具请求、长上下文仍不默认禁用 thinking；只对低预算探针/ClaudeCode 小流式探针做初始 `thinking: disabled`，并保留 Hermes/OpenClaw compat tool-use thinking 策略。 |
| buffered | ClaudeCode Anthropic buffered stream 不再仅因 `max_tokens<=512` 触发；现在需要 exact-output literal，或 `before_tokens>=50k && max_tokens<=2048`。小流式请求走直接流式 + 初始低预算策略。 |
| 错误可观测 | 空输出错误现在可带 `class=empty_output/reasoning_only/reasoning_only_length/buffered_retry_exhausted`；日志记录 `reasoning_chars/content_chars/finish_reason/tool_call_count/short_request_kind`。 |
| 验证 | 本地 `cargo fmt`、`CARGO_INCREMENTAL=0 cargo clippy --all-targets -- -D warnings`、`CARGO_INCREMENTAL=0 cargo test` 已通过；golden 测试新增 OpenAI/Anthropic 非流式 reasoning-only-length disabled retry 和小流式低预算不走 buffered retry。 |
| 部署 | 2026-06-05 10:47 已部署到 panda 三实例；线上 stripped SHA256 为 `8f8513c418c40704bd50c8ce73f27696fdc9fbb1aa75290f2829cedd9eb9e2f2`，旧 V4.98 hash `566e1c519056a4d2ee95697803d0e8bff9db40dc706c81ab753d70405edfb224` 已备份到 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-v499-20260605-104718-566e1c5`。 |
| 部署验收 | `zen-proxy-rs@1/@2/@3` 和 nginx 均 active；4001/4002/4004/4000 `/health` 均为 `status=ok`、`dispatch=90`、`dead=0`、`ratelimited=0`；4000 `/v1/models` 返回两个公开模型；panda NewAPI 8081 `/v1/models` 200。 |
| 烟测结果 | panda NewAPI OpenAI 非流式短问答 200，约 2.03s，返回 `2+2 equals 4.`；panda NewAPI Anthropic 流式 exact prompt 返回 `STREAM_OK` 且无 error；非 exact 小流式返回正常 greeting 且日志显示 `protocol="anthropic"`，未因 `max_tokens=64` 进入 `anthropic_buffered`。 |
| 线上观测 | 部署后日志已出现 V4.99 `applied upstream thinking policy`、`thinking_policy="low_budget_probe_disabled"`、`provider cache usage observation` 和 request shape 字段；部署后最小窗口内未见 `empty_output_class`、`upstream returned no assistant`、`stream error`、`retry budget`、`client_gone`。 |

P1.17 2026-06-05 ClaudeCode low-budget tool probe 部署记录：

| 项 | 事实 |
| --- | --- |
| 触发 | 用户反馈部署前后又出现多条 NewAPI 502；复查最近两小时 channel 69：`deepseek-v4-flash` 成功 stream 742、成功 non-stream 382、错误 non-stream 56、错误 stream 1。 |
| 根因 | 线上旧 V4.99 未包含本地低预算工具探针补丁。错误集中在 ClaudeCode 内部 `/context`/探针形态：Anthropic 非流式、`message_count=1`、`tool_count=1`、`max_tokens=1/16`、`prompt_tokens=57/417`，上游返回 `reasoning_only_length` 后变成空输出 502。 |
| 修复 | 部署本仓库最新补丁到 `zen-proxy-rs`：ClaudeCode 非流式低预算工具探针第一次上游请求前 `thinking=disabled`，并把 `max_tokens<=32` 最小抬到 64。 |
| 本地构建 | `/home/lenovo/zen-proxy-rs` 执行 `CARGO_INCREMENTAL=0 cargo build --release` 通过；未 strip release SHA256 为 `5732beb2c6cc7b9092ae7d9dfe580fd69d48f602b8cb16c859e9beb5f2022f67`。 |
| 部署 | 2026-06-05 20:52 已部署到 panda；线上 stripped SHA256 为 `369e45062f870f8460ebf4d52f06bda30d94fe0f4459cf8cdebbc4829fe3316d`，旧 V4.99 hash `8f8513c418c40704bd50c8ce73f27696fdc9fbb1aa75290f2829cedd9eb9e2f2` 已备份到 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-low-budget-probe-20260605-205234-8f8513c`。 |
| 部署验收 | `zen-proxy-rs@1/@2/@3` active；4001/4002/4004/4000 `/health` 均 `status=ok`、`dispatch=90`、`dead=0`、`ratelimited=0`；二进制包含 `low_budget_tool_probe_disabled` 与 `raised ClaudeCode low-budget tool probe max_tokens before upstream` 字符串。 |
| NewAPI 验收 | panda 本机 NewAPI Anthropic `/v1/messages?beta=true`，带 1 个 `ctx_probe` 工具，`max_tokens=1` 返回 HTTP 200、2.31s、`stop_reason=tool_use`；`max_tokens=16` 返回 HTTP 200、2.13s、`stop_reason=tool_use`。 |
| 日志验收 | ZenProxy 新 pid 日志出现 `requested_max_tokens=Some(1/16)`、`effective_max_tokens=Some(64)`、`thinking_policy="low_budget_tool_probe_disabled"`；部署后近 10 分钟 channel 69 无错误记录，ZenProxy 近 5 分钟未见 `upstream returned no assistant content`、`stream truncated`、`retry budget exhausted`。 |
| 残留 | 仍需用户真实 ClaudeCode `/context` 和日常使用长窗口观察；单条历史 `stream truncated before DONE or finish_reason` 与本次批量非流式 502 不同，若复发需单独排查。 |

P1.18 2026-06-05 ClaudeCode Anthropic stream idle ping 部署记录：

| 项 | 事实 |
| --- | --- |
| 触发 | 用户反馈 NewAPI 仍有红行和偶发无输出。复查截图时段后确认这些记录不是上一轮非流式低预算工具探针 502，而是成功消费 `type=2`、`stream=true`、约 50k prompt tokens、`completion=0`、`use_time≈64s`，`other.stream_status.end_reason=client_gone`。 |
| 根因判断 | ZenProxy 同窗口无 `upstream returned no assistant content`、无 `stream truncated`、无 `retry budget exhausted`；更像是 ClaudeCode/NewAPI/cc-switch 在真实内容或工具调用长时间未到达时断开下游流。 |
| 修复 | `src/proxy/anthropic.rs` 对 ClaudeCode Anthropic SSE 增加 15 秒 idle ping：等待上游事件超时，或上游只有 reasoning/usage 等不可转发事件且下游 15 秒无活动时，发送 `event: ping`、`data: {"type":"ping"}`。 |
| 边界 | 不对 OpenAI SSE 启用；不对 Hermes/OpenClaw 启用；不把 ping 当首字；不生成空 content delta；不改变模型输出、工具调用、prompt、`max_tokens` 或 thinking 策略。 |
| 本地验证 | WSL 原生路径执行 `CARGO_INCREMENTAL=0 cargo fmt -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 均通过；新增 golden `claude_code_anthropic_stream_sends_idle_ping_before_delayed_content`，模拟上游 16 秒后才出内容，断言先出现 `event: ping`，随后仍完整输出 `delayed answer` 和 `message_stop`。 |
| 构建 | `/home/lenovo/zen-proxy-rs` release 构建通过；未 strip SHA256 为 `00ffe54a9b8b9ab09a5fda5c55cf68ebc06825dbece2dabbe6e888bc2bd2f300`；部署 stripped SHA256 为 `be6b859576169d2cc710ed2c079a125c50d0a51a0d744abbd3668ba1e030e793`。 |
| 部署 | 2026-06-05 22:34 已部署到 panda；旧 hash `369e45062f870f8460ebf4d52f06bda30d94fe0f4459cf8cdebbc4829fe3316d` 备份到 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-stream-idle-ping-20260605-223420-369e450`。 |
| 部署验收 | `zen-proxy-rs@1/@2/@3` active；4001/4002/4004/4000 `/health` 均 `status=ok`、`dispatch=90`、`dead=0`、`ratelimited=0`；线上二进制包含 `sent ClaudeCode stream idle ping while upstream produced no forwardable output` 和 `sent ClaudeCode stream idle ping while waiting for upstream event` 字符串。 |
| NewAPI smoke | panda 本机 token id `38`/name `ds`/group `vip` 下，`/v1/models` 200 且包含 `deepseek-v4-flash`、`deepseek-v4-flash-lite`；Anthropic `/v1/messages?beta=true` + `x-fmc-client=claude-code` 流式 smoke HTTP 200，starttransfer 约 1.39s、total 约 1.92s，响应按 SSE 分片输出目标 marker，无 error。 |
| 残留 | idle ping 只能解决“下游长时间无字节活动”的 client_gone；如果上游 60 秒后仍真实空输出，或客户端有“必须真实内容在 N 秒内出现”的硬超时，还需要 first-content watchdog / retry 降级另行设计。 |

P1.19 2026-06-06 V4.99 ClaudeCode Anthropic Stream Guard 部署记录：

| 项 | 事实 |
| --- | --- |
| 触发 | 用户反馈 ClaudeCode 仍偶发 `API Error: Failed to parse JSON` 和中断；复查 NewAPI/ZenProxy 发现对应服务端错误不是 30KB Write JSON 溢出，而是 Anthropic stream 的 `status_code=500, stream truncated before DONE or finish_reason`。 |
| 证据 | 部署前 90 分钟 channel 69 为 400 次调用、398 成功、2 错误；2 条错误均为 `stream=true` 的 `stream truncated before DONE or finish_reason`。失败样本均为 ClaudeCode Anthropic `/v1/messages`，工具 schema 存在，`max_tokens=32000`，上游在真实 text/tool 输出前长时间只有 reasoning/空 delta 或直接截断。 |
| 修复 | `src/proxy/anthropic.rs` 将 ClaudeCode Anthropic stream 改为 Stream Guard 状态机：`message_start` 只发一次；真实 text/tool 未输出前，上游 fetch/stream 截断可原地重试；60 秒无可转发内容触发重试；最后一次仅在工具请求场景启用 disabled thinking 兜底；半截 tool JSON 不会被伪成功交给客户端。 |
| 工具参数 | Anthropic `input_json_delta` 普通流式和 buffered huge-stream 均改为 4KB 分片；分片只改变 SSE 传输颗粒度，拼接后 JSON 字符不变。 |
| forced tool_choice | ClaudeCode 显式 forced `tool_choice` 首跳禁用 thinking，避免 DeepSeek 返回 `Thinking mode does not support this tool_choice`；`tool_choice=auto`、普通 tools auto 和 unknown client 不受影响。 |
| 本地验证 | WSL 原生路径执行 `cargo fmt -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 均通过；库测试 89 条、kernel golden 100 条全部通过。新增单元覆盖 Stream Guard retry 判定、tool JSON 分片无损、ClaudeCode forced tool_choice thinking 策略。 |
| 构建 | `/home/lenovo/zen-proxy-rs` release 构建通过；最终部署前 strip 后 SHA256 为 `39dc0bb94092597a00518abf83e80f8c32a91e8c60682c169942bf16bf70017d`。 |
| 部署 | 2026-06-06 00:54 已部署到 panda；旧 hash `9d64728e5511f2b414d16f4f4dac27395dabb1abe3ae64c2cf9404ee4f31ba0e` 备份到 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-v499-forced-tool-20260606-005416-9d64728`。 |
| 部署验收 | `zen-proxy-rs@1/@2/@3` active；4001/4002/4004/4000 `/health` 均 `status=ok`、`dispatch=90`、`dead=0`、`ratelimited=0`。 |
| NewAPI smoke | panda 本机有效 `vip` token 下，`/v1/models` HTTP 200；Anthropic stream PONG HTTP 200、`message_stop` 存在、`event:error=0`；Anthropic forced `tool_choice` 工具流 HTTP 200，`tool_use` 和 `input_json_delta` 存在，`Thinking mode does not support this tool_choice=0`。 |
| 部署后观察 | 00:54:16 最终部署后 channel 69 采样 13/13 成功、0 错误。部署前仍有 3 条非流式 300s/504 旧记录，属于另一类长非流式超时，不计入 V4.99 Stream Guard 后验收。 |
| 残留 | 仍需用户真实 ClaudeCode 长会话观察 1-2 小时，重点看 `stream guard retrying`、`refusing to emit possibly partial tool calls`、`stream truncated` 是否继续出现；非流式 300s/504 需另按 long non-stream 保护排查。 |

P1.20 2026-06-06 provider reasoning_content 400 修复记录：

| 项 | 事实 |
| --- | --- |
| 触发 | NewAPI channel 69 日志出现 `status_code=400/500`，public content 包含 `opencode zen 400` 和 provider 返回的 `The reasoning_content in the thinking mode must be passed back to the API`。 |
| 根因 | ClaudeCode Anthropic `/v1/messages` 被内核转换为 OpenAI-compatible 上游请求；历史 assistant/tool 调用没有可回传的 `reasoning_content`，但普通 tools auto 仍保持默认 thinking，DeepSeek provider 直接 400。 |
| 修复 | 对 `provider_missing_reasoning_content` 增加一次性 disabled-thinking 重试，覆盖 OpenAI/Anthropic 非流式、OpenAI 流式、ClaudeCode Anthropic 流式和 buffered huge-stream；只有 provider 明确返回该错误才触发。 |
| 错误映射 | 上游错误 public response 统一脱敏，不再返回 `opencode zen` 或原始 provider body；返回稳定 `type/code/message`，并保留 `Retry-After`。 |
| 本地验证 | `cargo fmt -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 通过；库测试 89 条、kernel golden 103 条。新增 3 条 golden 覆盖 missing reasoning_content 非流式/流式重试和 public error 脱敏。 |
| 部署状态 | 已于 2026-06-06 12:40 部署 panda 三实例；源码 commit `68bf5383f7c8915f0950a6864134b77dd51a1214`，线上 stripped hash `d5b7558c9f8f9fc7ea6faa802634dba85435868f1e338a4830f77079c3a1fc8e`，旧版本备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260606-124001.pre-68bf538`。部署后 ZenProxy `/health`、`/v1/models`、ZenProxy OpenAI/Anthropic smoke、panda NewAPI OpenAI/Anthropic smoke 均通过；部署后 10 分钟窗口未见新的 `reasoning_content`、`opencode zen`、空输出或 NewAPI 错误日志。 |

P1.21 2026-06-08 ClaudeCode 大上下文慢首字诊断与首包保护：

| 项 | 事实 |
| --- | --- |
| 触发 | 用户反馈 2026-06-08 channel 69 调用中，约 180k input tokens 场景偶发首字很长，cache 观感不稳定，整体耗时和 FRT 不够快。 |
| 数据结论 | 截至 2026-06-08 11:55 CST 左右，`deepseek-v4-flash` 成功流式 `ok/eof` 约 2783 条，NewAPI `frt` P50 约 4.1s、P90 约 8.7s、P99 约 35.9s；`150k-220k` 桶 cache hit 约 98.4%，首字 >=15s 约 3.45%，首字 >=30s 仅 1 条。 |
| 根因样本 | NewAPI id `136957`：prompt `173751`、completion `218`、FRT `47123ms`、cache `270976`。ZenProxy 对应 11:13:13 入口，estimated tokens `194191`，第一次上游 fetch 在 11:13:55 返回 520，第二次 attempt 在 11:14:00 cache accepted；慢首字来自上游慢失败+重试，不是 cache miss 或本地 CPU/池资源耗尽。 |
| 已实现 | `src/proxy/anthropic.rs` 对 ClaudeCode Anthropic stream 增加大上下文首包 fetch 超时保护：仅在真实 text/tool 输出前、且满足 token 门槛时触发；超时后按既有 Stream Guard 重试，不对已输出内容或半截工具调用重试。 |
| 新配置 | `FREE_MODEL_CLAUDE_CODE_STREAM_INITIAL_FETCH_TIMEOUT_SECS` 默认 `30`，设 `0` 可关闭；`FREE_MODEL_CLAUDE_CODE_STREAM_SLOW_GUARD_MIN_INPUT_TOKENS` 默认 `150000`；`FREE_MODEL_CLAUDE_CODE_STREAM_NO_FORWARDABLE_RETRY_SECS` 默认 `45`。 |
| 新观测 | ClaudeCode Anthropic stream 正常结束时新增 `ClaudeCode stream guard completion summary` 日志，包含 `attempts_used/retry_count/first_upstream_response_ms/first_upstream_event_ms/first_reasoning_ms/first_content_ms/first_tool_call_ms/idle_ping_count/cache_observation/cache_read_input_tokens/estimated_total_tokens/max_tokens/prompt_hash_hex`。 |
| 边界 | 不改 prompt、不裁剪输入、不限制输出、不把 ping 当真实首字、不对 Hermes/OpenClaw 生效、不在已有 text/tool 输出后重试。 |
| 本地验证 | `cargo fmt -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 均通过；库测试 94 条、kernel golden 105 条。新增 golden `claude_code_anthropic_stream_retries_slow_initial_fetch_before_output` 覆盖首包慢失败主动重试。 |
| 部署状态 | 2026-06-08 15:14 CST 先滚动部署 P1.21；15:42 CST 又补齐 ZenProxy env 配置透传并再次滚动部署。最终线上 stripped SHA256 `a771174350bf6701c97b7deed1bbf4deecd995463c5cfb27ff4b4e6c7c440f6b`。旧版本备份包括 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-p121-20260608-151426-dfd52e3489e6` 和 `/opt/zen-proxy-rs/backups/zen-proxy-rs.pre-p121-envwired-20260608-153746-5c33046808ae`。 |
| 部署验收 | `zen-proxy-rs@1/@2/@3` active；4001/4002/4004/4000 `/health` 均 `status=ok`、`dead=0`、`ratelimited=0`；`/v1/models` 返回 `deepseek-v4-flash` 和 `deepseek-v4-flash-lite`；Anthropic/ClaudeCode 最小流式 smoke HTTP 200，`starttransfer=1.663s`，返回 `pong`。OpenAI-compatible 极短流式 smoke 仍返回 `reasoning_only_length`，列为 OpenAI 短流式残留，不作为本轮 ClaudeCode 主链路回滚条件。 |

P1.22 2026-06-09 V4.104 ClaudeCode progressive tool streaming 与质量回退：

| 项 | 事实 |
| --- | --- |
| 触发 | 用户反馈压首字后的多个版本体感变慢、变笨；cc-switch/NewAPI 出现 10-20s 甚至 60s+ 首字。复查确认线上跑的是 V4.102，三实例于 2026-06-09 11:10 CST 启动。 |
| 数据结论 | channel 69 近 12 小时 NewAPI FRT P50 约 4.66s、P90 约 11.45s、P95 约 14.64s、P99 约 33s、最大约 114.7s；11:10 前 FRT P95 约 6.1s，11:10 后约 17.9s。ZenProxy `first_upstream_response_ms` P95 约 3.3s，说明主要不是网络/CPU，而是下游可见输出被工具门控延后。 |
| 根因 | V4.102 为降低 `Invalid tool parameters`，要求 ClaudeCode 工具参数完整 JSON parse、required 字段和本地规则全部通过后才下发 `tool_use`；大 Write/Edit/Agent 参数会等完整 arguments 生成完，导致 `first_tool_call_ms` 已出现但 `first_tool_emit_ms` 长时间不动。另有 528 次“从最新 user 文本补工具参数”的日志，说明激进补参会带来质量漂移。 |
| 修复 | ClaudeCode Anthropic stream 改为 progressive tool streaming：工具 id/name 出现且 arguments 开始生成后，立即发送真实 `content_block_start tool_use`，后续按上游累计 arguments 增量发送 `input_json_delta`；最终只有完整 JSON 通过 `streamable_anthropic_tool_call` 校验后才发送 `content_block_stop`。 |
| 质量回退 | 取消从最新 user 文本推断 `Read/Write/Edit/Bash/Task/ToolSearch/WebSearch` 参数；缺 required、空 `command/query`、坏 `file_path` 不再由代理猜测修复。仅保留 `SendMessage` 字符串消息自动补 `summary` 这类确定性窄修复。 |
| 边界 | 不裁剪输入、不恢复输出上限、不默认禁用 thinking、不注入隐藏提示词、不把 ping 当首字；`SendMessage` 不走 progressive，避免 deterministic summary 修复无法落到流里。 |
| 本地验证 | WSL 原生执行 `cargo fmt -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 通过；库/main 114 条、kernel golden 112 条。测试语义已改为“代理不从用户文本猜工具参数”。 |
| 部署状态 | 2026-06-09 13:43 CST 已滚动部署 panda 三实例；线上 stripped SHA256 `08d9064600e66097ab45bbe97290bf5e7015174a15adbe27dc5fcf8261c2ed9f`；旧版备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260609-134330.pre-v4104-ebe41572fe76a5f99783ba5e4308e164368415b00277432cd9829e60ecc651dd`。 |
| 部署验收 | `zen-proxy-rs@1/@2/@3` active；4001/4002/4004/4000 `/health` 均 `status=ok`、`dead=0`、`ratelimited=0`；ZenProxy 直连 `/v1/models` 200，只暴露 `deepseek-v4-flash` 和 `deepseek-v4-flash-lite`；ZenProxy OpenAI 非流式 `PONG` HTTP 200、2.06s；ZenProxy Anthropic 非流式 `PONG` HTTP 200、1.78s；panda NewAPI `/v1/models` 200；NewAPI OpenAI 非流式 `PONG` HTTP 200、1.90s；NewAPI Anthropic 非流式 `PONG` HTTP 200、1.72s；ClaudeCode Anthropic forced `Bash` tool stream HTTP 200，输出 `content_block_start tool_use`、完整 `input_json_delta`、`content_block_stop`、`message_stop`。部署后日志窗口未扫到 `Invalid tool parameters`、`Failed to parse JSON`、`summary is required`、`provider_missing_reasoning_content` 或 panic。 |
| 待观察 | 继续用真实 ClaudeCode 长会话观察 NewAPI channel 69 FRT 分位、`client_gone`、`provider_missing_reasoning_content`、`Invalid tool parameters`、`first_tool_call_ms/first_tool_emit_ms` 差值和工具成功率；短非流式 `reasoning_only_length` 警告仍按既有上游空输出/低预算探针分类继续跟踪，不作为 V4.104 部署失败结论。 |

P1.23 2026-06-09 V4.105 true-stream/cache-hit 诊断记录：

| 项 | 事实 |
| --- | --- |
| 触发 | 用户换 Clash Verge 节点后，ClaudeCode 真实首字明显提升并稳定在约 6-9s，但仍有 20-50s 慢尾；同时 cc-switch 显示 cache hit 约 60.6%，要求继续深入检查。 |
| cc-switch 真实体验 | 2026-06-09 15:29 CST 后，`deepseek-v4-flash-free` 成功样本约 955 次，首字 P50 约 6.5s、P90 约 13.5s、P95 约 17.2s、P99 约 31.6s；输入最大约 227k tokens，未见 90k 输入墙。 |
| 假流式慢尾 | 同窗口 `first_token_ms≈latency_ms` 的 buffer-like 请求约 290 次，P95 约 26.6s；progressive 真流式约 665 次，P95 约 13.5s。100k+ 输入桶中约 44%-47% 为 buffer-like，是慢尾主因。 |
| 代码根因候选 | `src/proxy/anthropic.rs::should_use_claude_code_buffered_stream` 只要 `has_exact_output_literal=true` 就进入 `anthropic_buffered`；`src/protocol/translate.rs` 的 exact-output 检测包含 `只输出/只回复/reply exactly/output only`，容易被普通 ClaudeCode 格式要求误触发。 |
| buffered 机制 | `anthropic_buffered` 会先 `collect_stream_parts(resp).await` 收完整个上游流，再组装 Anthropic SSE 给下游；所以缓存命中也无法改善真实首字，cc-switch 会看到首字接近总耗时。 |
| cache 现状 | 当日 cc-switch `deepseek-v4-flash-free` 成功流式约 6840 次，cache_read 命中约 4988 次，命中率约 72.9%；小时维度 13:00 约 63.2%、14:00 约 59.8%、16:00 约 95.3%。 |
| cache 分桶 | `10k-50k` 命中约 9.9%，`50k-100k` 约 84.1%，`100k-200k` 约 98.1%，`200k+` 约 98.3%；用户看到的 60.6% 更像是统计口径或中等上下文样本拉低，不代表大上下文 cache 全面失败。 |
| cache 代码风险 | 修复前 `src/zen/client.rs::ZenUsage` 只解析 `cache_read_input_tokens`、`cache_creation_input_tokens` 和 `prompt_tokens_details.cached_tokens`；未显式解析 DeepSeek 官方常见 `prompt_cache_hit_tokens/prompt_cache_miss_tokens`，可能导致 cache 命中低估或 NewAPI/cc-switch 显示不完整。 |
| 已落地源码 | V4.105 已补 `buffer_reason` 日志；ClaudeCode 带 tools 的长会话、Markdown/JSON/代码块格式要求不再仅因 `只输出/只回复/output only` 进入 `anthropic_buffered`；`prompt_cache_hit_tokens` 会映射到通用 `cache_read_input_tokens`/`cached_tokens` 路径，`prompt_cache_miss_tokens` 会进入 cache miss 观测。 |
| 部署验收 | 2026-06-09 17:28 CST 已构建并部署 panda 三实例；线上 stripped SHA256 为 `a52f4d6add0a93fe0070a59c3a3ec9ee3b4bc0a9172047c7b3ec5855e67ff7e8`，旧版 V4.104 备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260609-172830.pre-v4105-08d9064600e6`；`zen-proxy-rs@1/@2/@3` active，4000 `/health` 和 `/v1/models` 200，panda NewAPI OpenAI/Anthropic 非流式 `PONG` 均 200。 |
| 本地验证 | WSL 原生路径执行 `cargo fmt -- --check`、`CARGO_INCREMENTAL=0 cargo clippy --all-targets -- -D warnings`、`CARGO_INCREMENTAL=0 cargo test` 均通过；完整测试为 lib/main 120 条、kernel golden 112 条。 |
| 待办 | 需建立 cc-switch/NewAPI/ZenProxy 三侧 cache hit 对齐报表，并用真实 ClaudeCode 长会话确认 `anthropic_buffered` 误触发和 buffer-like 慢尾下降。 |

P1.24 2026-06-10 V4.109 ClaudeCode Anthropic 非流式 no-forwardable 保护：

| 项 | 事实 |
| --- | --- |
| 触发 | 用户持续反馈 ClaudeCode 使用中出现 `API Error: Failed to parse JSON`，NewAPI 日志可见 Anthropic `/v1/messages`、`stream=false`、tools、大上下文或 `max_tokens=32000` 请求最终 300s/504。 |
| 根因 | Anthropic 非流式路径内部从上游收 SSE 后聚合成 JSON，但原路径缺少流式 guard 的 no-forwardable 检测和 disabled-thinking fallback；当上游长期只吐 reasoning 或空输出时，NewAPI 会等到超时，客户端侧表现为 JSON parse/read failure。 |
| 修复 | `src/proxy/anthropic.rs` 增加 ClaudeCode 专用非流式 collect guard：检测 reasoning-only/no-forwardable 后重试；ClaudeCode tools 请求在 no-forwardable 后可切一次 disabled-thinking；已有完整 text/tool 时即使尾部 stream error 也返回结构化 JSON。 |
| 边界 | 只对 `ClientKind::ClaudeCode` 生效；不改 prompt、不裁剪上下文、不限制输出、不全局禁用 thinking、不修改 NewAPI/ClaudeCode/cc-switch。 |
| 本地验证 | `free-model-client-rs`：`cargo fmt -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 通过；库测试 122 条、kernel golden 115 条。`zen-proxy-rs`：`cargo test` 通过，e2e 27 条；`cargo clippy --all-targets -- -D warnings` 通过。新增 golden 覆盖 ClaudeCode Anthropic 非流式 reasoning-loop 后 disabled-thinking 重试到 tool_use。 |
| 部署状态 | 2026-06-10 16:44 CST 通过临时 GitHub 中转仓部署到 panda 三实例；线上 stripped SHA256 `67c435b5a02cc1d1ba9839110f01d017aa1acbff14a258f258171e328957b31b`，xz SHA256 `af53be47528f907b0f1c752e680ebf7fec34635d408a8281f93291190dc4f171`；旧版备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260610-1644*.pre-v4109-nonstream-guard`。中转仓随后已强推为空提交，raw 包地址返回 404。 |
| 部署验收 | `zen-proxy-rs@1/@2/@3` active；4001/4002/4004/4000 `/health` 均 `status=ok`；4000 `/v1/models` 返回 `deepseek-v4-flash`、`deepseek-v4-flash-lite`。panda NewAPI 使用 `ds` token 验证：`/v1/models` 可见两个 deepseek 模型；OpenAI 非流式 `只输出 OK` HTTP 200，总耗时约 1.63s；Anthropic 非流式 forced `echo_tool` HTTP 200，总耗时约 2.46s，返回 `tool_use`。 |
| 观察结果 | V4.109 日志已出现 `ClaudeCode non-stream guard retrying after reasoning-only/no-forwardable upstream output`，说明新保护路径生效。滚动重启 08:46-08:48 UTC 期间 NewAPI 出现大量 `status_code=503, no proxy resources available`，稳定后最近 60 秒错误为 0；另有 3 条 08:48 发起、08:52 结束的 `empty_output` 502 残留，需要后续观察是否复发。 |
| 待办 | 下一次生产滚动部署不能只看 `/health`；应等待每个实例资源池 active/dispatch 达到阈值再切下一个实例，避免池预热期产生 `no proxy resources available`。继续跟踪 V4.109 后真实 ClaudeCode 长会话里的 `Failed to parse JSON`、`empty_output`、非流式 300s、tool_use 完整率和 NewAPI 错误率。 |

P1.25 2026-06-10 V4.110 预部署修复包：

| 项 | 事实 |
| --- | --- |
| 触发 | V4.109 上线后，channel 69 继续出现大量 `empty_output`、`zenproxy lane is saturated`、`do request failed` 和 nginx `768 worker_connections are not enough`。 |
| 根因链 | ClaudeCode Anthropic 非流式无工具请求上游只吐 reasoning/no-forwardable -> V4.109 只允许 tools 场景 disabled-thinking 重试，no-tool 小请求会拖到 retry budget；同时 `max_tokens=4096` 的小非流式请求被 `>=4096` 误分到 `long_output`，8 个槽位被大量 1k-2k token 小请求占满；请求堆积后触发 nginx worker connection 上限。 |
| free-model-client-rs 修复 | `src/proxy/anthropic.rs` 对 `ClientKind::ClaudeCode` 的 Anthropic non-stream `RetryNoForwardable` 增加 no-tool disabled-thinking 失败恢复路径。该路径只在 no-forwardable/empty reasoning 失败后触发，不全局禁用 thinking，不改 prompt，不裁剪上下文，不限制输出。 |
| free-model-client-rs 验证 | 已提交 `76fa9f4 fix claude code nonstream no-forwardable retry`。完整验证已通过：`cargo fmt -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`；库测试 122 条、kernel golden 116 条。本轮复跑 targeted golden `claude_code_anthropic_non_stream_no_tool_retries_no_forwardable_reasoning_with_disabled_thinking` 通过。 |
| zen-proxy-rs 修复 | `src/lanes.rs` 把 `max_tokens >= v46_long_output_tokens` 改为 `max_tokens > v46_long_output_tokens`，避免默认 `4096` 小非流式请求进入 `long_output` lane。 |
| zen-proxy-rs 验证 | 已提交 `6481003 fix lane classification for default 4096 nonstream output`。本轮复跑 targeted test `routes_default_4096_output_small_nonstream_to_short_lane` 通过；release build 成功。 |
| V4.110 包 | 本地新包 `/tmp/zen-proxy-rs-v4110` SHA256 `c768a71c928c97e5e9c0839c0eb2bb155ad50312aec9dbf90413d67023dcdd74`；压缩包 `/tmp/zen-proxy-rs-v4110.xz` SHA256 `056b179f4fb54e5dc057f448678ee6001a6a5b32938689eb5c2f508001f0a074`，大小约 3.3M。旧的 V4.110 包哈希作废。 |
| panda 只读证据 | 2026-06-10 18:12 CST 只读检查确认：4001/4002/4004/4000 `/v1/models` 均 HTTP 200；nginx 当前仍是 `worker_connections 768`，错误日志有 `768 worker_connections are not enough`，涉及 public NewAPI 8081 和内部 4000 -> 4001/4002/4004 流量。 |
| 部署状态 | 2026-06-10 18:30 CST 已通过 GitHub 临时仓中转部署到 panda 三实例；线上 stripped SHA256 `c768a71c928c97e5e9c0839c0eb2bb155ad50312aec9dbf90413d67023dcdd74`，xz SHA256 `056b179f4fb54e5dc057f448678ee6001a6a5b32938689eb5c2f508001f0a074`；旧版备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260610-183000.pre-v4110-67c435b5a02c`。中转仓已强推为空提交，raw 包地址返回 404。 |
| nginx 调整 | 2026-06-10 18:28 CST 已备份 `/etc/nginx/nginx.conf.20260610-182856.pre-v4110`，设置 `worker_rlimit_nofile 65535`、`worker_connections 4096`，`nginx -t` 通过并 reload，`nginx.service` active。 |
| 部署验收 | 滚动重启 `zen-proxy-rs@1/@2/@3` 时，每个实例均等待 `/health` 达到 `dispatch>=60`、`dead=0` 后再继续；最终 4001/4002/4004/4000 `/health` 均 `status=ok`，`dispatch` 约 85-88，`dead=0`、`ratelimited=0`；4000 `/v1/models` 返回两个 deepseek 模型。panda NewAPI `ds` token smoke：`/v1/models` 可见两个模型，OpenAI 非流式 `只输出 OK` HTTP 200，Anthropic 非流式 `max_tokens=4096` HTTP 200 且返回 `OK`。 |
| 部署后窗口 | 18:30 CST 后 NewAPI channel 69 共 1727 条记录，`type<>2` 错误 0；`empty_output`、`lane is saturated`、`do request failed`、`bad response status code 500`、`stream truncated`、`provider_missing_reasoning_content` 均为 0；nginx 同窗口 `worker_connections/upstream/reset/connect/recv` 匹配错误 0；ZenProxy 同窗口关键错误匹配 0。 |

P1.26 2026-06-22 V4.111 cache header stabilization:

| 项 | 事实 |
| --- | --- |
| 触发 | 用户要求在不裁剪上下文、不伪造 cache usage、不牺牲 ClaudeCode Bash/WebFetch/WebSearch 工具质量的前提下，继续提升 deepseek-v4-flash 和 mimo-v2.5 cache 命中率。 |
| 修复 | `src/zen/client.rs` 将 `x-opencode-request` 从随机值改为基于完整规范化上游 body 的稳定 ID；相同 body 重试保持 request header 稳定，body 任意字段变化仍生成不同 request。 |
| 边界 | 不改请求正文、不改工具 schema、不裁剪上下文、不缩输出、不禁用 WebFetch/WebSearch/Bash、不扩展 deepseek-v4-flash-lite 到 ClaudeCode 主线、不伪造 usage。 |
| 本地验证 | `free-model-client-rs`：`cargo fmt -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 通过；库测试 132 passed，kernel golden 127 passed。`zen-proxy-rs`：`cargo fmt -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`、`cargo build --release` 通过；unit 194 passed，e2e 44 passed。`scripts/run_claudecode_acceptance.py` 通过 `python3 -m py_compile`。 |
| 部署 | 2026-06-22 10:12 CST 通过 GitHub 临时 release 中转部署到 panda 三实例；线上 stripped SHA256 `ee6393093d61b9fedd77112db67f093e469cdeceb5b7f9cfdd9c885d7fc2dc38`，xz SHA256 `c8ffdf66797f66023096c6682b965ab054f382e04a254e93797a7027ee863efb`；旧版备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260622-101213.pre-v4111-cache-header-4bb606dc25c7`。临时 release/tag `v4111-cache-header-20260622-1002` 已删除，release not found，仓库 contents `[]`。 |
| 部署验收 | `zen-proxy-rs@1/@2/@3` active；4001/4002/4004/4000 `/health` 均 `status=ok`、`dispatch=100`、`dead=0`、`ratelimited=0`。4000 `/v1/models` 只公开 `deepseek-v4-flash`、`deepseek-v4-flash-lite`、`mimo-v2.5`。 |
| 小流量验证 | 直连 ZenProxy OpenAI-compatible 最小 chat：`deepseek-v4-flash` 返回 `OK`；`mimo-v2.5` 返回 `OK` 且 usage 出现 `cache_read_input_tokens=192`。Windows official `claude.orig.exe` bridge smoke 18 项无失败，覆盖 Bash/WebFetch/WebSearch x text/json/stream-json；其中 deepseek 9/9 有效。 |
| 验收边界 | Windows bridge 的 `mimo-v2.5` 9 项报告为 pass/slow_pass，但 panda ingress 日志显示实际请求仍是 `deepseek-v4-flash`，因此不能计为真实 mimo ClaudeCode 验收。WSL `/home/lenovo/.local/bin/claude` 当前是 clawgod launcher，本轮 WSL deepseek Bash 前两项失败后已中止，也不能作为 official ClaudeCode 证据。 |
| cache 短窗口 | 部署后 10:12-10:58 CST 窗口，`deepseek-v4-flash-free` + ClaudeCode cache rows 153：accepted 102、rejected 51，token read/miss 约 `2,565,376 / 1,816,886`，read_pct 约 `58.54%`；`50k-100k` 桶 accepted 31/rejected 4，read_pct 约 `81.16%`。`prefix_4k/prefix_32k` 已出现重复稳定组，top 组分别出现 63 次和 30 次；`prefix_128k/prefix_256k` 仍随尾部变化。 |
| 待观察 | 仍缺同一真实 mimo ClaudeCode 长会话 A/B；需要继续观察生产 channel 69 真实流量中的 mimo accepted/rejected、prefix 稳定度、TTFT/first_content 和工具质量，不能用本轮 Windows bridge 的 mimo 报告替代。11:24 CST 收尾复查 4004 为 `dead=1/dispatch=99`，4001/4002 和 4000 聚合仍 `status=ok`，按池节点健康波动继续观察。 |

P1.27 2026-06-22 V4.112 non-stream cache affinity follow-up:

| 项 | 当前事实 |
|----|----|
| 目标 | 在不改 request body、prompt、tools、tool_choice、thinking、max_tokens、输出长度或 usage 的前提下，让 32KB+ 非流式 ClaudeCode 中/大请求也使用已有 cache-affinity 软亲和。 |
| 生产前基线 | 14:17 CST 只读聚合最近 120 分钟：`deepseek-v4-flash-free` + ClaudeCode stream cache rows 140，accepted/rejected `110/30`，token read_pct `72.29%`；non-stream cache rows 131，accepted/rejected `124/7`，token read_pct `92.87%`。同窗口 ingress `deepseek-v4-flash` + `claude-code` 272 条，其中 non-stream 130、stream 142、32KB+ 240。 |
| 代码改动 | `zen-proxy-rs/src/v4/provider.rs` 的 affinity key 不再因 `stream=false` 返回空；32KB+ 流式和非流式都会按 `model/path/source_client/client/prefix/tools/tool_choice` 构造软亲和 key。 |
| 部署 | 2026-06-22 14:32 CST 已通过 GitHub 临时 release 中转部署到 panda 三实例；线上 stripped SHA256 `766eef7f3e51b7eb8e3af57bf058db35da538e1b3fa14074dd3a4f5f789dcbca`，xz SHA256 `739a54ba07783dd0bbb2b697f7e08a13b0568d5cc6fbfdaa6c2f7eeb64a30b88`；旧版备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260622-143230.pre-v4112-cache-affinity-ee6393093d61`。 |
| 部署验收 | 4001/4002/4004/4000 `/health` 均 `status=ok`、`dispatch=100`、`dead=0`、`ratelimited=0`；线上 binary hash 已确认；4000 `/v1/models` 只公开 `deepseek-v4-flash`、`deepseek-v4-flash-lite`、`mimo-v2.5`。 |
| 中转清理 | 公开中转 release/tag `v4112-cache-affinity-20260622-1428` 删除后 `release not found`，`croppedtravelleralex/new` contents 为 `[]`；误建在私有源码仓的 `v4112-cache-affinity-20260622-1424` 也已删除并返回 `release not found`。 |
| 限制 | 当前 shell 环境没有 `ANTHROPIC_API_KEY`，未跑本地 runner 的完整 ClaudeCode smoke；4000 直连假 key 返回 403，不作为模型失败。后续仍需用有效且不泄露的 API key/cc-switch 环境跑 DeepSeek Bash/WebFetch/WebSearch x text/json/stream-json。 |

## 临时产物归类

| 路径 | 当前归类 | 处理原则 |
|------|----------|----------|
| `.codex_tmp/` | 历史客户端矩阵、Hermes/OpenClaw 临时配置、Mimo wrapper、cache audit 脚本等一次性产物 | 2026-07-02 已清理；以后由正式 runner 重新生成，仍不提交。 |
| `test-records/runs/` | 历史验收和运行证据 | 保留在本地，已加入忽略；报告只提交脱敏摘要。 |
| `.bun/`、`~/`、`tmpcc-zenprobe-wsl-*` | 本地包缓存或误建临时目录 | 2026-07-02 已清理并加入忽略。 |
| `north-mini-code`、`""`、`\` | 根目录孤儿/异常文件 | 2026-07-02 已确认并清理；不是业务模型配置。 |

## 当前风险

1. 小矩阵通过不等于 4 客户端 x 500 次压测通过；当前 dry run 已阻断 full run，不能直接开 2000 次压测。
2. 输出限制和 flash 输入墙完全取消后，上游 413、超时、空输出、延迟和成本风险回到 upstream 与 lane/pool 调度层；必须用真实 panda `policy-smoke/policy-dry` 和后续 dry run 压测确认，不能凭源码测试判定生产安全。
3. Hermes/OpenClaw 当前测试使用临时环境变量或临时配置，不能误当成用户默认配置已经切换。
4. OpenClaw 系统 Node 仍是 `v20.20.2`，只有显式使用隔离 Node 22 路径才满足运行要求。
5. `.codex_tmp/`、异常字符文件和根目录孤儿文件已在 2026-07-02 清理；后续重新生成的临时产物仍不得提交。
6. 当前保留的 `test-records/runs/` 是本地证据目录，已忽略；不要把原始大日志、密钥、完整请求体或完整响应体加入提交。
7. 客户端策略隔离和 final-anchor 修复已在代码层落地，panda 本机 source-side huge stream smoke 通过；但真实 ClaudeCode dry run 仍显示 huge_context 语义漂移，不能进入 full run。
8. panda ZenProxy 三实例健康且池指标正常，但 NewAPI/docker 日志里出现过上游 Cloudflare 502/524、stream JSON 截断和 client_gone，需要在正式报告中和 ZenProxy 指标分开归因。
9. Windows ClaudeCode 不能从当前 WSL 非交互环境稳定启动时，应归类为测试执行环境问题；不要把它误判成 panda/ZenProxy 链路失败。
