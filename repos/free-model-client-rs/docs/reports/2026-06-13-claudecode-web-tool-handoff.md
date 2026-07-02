# 2026-06-13 ClaudeCode Web/Tool 调查交接

## 状态

本轮按用户要求停止继续测试和修复，只做文档交接。

没有部署、没有修改 NewAPI、ClaudeCode、cc-switch，也没有继续改 ZenProxy/free-model-client-rs 运行逻辑。

当前仓库分支：

```text
codex/v47-client-split-cache-harness
```

除本轮交接文档改动外，工作区还存在一个既有未跟踪项：

```text
?? north-mini-code
```

`north-mini-code` 是未跟踪项，本轮未触碰。后续不要盲删，也不要混入提交。

2026-07-02 补充：该根目录孤儿文件已确认是残缺 SQL 片段并已清理；本报告其余链路和 Web/Tool 结论仍按 2026-06-13 当时事实保留。

## 本轮真实测试入口

Windows ClaudeCode 实际入口不是直接官方二进制，而是 PowerShell profile 中的 `claude` 函数：

```text
claude -> Invoke-ClaudeCode -> C:\Users\Lenovo\.local\bin\clawgod.cmd
clawgod.cmd -> bun -> C:\Users\Lenovo\.clawgod\cli.cjs
```

同时用官方原始入口做了对照：

```text
C:\Users\Lenovo\.local\bin\claude.orig.cmd
```

两者版本一致：

```text
2.1.139 (Claude Code)
```

## 当前 cc-switch 事实

本轮检查时，Windows cc-switch 当前 Claude provider 是：

```text
provider_id = close-1778594644123
name        = closedeepseek
base URL    = https://sub2api.closeapi.top
model       = deepseek-v4-flash
mapped      = deepseek-v4-flash-free
```

这不是本机 `127.0.0.1:8081`，也不是 panda NewAPI `http://100.69.228.93:8081` 的本地直连配置。

因此：

```text
cc-switch 本地日志 != panda NewAPI channel 69 日志
```

不能把这两边的数据直接合并归因。后续接手必须先确认当前 provider，再判断是否经过 ZenProxy。

## 真实测试样本

测试产物保存在本机临时目录：

```text
C:\Users\Lenovo\AppData\Local\Temp\zen_cli_probe_20260612_005541
C:\Users\Lenovo\AppData\Local\Temp\zen_cli_probe_official_20260612_010110
C:\Users\Lenovo\AppData\Local\Temp\zen_cli_web_batch_20260612_010559
```

这些是临时测试输出，不应提交。

### 基础工具链

用例：

```text
Bash -> Write -> Read
```

结果：

```text
通过
```

证据：

- `Bash` 参数完整：`echo ZEN_BASIC_OK`
- `Write` 参数完整：`file_path` 和 `content` 都存在
- `Read` 参数完整：`file_path` 存在
- 最终返回：`{"bash_ok":true,"read_ok":true}`

结论：

```text
本轮没有复现“ZenProxy/free-model-client-rs 稳定清空 ClaudeCode 工具参数”的证据。
```

### WebSearch/WebFetch 组合

用例：

```text
ToolSearch -> WebSearch -> WebFetch
```

结果：

```text
ToolSearch 参数正确
WebSearch 参数正确，但 results 为空
WebFetch 参数正确，但 ClaudeCode 本地安全校验失败
```

WebFetch 精确错误：

```text
Unable to verify if domain example.com is safe to fetch. This may be due to network restrictions or enterprise security policies blocking claude.ai.
```

参数层事实：

```json
{"name":"ToolSearch","input":{"query":"WebSearch WebFetch"}}
{"name":"WebSearch","input":{"query":"OpenAI prompt caching documentation"}}
{"name":"WebFetch","input":{"url":"https://example.com","prompt":"Return only the page title or heading"}}
```

这说明本轮 WebFetch 失败不是因为：

```text
缺 prompt
缺 url
工具名大小写错
ZenProxy 把参数清空
```

### 官方入口对照

用 `claude.orig.cmd` 跑同样 Web 用例，WebFetch 仍然失败，错误同样指向 `claude.ai` 安全验证链路。

结论：

```text
WebFetch 失败不是 clawgod wrapper 单独导致。
```

### Playwright fallback

同样页面 `https://example.com` 和 Anthropic/Claude Code 文档页面，WebFetch 失败后模型改用 Playwright 可以抓到内容。

这说明：

```text
网页访问本身可用
ClaudeCode 内置 WebFetch 的安全验证/后端链路不可用
```

## 网络和服务观察

本机探测：

```text
https://example.com       -> 200
https://code.claude.com   -> 200
https://claude.ai         -> 500
https://docs.anthropic.com -> 500
https://api.anthropic.com -> 403
```

panda 探测：

```text
https://claude.ai         -> Cloudflare challenge 403
https://docs.anthropic.com -> 301
https://example.com       -> 200
```

这和 WebFetch 错误中的 `blocking claude.ai` 一致。

## cc-switch 数据摘要

最近约 2 小时 cc-switch Claude 记录：

```text
总数: 410
200: 285
502: 125
```

成功请求首 token：

```text
P50: 15.6s
P90: 48.6s
P95: 69.4s
P99: 99.6s
max: 115.1s
```

成功请求总耗时：

```text
P50: 20.2s
P90: 54.2s
P95: 82.3s
P99: 122.1s
max: 138.3s
```

该窗口内 502 主要为：

```text
client error (Connect)
client error (SendRequest)
error sending request for url (https://sub2api.closeapi.top/v1/responses)
```

这类更像上游/网络/Cloudflare/连接层异常，不是工具参数 JSON 本身。

## parse JSON 结论

本轮精确查 cc-switch SQLite：

```text
error_message like '%parse%' -> 0
error_message like '%JSON%'  -> 0
```

但 cc-switch 文本日志中确实看到：

```text
流错误: error reading a body from connection
Response parse failed: connection error
```

判断：

```text
用户看到的 API Error: Failed to parse JSON，很可能是上游连接中断、返回非 JSON HTML、Cloudflare 502/524 页面、或半截流导致客户端解析失败。
```

后续不能直接归因为：

```text
30KB Write 参数过大
ZenProxy 清空工具参数
ClaudeCode 自身坏了
```

必须用同一时间窗口的 raw SSE、cc-switch proxy_request_logs、NewAPI logs 和 ZenProxy journal 对齐。

## panda / ZenProxy 状态观察

panda 三实例当时为 active：

```text
zen-proxy-rs@1 active
zen-proxy-rs@2 active
zen-proxy-rs@3 active
```

`http://127.0.0.1:4000/health` 显示：

```json
{"status":"ok","pools":{"total":90,"dispatch":90,"dead":0,"ratelimited":0}}
```

这说明当时不是“panda ZenProxy 没起”或“节点全死”。

但测试窗口里夹杂了用户/其他客户端大长会话请求，不能把所有 panda 日志都算进本轮测试样本。

本轮自己的 deepseek-v4-flash 小工具请求在 cc-switch/NewAPI 对账中没有 502，首 token 约 4.3s 到 18.9s。

## 已确认问题

### P0: WebFetch 原生工具不可用

现象：

```text
WebFetch 参数完整，但 ClaudeCode 本地安全验证失败。
```

当前判断：

```text
不是 ZenProxy 参数转换问题，而是 ClaudeCode 内置 WebFetch 依赖的 claude.ai 校验/安全链路不可用。
```

后续可选方向：

1. 确认当前网络/代理/Clash 节点是否能稳定访问 `claude.ai`，且不被 Cloudflare challenge。
2. 比较官方 API/官方 ClaudeCode 直连在相同网络下 WebFetch 是否可用。
3. 如果必须源头侧适配，只能考虑把模型更稳定地引导到客户端已有替代工具，例如 Playwright/MCP web 工具；但这属于行为适配，不应伪装成原生 WebFetch 修好了。

### P0: WebSearch 返回空结果

现象：

```text
WebSearch 参数正确，但 tool_result results=[]
server_tool_use.web_search_requests=0
```

当前判断：

```text
这不是上游模型 server-side search，而是 ClaudeCode 内置搜索后端没有拿到真实结果。
```

后续要分清：

```text
ClaudeCode 内置 WebSearch
MCP/Playwright/browser 搜索
模型服务商原生联网搜索
```

这三者不是同一个能力。

### P0: 502/连接错误多

现象：

```text
cc-switch 最近约 2 小时 410 条 Claude 请求中 125 条 502。
```

主要错误：

```text
Connect
SendRequest
error sending request for url
Cloudflare/HTML 502/524 body
```

当前判断：

```text
主要是 sub2api.closeapi.top / Cloudflare / 网络连接层异常，不是单纯 ZenProxy 工具协议错误。
```

后续需要按 provider 拆分：

```text
cc-switch -> sub2api.closeapi.top
panda NewAPI channel 69 -> ZenProxy -> upstream
ZenProxy 直连 4000
```

### P1: 真实首字仍慢

现象：

```text
cc-switch 口径首 token P50 15.6s，P90 48.6s。
```

原因候选：

1. cache miss / cache warm-up。
2. sub2api/Cloudflare 连接层抖动。
3. ClaudeCode 本地工具循环和 fallback，尤其 WebFetch 失败后绕 Playwright。
4. 上游 reasoning-only / no-forwardable retry。
5. 大上下文工具历史导致模型先思考很久才给可转发内容。

本轮未继续优化。

### P1: JSON 格式和听指令问题仍存在

本轮 Web 测试里出现过模型最终输出非严格 JSON：

```text
{[toolsearch_ok:true,websearch_ok:true,...]}
```

这属于模型输出质量/约束服从问题，不是工具参数传输失败。

后续如果要优化，必须和“工具 JSON 参数完整性”分开验收。

## 后续接手建议

### 如果继续修 WebSearch/WebFetch

先做这 4 个最小验证：

1. 官方 ClaudeCode + 官方 Anthropic API + 当前网络：WebFetch 是否可用。
2. 官方 ClaudeCode + deepseek-v4-flash + sub2api：WebFetch 是否可用。
3. 官方 ClaudeCode + deepseek-v4-flash + panda NewAPI channel 69：WebFetch 是否可用。
4. Playwright/MCP web 工具是否稳定可用。

只有第 1 条可用、第 2/3 条不可用时，才继续查 ZenProxy/free-model-client-rs 适配。

如果第 1 条也不可用，优先查本机网络、Clash、Cloudflare challenge、ClaudeCode 内置 Web 工具依赖。

### 如果继续查 Failed to parse JSON

必须拿同一时间点的 4 份证据：

1. ClaudeCode stream-json 原始输出。
2. cc-switch `proxy_request_logs` 对应 request_id。
3. panda NewAPI logs 对应 request_id/channel。
4. ZenProxy journal 的 `prompt_hash_hex`、`final_stream_error`、`first_content_ms`、`first_tool_call_ms`、`attempts_used`。

没有这些证据前，不要先改代码。

### 如果继续查工具参数缺失

先看 raw stream-json 中 assistant 的 `tool_use.input`。

判断顺序：

```text
模型真的没生成参数
free-model-client-rs 没等完整 JSON 就下发
ZenProxy/NewAPI/SSE 中间层截断
ClaudeCode 本地工具 schema 和模型生成不匹配
客户端/插件 hook 拦截
```

V4.102/V4.104 之后，理论上不应再把空 `{}` 或缺必填字段的工具调用直接下发给 ClaudeCode。如果复发，抓 raw SSE 后再修。

## 不要做的事

1. 不要把 cc-switch 当前 `sub2api.closeapi.top` 数据直接说成 panda channel 69 数据。
2. 不要用 NewAPI FRT 替代 cc-switch/ClaudeCode 真实首字体验。
3. 不要把 WebFetch 失败简单说成 ZenProxy 工具名映射错。
4. 不要为了 WebFetch 伪造工具结果。
5. 不要为了降低首字恢复输入裁剪、输出 cap、全局 disabled thinking 或隐藏提示词。
6. 不要把临时测试目录、密钥、JSONL 大输出提交进仓库。

## 当前建议停止点

本轮最清楚的结论是：

```text
基础工具链正常。
WebSearch/WebFetch 失败点在 ClaudeCode 内置 Web 工具后端/安全验证/网络链路，不是当前样本中的 ZenProxy 参数清空。
502/Failed to parse JSON 更像连接层/Cloudflare/半截流问题，需要按 provider 和 request_id 精确对齐。
```

下一位接手者应先按上面的证据链复现，不要直接继续堆适配。
