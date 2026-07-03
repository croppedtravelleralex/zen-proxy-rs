# Project Handoff

更新时间：2026-07-02

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
- 尚未部署该 prefix-scope USK 修复到 panda；下一步仍必须走 GitHub release/download，部署后用 Windows/WSL ClaudeCode + ccswitch + NewAPI 新窗口验收三模型 95%+。
