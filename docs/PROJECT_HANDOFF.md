# Project Handoff

更新时间：2026-07-04

## 项目定位

这个协同项目现在由一个总仓库承载两个真实子项目：

- `/home/lenovo/zen-free-model-suite/repos/free-model-client-rs`：协议适配内核，负责 OpenAI/Anthropic 请求转换、ClaudeCode 工具历史修复、stream guard、cache usage 透传、request-shape 观测。
- `/home/lenovo/zen-free-model-suite/repos/zen-proxy-rs`：生产代理控制面和数据面，负责模型公开/隐藏路由、proxy node pool、lane、dispatch、global budget、admin/health、NewAPI 后端服务。

统一入口是 `/home/lenovo/zen-free-model-suite`。两个子项目已经通过 `git subtree` 导入为真实目录，不再使用软链接。原 `/home/lenovo/free-model-client-rs` 与 `/home/lenovo/zen-proxy-rs` 暂时保留为备份/回滚点，不作为默认开发入口。

顶层暂不声明 Cargo workspace。继续分别在两个子项目目录运行 `cargo` 命令，避免结构迁移改变锁文件、依赖解析或生产构建路径。

## 当前链路

```text
ClaudeCode / compatible client
-> https://sub2api.closeapi.top
-> NewAPI channel 69
-> http://172.17.0.1:4000 on panda
-> zen-proxy-rs instances :4001/:4002/:4004
-> free-model-client-rs kernel
-> opencode/zen upstream through Webshare proxy nodes
```

本机 ClaudeCode 验收必须走：

```text
Windows claude.orig.exe
-> cc-switch 127.0.0.1:15721
-> https://sub2api.closeapi.top
-> NewAPI channel 69
-> ZenProxy on panda
```

不能用 Tailscale/panda 内网 URL 代替，否则不能代表普通用户。

## 当前生产事实

- channel 69 名称：`Zenproxyrs4.3`。
- channel 69 公开模型：`deepseek-v4-flash,big-pickle,mimo-v2.5`。
- `big-pickle` 已恢复为公开名；`deepseek-v4-flash-lite` 不再作为公开模型或 NewAPI mapping 暴露。
- panda ZenProxy 入口：nginx `:4000`，后端 `4001/4002/4004`。
- 新 Webshare 100 代理已替换到 panda，低并发验证 100/100 可访问上游，出口国家为 `SG`。
- 2026-07-04 12:22，panda 三个 `zen-proxy-rs@1/2/3` 均运行 sha256 `886344e54013386a8bc648286e79a862dccb2a06839abf3c0e0eb4c5a04b1977`，health OK；这是第二版 `tool_call_id` 稳定化修复，已证明仍不足以达成 95%+。
- 2026-07-04 12:55，DeepSeek 继续在同一 `session_id` 上重复 `empty_output`，audit 显示坏 node 被 `session_pin` 黏住；本地已补 `empty_output` 清 session pin，但尚未部署到 panda。
- 2026-07-04 09:55 严格窗口显示 `usk/prefix_32k_hash/prompt_cache_key` 已全量存在，但三模型 cache 仍未达到 95%+：DeepSeek R1/R2 约 11.40%，BigPickle R1/R2 约 15.17%，Mimo R1 约 87.70% 且 R2 受缺失 miss token 影响不能单独采用。
- NewAPI `logs.other` 当前没有 cache 字段，真实 cache 验收必须以 panda audit 的 `cache_read_input_tokens/cache_miss_input_tokens` 为准；cc-switch SQLite 只用于本地请求、模型和耗时对账。

## 已完成的关键工作

- 修复 ClaudeCode 工具参数完整性、坏文件路径、重复工具调用、`SendMessage.summary`、空 `query/command` 等问题。
- 支持 Anthropic `input_json_delta` 分片和 progressive tool streaming，降低工具首字延迟。
- 区分 ClaudeCode、Hermes、OpenClaw profile，避免跨客户端套错高侵入策略。
- 取消不合理输出限制：默认不补 `max_tokens`，显式值原样透传。
- 增加 stream idle ping、no-forwardable watchdog、有限重试和 provider 错误脱敏。
- 增加脱敏 request-shape、prefix hash、cache material bytes 和 cache 四态观测。
- 透传/合并真实 cache usage：`cache_read_input_tokens`、`cache_creation_input_tokens`、`prompt_cache_hit_tokens`、`prompt_cache_miss_tokens`。
- 稳定上游 session/project/request header，并剥离 ClaudeCode 动态 billing header，降低 prompt cache 前缀抖动。
- 排查并恢复旧 Webshare 代理失效导致的 502/proxy auth 问题。
- 通过 GitHub 临时 release 中转部署生产 ZenProxy，部署后删除 release/tag。
- 同步 NewAPI channel 69、models、abilities，恢复 `big-pickle` 公开名。
- 将 `/home/lenovo/zen-free-model-suite` 升级为真实 monorepo：删除聚合软链接，用 `git subtree` 导入 `free-model-client-rs` 和 `zen-proxy-rs` 历史，降低后续 agent 跨仓上下文损耗。

## 最近验收

2026-07-02 晚间，使用 Windows official `claude.orig.exe`，经 cc-switch 和 `sub2api.closeapi.top`：

- `deepseek-v4-flash`：Bash/WebFetch/WebSearch x text/json/stream-json，9/9 pass。
- `mimo-v2.5`：Bash/WebFetch/WebSearch x text/json/stream-json，9/9 pass。
- `big-pickle`：Bash/WebFetch/WebSearch stream-json，3/3 pass。

同窗口 NewAPI channel 69 只有成功消费记录；ZenProxy 未见 `unsupported V4 model`、`upstream_429`、`proxy authorization required`、`stream truncated`、`lane is saturated`。

## 重要解释

cc-switch 使用统计显示的是 `request_model -> provider/upstream model`，所以可能显示 `mimo-v2.5 -> mimo-v2.5-free`。NewAPI 使用日志显示 channel 69 收到的公开模型名，所以显示 `mimo-v2.5`。这是层级差异，不是路由错误。

Cloudflare 1010 的 A/B 结论：`Python-urllib/3.12` UA 会触发 403/1010；curl/no-UA/ClaudeCode-like/Mozilla UA 可到达 NewAPI 鉴权层。不要用 Python urllib 直连失败判断 ClaudeCode/cc-switch 被 Cloudflare 挡。

## 工程反思

这条链路最容易出错的是口径混淆：cc-switch、NewAPI、ZenProxy、provider 四层字段和耗时指标都不同。NewAPI FRT 不等于真实 ClaudeCode 首字；本地 prompt prefix 稳定也不等于远端 cache material 稳定。后续排障应继续坚持同一时间窗口四侧对账：ClaudeCode stream-json、cc-switch SQLite、NewAPI logs、ZenProxy journal/admin。

## 后续建议

1. cache/TTFT 优化先跑 20rpm 分桶校准，再考虑 50rpm。
2. 报告必须同时列 cache hit、TTFT/first_content/first_tool_call/first_tool_emit/total P50-P99、prefix 稳定度、工具质量和错误分类。
3. 不把 `--exclude-dynamic-system-prompt-sections` 设为默认；历史 A/B 在本链路降低 cache read pct 且增加 Web 工具错误。
4. Mimo cache 报告同时写 accepted/rejected 和 `read_tokens / estimated_total_tokens`，不要因缺 miss token 写成 100%。
5. 生产部署继续用 GitHub 临时 release，中转后删除 release/tag，不用 scp。
6. 后续新工作默认从 `/home/lenovo/zen-free-model-suite` 进入；只有需要回滚或对照历史时再读取两个旧路径。

## 生产资源红线

- panda 是生产机，禁止在 panda 上执行 `cargo build`、`rustc` 或任何高 CPU 编译任务；不要用 `nice/ionice` 作为例外。
- 生产更新只能使用已在本地或 CI 构建完成的产物，经 GitHub release/download 中转到 panda；panda 侧只做下载、hash 校验、替换、重启和 health/smoke。
- 如果 GitHub release asset 上传失败，必须暂停部署或改用 CI 构建 release asset；禁止退回到“下载 GitHub source tarball 后在 panda 编译”的兜底方案。
- 2026-07-04 曾因在 panda 上从 GitHub source 编译新提交导致 CPU 被打满并影响其他业务；该路径已列为禁止项。

## 2026-07-03 22:40 二次排障更新

- 用户截图显示 Claude Code 21:00 后累计缓存约 **45.3%**；NewAPI channel 69 在 21:58 连续 `mimo-v2.5` 502。
- panda 生产 hash `8817109b…` 下，21:40-22:00 deepseek 主流量 `session_pin_hit` 高但 `usk/prompt_cache_key` 覆盖低：`21:40-21:50` 74 行仅 6 行有 `usk`，`21:50-22:00` 20 行 0 行有 `usk`。
- 根因是 ClaudeCode 主路径走 Anthropic `/v1/messages`，旧 `zen-proxy-rs::resolve_session_identity()` 只按 OpenAI `ChatRequest` 解析，dispatch 前拿不到 USK；free-model-client 后续转换/注入的 `prompt_cache_key` 与 ZenProxy L3 pin 不同层，导致 L3 粘住但 L4 cache shard 不稳定。
- Mimo 502 根因不是上游真限流：audit `rate_limited=false`，journal 同窗连续 `fatal runtime error: stack overflow, aborting`。代码根因是 `dispatch_sticky()` fallback 调 `self.dispatch(meta)`，pinned node 忙时递归命中同一 session pin。
- 本轮本地修复：`messages` 路径 dispatch 前转换为 ChatRequest 计算 USK；FMC/ZenProxy 共用 api key cache id；sticky fallback 改为 `dispatch_without_session_pin()`，并修正 `session_pin_hit` 只在真实 pinned node 命中时为 true。
- 本轮本地验证：`free-model-client-rs cargo fmt/clippy/test` 通过；`zen-proxy-rs cargo fmt/clippy/test` 通过（205 unit + 44 e2e）。
- 下一步部署必须走 GitHub release/download，不走 scp；部署后只在新生产窗口达成 `usk/prefix_32k_hash/prompt_cache_key` 全量非空、无 stack overflow、三模型稳态 cache 达标时才能宣称 95%+。

## 2026-07-04 00:30 三次排障更新

- 00:08 后 NewAPI 全渠道未见 DeepSeek/Mimo 真实用户请求；截图里的 45.3% 和 21:58 Mimo 502 均属于旧窗口，不能当作最新生产二进制验收。
- 旧窗口 DeepSeek 真实低缓存成立：`2026-07-03 21:00-00:08` panda audit 中 125 行，成功 115 行，身份覆盖 `37/125`，provider R2 `62.68%`。
- 第二层根因：22:00 后身份覆盖已补齐，但同一 `prefix_32k_hash=a1a6c89803c073d6` 因 `tools_hash` 改变导致 USK 从 `usk_v1:7234...` 变成 `usk_v1:6d6f...`，23:16 出现同前缀冷启动 `cache_miss_input_tokens=50039`。
- 本轮本地修复：长上下文 `icp_scope` 改为 `icp:p32k:{prefix_32k_hash}`，`tools_epoch_id` 保留为观测/冻结信号但不再进入 provider `prompt_cache_key`；同前缀工具变化不再切 affinity/cache key，真实前缀变化仍切 key。
- 本轮验证：WSL 下 `free-model-client-rs` fmt/clippy/test 通过（142 unit + 136 kernel golden）；`zen-proxy-rs` fmt/clippy/test 通过（205 unit + 44 e2e）。
- 后续生产 hash `41afc...a32` 已运行，但 09:55 严格窗口证明仅有 prefix-scope USK 仍不够，不能宣称三模型 95%+。

## 2026-07-04 10:20 四次排障更新

- WSL ClaudeCode 经 cc-switch provider-specific 双轮测试三模型均能请求成功；此前 WSL `403 无权访问 kiro 分组` 的根因是 cc-switch provider/live backup 使用了 `kiro` group token，已统一为 `hhhl` group token。
- OpenCode 的 Mimo 应使用 `opencode/mimo-v2.5-free`；`opencode-go/mimo-v2.5` 报 401/Missing API key，属于 OpenCode Go credential 问题，不是 NewAPI/ZenProxy/cache 问题。
- 严格 cache 根因继续推进：ClaudeCode 工具链中 59KB 左右、含约 12KB `tool_result` 的请求会让 `prefix_32k_hash/USK/session_id/node` 分裂；4K 稳定请求能命中，但中等工具请求继续冷启动。
- 本轮本地修复：`free-model-client-rs/src/protocol/translate.rs` 在 cache identity 材料中标准化 `role=tool` 的动态工具结果内容，避免工具输出污染 `prefix_32k_hash`；完整 `prompt_hash` 仍反映真实内容变化。
- 本轮验证：`free-model-client-rs cargo fmt --check`、`cargo test`（143 unit + 136 kernel golden）、`cargo clippy --all-targets -- -D warnings` 通过；`zen-proxy-rs cargo test affinity_key_uses_stable_prefix_scope` 通过。
- 尚未部署该 tool_result 修复到 panda。生产更新仍必须走 GitHub release/download，不走 scp；部署后用 Windows/WSL ClaudeCode + cc-switch + NewAPI + panda audit 新窗口重新验收。

## 2026-07-04 12:15 五次排障更新

- 第一版 `tool_result` 修复已按要求通过 GitHub release asset 下载部署到 panda，运行 hash `171e7a3c21b1da1c1d655a5442b8c17396299d9b4d63af273f5698328ad11358`，三实例 health OK，临时 token 已清理。
- 部署后 WSL ClaudeCode 真实工具任务仍失败：DeepSeek 第 1 轮 360s timeout；ClaudeCode JSONL 在 3 个 `Read` tool result 后连续 `api_retry`。
- NewAPI 同窗口显示短请求可成功且第二次有 `cache_tokens=3584`，但 45k prompt 工具请求均为 `stream_status.end_reason=client_gone`、`end_error=context canceled`、`cache_tokens=0`，FRT 约 60-72s。
- panda audit 显示 45k 工具请求同一轮 retry 的 prefix/USK 稳定但全 miss；第二轮相同任务的 45k prefix 又变，说明还存在动态字段污染 cache identity 和真实 upstream body。
- 新根因：ClaudeCode 动态 `tool_use_id` 经转换后进入 assistant `tool_calls[].id`，第一版只处理 `role=tool` 内容，没有稳定 assistant tool id，也没有让真实转发 body 稳定。
- 本轮本地修复：`canonicalize_openai_tool_history_with_policy()` 配对完成后把现有 tool id 重写为稳定 `call_fmc_*`，并同步 tool result；`request_cache_material()` 使用同一稳定 id 且忽略 `reasoning_content`。
- 本轮验证：`free-model-client-rs` fmt/test/clippy 通过（145 unit + 136 kernel golden）；`zen-proxy-rs` fmt/test/clippy 通过（205 unit + 44 e2e）。
- 该第二版修复尚未部署到 panda。下一步仍必须走 GitHub release/download，不走 scp；部署后重新做严格 cache 验收。不能把 171e 部署窗口写成 95%+ 成功。

## 2026-07-04 12:55 六次排障更新

- 第二版 `tool_call_id` 稳定化修复已通过 GitHub release asset 下载部署到 panda，运行 hash `886344e54013386a8bc648286e79a862dccb2a06839abf3c0e0eb4c5a04b1977`，三实例 health OK。
- 部署后 WSL ClaudeCode 的 DeepSeek 非工具和工具请求仍连续返回 `500 server_error`，audit 指向同一 `session_id=ses_afd18e9f402311f2`、同一 node `97a7d2b4`、多次 `outcome=empty_output` 且 `session_pin_hit=true`。
- 根因推进为：`empty_output` 会记录节点失败，但旧代码没有清理 Redis/session pin；ClaudeCode retry 被稳定路由到同一坏节点，导致 500/empty_output 放大，cache_read=0 不能代表稳定缓存能力。
- 本轮本地修复：`session_pin::clear(upstream_model, session_id)` 同时清 Redis pin 和内存 fallback；非流式与流式 `empty_output` 分支都会清 pin，让下一轮可换节点。
- 本轮验证：`zen-proxy-rs cargo fmt --check`、新增 pin 清理单测、完整 `cargo test`（206 unit + 44 e2e）、`cargo clippy --all-targets -- -D warnings` 均通过。
- 该 v2.6 修复尚未部署到 panda。部署后第一验收目标不是直接宣称 95%+，而是确认 DeepSeek 不再连续命中同一 empty_output pin；随后再跑三模型 cache 新窗口。
