# ClaudeCode 三模型稳定性与缓存交接

更新时间：2026-07-15

## 结论

- `deepseek-v4-flash`、`mimo-v2.5`、`hy3` 已通过 Windows official ClaudeCode -> CC Switch -> `sub2api.closeapi.top` -> NewAPI channel 69 -> Panda ZenProxy 的真实链路验收。
- 正式 189 项矩阵通过 `184/189`，工具执行识别 `185/189`；部署后针对故障项和高风险工具再跑 18 项，首轮 `16/18`，两个随机性失败各自重跑均通过。
- 部署后 NewAPI 专用 token 窗口为 `57/57` 成功消费、0 条错误；CC Switch 同窗口也是 `57/57` HTTP 200。
- Mimo 在本轮连续请求中请求级 cache-read 覆盖达到 `95.65%`。DeepSeek 为 `36.84%`，Hy3 为 `40.00%`；后两者没有达到 85%-95%，不能用会话粘性或伪造 usage 宣称达标。
- Hy3 不启用 Anthropic `cache_control` breakpoint：生产 canary 证明它会放大 forced-tool 参数不完整。Hy3 当前只保留稳定 session/project/request header 和 prompt cache key。

## 源码改动

提交：

- `a3fc5ca fix: stabilize ClaudeCode free-model routing`
- `2c29662 fix: keep hy3 cache markers out of tool payloads`
- `210f60c fix: adapt hy3 forced tools for ClaudeCode`

关键行为：

1. Hy3 使用 ClaudeCode profile；Hermes/OpenClaw 不再误套 ClaudeCode 高侵入策略。
2. Hy3 的 forced `tool_choice` 在上游降为 `auto`，避免上游返回半截工具参数；DeepSeek、Mimo、Big Pickle 保持原 forced-tool 语义。
3. Anthropic/OpenAI 非流式下游请求在上游强制使用 SSE，再聚合为非流式响应，修复 JSON 响应被当成 SSE 导致的截断/502。
4. 接受上游 `reasoning` 别名，缩短 no-forwardable watchdog 的实际等待，重试只保留在 ClaudeCode 路径。
5. provider 错误统一脱敏，Bearer token 和内部拓扑标签不会进入公开错误文本。
6. Mimo 保留 cache breakpoint；Hy3 禁用 breakpoint，但保留稳定的 session/project/request 身份材料。

本地验证：

- `free-model-client-rs`: 172 unit + 140 golden 全通过。
- `zen-proxy-rs`: 221 unit + 44 e2e 全通过。
- 两个项目 release build 均在本地完成；Panda 未执行 `cargo build`、`rustc` 或其它编译。

## 正式 189 项矩阵

| 模型 | 通过 | 工具执行 | 总耗时 P50 | P90 | P95 | P99/最大 |
|---|---:|---:|---:|---:|---:|---:|
| DeepSeek V4 Flash | 63/63 | 63/63 | 28.225s | 51.971s | 60.664s | 250.274s |
| Mimo V2.5 | 59/63 | 60/63 | 25.581s | 36.478s | 44.412s | 177.499s |
| Hy3 | 62/63 | 62/63 | 21.950s | 34.778s | 41.977s | 53.113s |
| 合计 | 184/189 | 185/189 | 24.244s | 41.977s | 51.971s | 177.499s / 250.274s |

五个失败中，四个是模型直接回答或 marker 缺失，一个是 Mimo 上游 Nginx HTTP 400。没有 ClaudeCode JSON 解析失败、工具参数 schema 错误或 ZenProxy 进程崩溃。

## 部署后定向回归

| 模型 | 首轮 | 覆盖 | 总耗时 P50 | P95 | 最大 |
|---|---:|---|---:|---:|---:|
| Hy3 | 5/6 | Bash、Grep、Task，text/stream-json | 26.738s | 39.239s | 42.306s |
| Mimo V2.5 | 6/6 | Glob、Grep include、WebSearch，text/stream-json | 28.277s | 88.204s | 106.859s |
| DeepSeek V4 Flash | 5/6 | Bash、Edit、Task，text/stream-json | 19.938s | 25.507s | 26.255s |

- Hy3 `Grep + stream-json` 首轮未调用工具，重跑通过，16.379s。
- DeepSeek `Edit + text` 首轮文件实际已修改但最终 marker 缺失，重跑通过，22.515s。
- Mimo 的 `Grep include + stream-json` 虽通过，但总耗时 106.859s，仍是需要持续观察的模型长尾。

## 部署后日志

时间窗：2026-07-15 21:20 后，专用 NewAPI token 897。

| 模型 | NewAPI 成功/错误 | NewAPI use_time P50/P95/最大 | CC Switch FRT P50/P95/最大 | cache-read 请求覆盖 |
|---|---:|---:|---:|---:|
| DeepSeek V4 Flash | 19/0 | 4s / 6s / 6s | 5.972s / 8.015s / 8.840s | 7/19, 36.84% |
| Mimo V2.5 | 23/0 | 6s / 13.8s / 16s | 7.309s / 15.748s / 19.578s | 22/23, 95.65% |
| Hy3 | 15/0 | 7s / 11.2s / 14s | 10.300s / 13.918s / 16.735s | 6/15, 40.00% |

CC Switch 的 `input_token_semantics=2`，因此这里报告请求级“是否出现 cache_read_tokens”，不把 `cache_read/(input+cache_read)` 当作真实 token 命中率。NewAPI `logs.other` 也没有足够的 provider cache miss 字段；严格 token 命中必须继续以 ZenProxy audit 的 read/miss 字段为准。

## 缓存判断

1. Mimo breakpoint 在稳定前缀连续请求中有效，本轮达到 95.65% 请求覆盖；仍要按 15-30 分钟和 24 小时窗口验证，不用 23 条样本替代稳态结论。
2. DeepSeek 上游缓存更偏向同一真实会话的后续轮次。`--no-session-persistence`、独立 ClaudeCode 进程或工具 schema/系统前缀变化都会形成冷启动；代理只能稳定身份材料，不能制造 provider 未返回的 cache hit。
3. Hy3 当前必须优先保证工具参数完整性。`cache_control` 与 forced tool 的组合已被实测否决；在 provider 修复前，不以牺牲工具质量换 85%-95%。
4. 后续缓存验收必须拆开：冷新会话请求覆盖、暖会话请求覆盖、token-weighted read pct、prefix 稳定度、工具质量和 TTFT。混成一个“缓存率”会误导优化方向。

## 生产部署

- 最终 Linux release SHA256：`1e5102df0d2f4ec9bd7cbb6fbae44134368ba48f1613a694df4becb6dfad41d7`。
- Panda `zen-proxy-rs@1/@2/@3` 均运行该 hash；`4000/4001/4002/4004` health 为 200。
- 发布路径：本地 release build -> GitHub 临时 release asset -> Panda 下载与 SHA256 校验 -> 单实例 canary -> 顺序滚动。
- 回滚锚点：`/opt/zen-proxy-rs/zen-proxy-rs.pre-claudecode-stability-20260715-212009`。
- 临时 GitHub draft release 和 3 个 asset、NewAPI token 897、CC Switch 临时 provider/备份、本地密钥均已删除。Panda 的 `-v2/-v3` 中间备份也已删除，只保留最早生产回滚锚点。

## 后续验收口径

1. 24 小时真实流量：HTTP/协议成功率 >= 99.5%，最终工具执行成功率 >= 99%，无持续 400/500/502/524 聚类。
2. 首字：按模型和 input bucket 报 P50/P90/P95/P99；不能只看 NewAPI use_time 或空协议首包。
3. 工具：Bash、Read、Write/Edit、Glob/Grep、Task/Agent、WebSearch/WebFetch 分开统计工具选择、参数完整、执行和最终 marker。
4. 缓存：分别报告冷会话、暖会话、请求级覆盖和 token-weighted read pct；目标只对稳定暖会话设 85%-95%，不把独立冷请求纳入同一硬门槛。
5. 质量红线：不裁剪上下文、不缩输出、不伪造 usage、不注入隐藏答案、不全局禁用 thinking、不为 cache hit 牺牲工具参数完整性。
