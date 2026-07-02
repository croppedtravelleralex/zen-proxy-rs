# 2026-07-02 Cross-Repo Suite Handoff

## WSL Dev 入口

统一入口：

```text
/home/lenovo/zen-free-model-suite
```

该目录软链接聚合：

- `/home/lenovo/free-model-client-rs`
- `/home/lenovo/zen-proxy-rs`
- `/tmp/claudecode-ccswitch-smoke-runs`

真实 git 仓库未移动，避免破坏 cargo、systemd、nginx、部署脚本和历史文档路径。

## 当前生产链路

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

不能用 Tailscale/panda 内网 URL 代替。

## 当前模型边界

生产 channel 69 公开模型只应是：

```text
deepseek-v4-flash
big-pickle
mimo-v2.5
```

`deepseek-v4-flash-lite` 已撤下公开名；`north-mini-code`、`nemotron-3-ultra`、`minimax-m3`、`qwen3.6-plus` 只做 hidden routing。

## 已完成事项

- 修复 ClaudeCode 工具参数完整性、坏文件路径、重复工具调用、空 `query/command`、`SendMessage.summary` 等问题。
- 支持 Anthropic `input_json_delta` 分片和 progressive tool streaming，降低工具首字延迟。
- 区分 ClaudeCode、Hermes、OpenClaw profile，避免跨客户端套错兼容策略。
- 取消默认输出限制：缺省不自动补 `max_tokens`，显式值原样透传。
- 增加 stream idle ping、no-forwardable watchdog、有限重试和 provider 错误脱敏。
- 增加脱敏 request-shape、prefix hash、cache material bytes 和 cache 四态观测。
- 透传/合并真实 cache usage，包括 DeepSeek `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens`。
- 稳定上游 session/project/request header，并剥离 ClaudeCode 动态 billing header，降低 prompt cache 前缀抖动。
- 更换失效 Webshare 代理池，恢复生产 channel 69。
- 通过 GitHub 临时 release 中转部署生产 ZenProxy，部署后删除 release/tag。
- 同步 NewAPI channel 69、models、abilities，恢复 `big-pickle` 公开名。
- 建立 `/home/lenovo/zen-free-model-suite` 跨仓聚合入口和本地交接文档。
- 清理 `.codex_tmp/`、`.bun/`、`tmp/`、异常根目录孤儿文件和误跟踪构建产物。

## 最近验收

2026-07-02 晚间，使用 Windows official `claude.orig.exe`，经 cc-switch 和 `sub2api.closeapi.top`：

- `deepseek-v4-flash`：Bash/WebFetch/WebSearch x text/json/stream-json，9/9 pass。
- `mimo-v2.5`：Bash/WebFetch/WebSearch x text/json/stream-json，9/9 pass。
- `big-pickle`：Bash/WebFetch/WebSearch stream-json，3/3 pass。

同窗口 NewAPI channel 69 只有成功消费记录；ZenProxy 未见 `unsupported V4 model`、`upstream_429`、`proxy authorization required`、`stream truncated`、`lane is saturated`。

## 工程反思

这条链路最容易出错的是口径混淆：cc-switch、NewAPI、ZenProxy、provider 四层字段和耗时指标都不同。NewAPI FRT 不等于真实 ClaudeCode 首字；本地 prompt prefix 稳定也不等于远端 cache material 稳定。后续排障应坚持同一时间窗口四侧对账：ClaudeCode stream-json、cc-switch SQLite、NewAPI logs、ZenProxy journal/admin。

## 自我约束

- 不泄露 key/token/proxy 凭据。
- 不裁剪上下文换速度。
- 不伪造 cache usage。
- 不牺牲工具调用质量。
- 不禁用 Bash/WebFetch/WebSearch 换稳定。
- 不通过降智、缩输出、隐藏提示词或全局 disabled thinking 换首字。
- 不把 Tailscale/panda 内网链路当成普通用户可交付链路。
- 不回滚未确认的用户改动或历史 dirty changes。

## 建议

1. cache/TTFT 优化先跑 20rpm 分桶校准，再考虑 50rpm。
2. 报告必须同时列 cache hit、TTFT/first_content/first_tool_call/first_tool_emit/total P50-P99、prefix 稳定度、工具质量和错误分类。
3. 不把 `--exclude-dynamic-system-prompt-sections` 设为默认；历史 A/B 在本链路降低 cache read pct 且增加 Web 工具错误。
4. Mimo cache 报告同时写 accepted/rejected 和 `read_tokens / estimated_total_tokens`，不要因缺 miss token 写成 100%。
5. 生产部署继续用 GitHub 临时 release，中转后删除 release/tag，不用 scp。

