# AI 维护手册

## 每轮开始

1. 先读 `docs/README.md`。
2. 再读 `docs/02-current-state.md`、`docs/03-roadmap.md`、`docs/04-improvement-backlog.md`。
3. 如果要改代码，读相关模块，不全仓漫扫。
4. 如果文档和代码冲突，以代码、配置、测试和真实命令结果为准，并在同一轮修正文档。

## 当前项目边界

- 本仓库负责 Rust 反代适配内核。
- NewAPI 管理端、panda 服务器渠道、closeapi 直连状态不是本仓库代码事实，必须通过运行探测或远端日志确认。
- 不要把 `panda NewAPI -> configured upstream` 擅自写成具体渠道名，除非有证据。

## 验证纪律

本仓库代码改动后，最低验证命令：

```bash
cd /home/lenovo/free-model-client-rs
CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/free-model-client-rs-target cargo fmt -- --check
CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/free-model-client-rs-target cargo clippy --all-targets -- -D warnings
CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/free-model-client-rs-target cargo test
```

注意：从 Windows UNC 路径直接跑 cargo 可能出现增量编译锁文件错误。优先在 WSL 原生路径 `/home/lenovo/free-model-client-rs` 下执行，并设置 `CARGO_INCREMENTAL=0` 和 WSL 本地 `CARGO_TARGET_DIR`。

## panda NewAPI 测试纪律

- base URL 使用 `http://100.69.228.93:8081`，除非用户更新。
- API key 在报告中必须脱敏。
- 不要把 token 名当作 API key；例如 NewAPI 日志里的 `token_name=ds` 不是可公开复用的明文 key。
- channel 69 属于 `vip` 组；用 default 组或已删除 token 测出来的 401/403/`No available channel` 不能证明 ZenProxy 不可用。
- `sk-dev` 已是历史失效 token，不再作为 panda channel 69 验收凭据。
- 测试前清空或禁用代理环境变量，避免走错链路。
- 报告必须列状态码、耗时、模型、协议类型、错误分类。
- 不要用本机 `127.0.0.1:8081` 代替 panda。

## 客户端策略纪律

- ClaudeCode、Hermes、OpenClaw 不应再共享一套高侵入兼容策略。
- 90 分客户端识别和策略隔离已实现；下一步要用真实 ClaudeCode/Hermes/OpenClaw 小矩阵验证效果。
- 显式识别头为 `x-fmc-client`，优先级高于 User-Agent 和请求体推断。
- ClaudeCode 默认策略：不因 tools 禁用 thinking，保留流式空白 delta，只做硬协议工具历史修复。
- Hermes/OpenClaw 默认策略：保留协议兼容修复，但任何补齐或降级都必须记录 profile 和 repair count。
- unknown 默认策略：不禁 thinking，只做最小协议修复。
- 新增或调整客户端 profile 时，必须补 profile 维度测试，至少覆盖 thinking、stream whitespace、tool history 三类策略。
- 当前 OpenClaw 自动识别必须优先看强 body marker 和 OpenClaw 专属工具集，再看 ClaudeCode 共用工具名；`read/write/edit` 这类共用工具不能把 OpenClaw 误识别成 ClaudeCode，`web_fetch`/`web_search` 也不能单独把 ClaudeCode 误识别成 OpenClaw。
- 普通用户正文里提到 OpenClaw/Hermes 不是客户端身份信号；只有 `running inside openclaw/hermes`、`openclaw cli/agent`、`hermes cli/agent` 等强 marker 才能作为 body marker。

## Hermes/OpenClaw 测试纪律

- 优先临时配置，不永久改用户默认配置。
- 若必须改配置，先备份，测试后恢复或明确记录。
- OpenClaw Node 版本不满足时，用隔离 Node，不覆盖系统 Node。
- 测试只覆盖授权和防御性能力，不执行真实公网攻击、凭据窃取、持久化、绕过检测、武器化 payload。

当前已验证的临时接入方式：

- Hermes：用 `--provider custom`，并通过临时环境变量 `CUSTOM_BASE_URL=http://100.69.228.93:8081/v1`、`OPENAI_API_KEY=<redacted>` 指向 panda；不要写入用户默认 `~/.hermes/config.yaml`。
- OpenClaw：用 `OPENCLAW_CONFIG_PATH=.codex_tmp/openclaw-panda/openclaw.json`、`OPENCLAW_STATE_DIR=.codex_tmp/openclaw-panda/state` 和隔离 Node `~/.local/opt/node-v22.21.1-linux-x64/bin/node`；不要覆盖系统 Node。
- panda 压测：报告中只写 `sk-***`，脚本从环境变量读取 key，不把 key 写入仓库。

## panda-only 压测目标

下一阶段正式压测只走 panda NewAPI：

```text
client -> panda NewAPI http://100.69.228.93:8081 -> configured upstream
```

客户端矩阵：

1. Windows ClaudeCode：500 次混合压力测试。
2. WSL ClaudeCode：500 次混合压力测试。
3. WSL Hermes：500 次混合压力测试。
4. WSL OpenClaw：500 次混合压力测试。

正式执行器：

```bash
cd /home/lenovo/free-model-client-rs
PANDA_NEWAPI_KEY=<redacted> python3 scripts/panda_pressure_runner.py --mode smoke
```

执行纪律：

- Windows ClaudeCode 必须从 Windows Python/PowerShell 启动同一个 runner，不要从当前 WSL 会话调用 Windows interop。
- dry run 没通过前不启动 full run；2026-05-31 历史 dry run 曾暴露 huge_context、OpenClaw subagent 和上游 overload 红旗。
- 2026-05-31 晚间 OpenClaw subagent 的 profile 误识别已修复，OpenClaw-only smoke 5/5、WSL 三客户端 smoke 15/15；但仍需重新跑 dry run 后才能进入 full run。
- 2026-06-01 已在 panda 部署 ClaudeCode huge_context final-anchor 修复；panda 本机 `/v1/messages` 约 1.0MB source-side smoke 中 flash 3/3、lite 3/3 均返回 `HUGE_OK`，但这不是四客户端真实 dry run。
- 2026-06-03 已在 panda 部署 channel 69 健康测试误判修复；空内容无工具探测应短路为 `ok`，小 `max_tokens` 请求不得进入 ClaudeCode huge buffered retry。
- 2026-06-03 已补管理端测渠道第二层兜底：`echo hi`/`hi`/`hello`/`test` 类极短流式无工具探测，只有在上游空输出时才降级为本地 `ok`；普通请求不应提前短路。
- 2026-06-03 晚间已部署源码层非流式第二层兜底：同类极短非流式无工具探测在上游连续空输出后返回本地 `ok`；普通请求仍返回结构化空输出错误。
- 2026-06-03 晚间已部署 ClaudeCode 格式误伤修复：`web_fetch`/`web_search` 和普通 OpenClaw/Hermes 文本引用不再把请求判为 OpenClaw；受控 `Task + web_fetch` `/v1/messages` 新 pid 日志为 `source_client=claude-code`。
- panda 当前没有 Rust 工具链；上线源码补丁时优先在本机/WSL 构建 Linux release，再上传 strip 后二进制，不要在生产机上临时高负载编译。
- Windows `ssh panda` 使用 `C:\Users\Lenovo\.ssh\config` 中的 `root@100.69.228.93`；WSL 默认 `ssh panda` 可能没有该配置。WSL 需要显式使用 `/mnt/c/Users/Lenovo/.ssh/id_ed25519`。
- huge stream 日志里若出现 `ClaudeCode huge stream buffered upstream returned empty output`，先按 buffered retry 已兜底处理归类；只有最终裸透给客户端或耗尽重试才算失败。
- 如果需要临时中止压测，保留已有 `raw-results.jsonl`，再生成 partial summary，不要补写伪造的完成数。
- WSL ClaudeCode 当前不能直接作为有效客户端样本：`/home/lenovo/.local/bin/claude` 和 `claude-deepseek-free` 是 clawgod launcher，实际启动 `/root/.clawgod/cli.cjs`，不是 Anthropic ClaudeCode CLI；正式四客户端压测前必须修复或替换。
- Windows ClaudeCode 经过 cc-switch 时必须先确认 current provider。2026-06-04 发现 current Claude provider 是 `closedeepseek -> https://sub2api.closeapi.top`，不是 `LocalNewapi -> http://127.0.0.1:8081`；普通 Windows 使用记录不能直接归因到 panda NewAPI/ZenProxy。
- OpenClaw 若输出固定 `HEARTBEAT_OK` 且 stderr 有 local secrets gateway `1006 abnormal closure`，先归类为 OpenClaw 本地 agent/gateway/harness 问题，不要先改 ZenProxy。

## 常见现象解释

- prompt tokens 稳定在约 60k：默认流式 compactor 在超过 80k 后压到约 60k；如果 ClaudeCode 被误判为 OpenClaw，就不会走 ClaudeCode huge-context 约 12k 目标。
- NewAPI cache tokens 几乎为 0：当前链路只转发上游 usage 里的缓存字段；上游不返回 `cache_creation_input_tokens`、`cache_read_input_tokens` 或 `cached_tokens` 时，ZenProxy 不自行制造缓存计数。
- ClaudeCode CLI Markdown/表格/代码块/列表显示异常：先抓 raw SSE 和 `source_client`。若新 pid 仍不是 `claude-code`，优先查 profile 识别；若 raw SSE 正确但终端显示错，再归类为 CLI 渲染问题。
- NewAPI 偶发 `status_code=500, upstream returned no assistant content or tool call`：先按空上游保护排查，不要先改 NewAPI。若样本是 ClaudeCode 大流式请求且 `max_tokens` 被 cap 到 768/1024，重点检查是否绕过了 ClaudeCode huge buffered retry；对齐后再决定是否扩大 buffered retry 覆盖。
- Web search 用不了：先分清“模型原生联网搜索”和“客户端工具搜索”。本仓库不自带搜索引擎，只转发 tools/tool_calls/tool results。排查顺序是：请求是否带 `WebSearch/WebFetch` 或 `web_search/web_fetch` 工具定义、模型是否发出 tool call、ZenProxy 是否把上游工具名 canonicalize 回客户端注册名、客户端/工具执行器是否执行联网、工具结果是否回到模型上下文。Hermes/OpenClaw/ClaudeCode 要分开验收。2026-06-04 已确认直连 panda NewAPI 带 `web_search` tool 能返回 tool call；用户截图也证明 Windows ClaudeCode 官方 Claude 路径可真实执行 `WebSearch/WebFetch`。不要再把“某次 ZenProxy 样本没有执行 web 工具”写成 ClaudeCode 不支持 web 工具。
- ClaudeCode 工具名大小写/别名：ClaudeCode 注册的是 `WebSearch`、`WebFetch`、`Task` 等真实工具名；如果上游返回 `web_search`、`web_fetch`、`task`，客户端可能不执行。free-model-client-rs 已补 canonicalization 回归，并已部署到 panda stripped hash `0f6cdf6e5cd2dd1946a69707c97591cca865b47178ff63846f04bbdf283f2314`；线上直连 smoke 已验证 `WebSearch` 和 `Task` 工具名。
- NewAPI 看到 70k-90k input tokens：不要直接判定为 NewAPI 输入墙。先对齐三种口径：ZenProxy `body_size` 是 JSON 字节数；free-model-client-rs `prompt_tokens` 是估算/策略口径；NewAPI/cc-switch usage 是最终账单口径。若日志出现 `compacted streaming ... before_tokens=... after_tokens=...`，说明是内核消息压缩后的上游输入；若 `context_action=pass` 且只有 `capped streaming ... max_tokens`，说明输入未被外层裁剪，只限制了输出。
- Windows ClaudeCode 当前可能先走 cc-switch：检查 `C:\Users\Lenovo\.claude\settings.json` 里的 `ANTHROPIC_BASE_URL`，再查 cc-switch provider。不要把 `ClaudeCode -> cc-switch -> closeapi` 的记录和 `panda NewAPI channel 69 -> ZenProxy` 的记录合并归因。
- ClaudeCode 表面短 prompt 不等于短请求：ClaudeCode 会带系统提示、工具 schema、plugins/skills、agent 信息、历史上下文和模型别名。当前源码已增加并部署脱敏 request-shape 采样，字段包括 `system_tokens/messages_tokens/tools_tokens/tool_count/message_count/largest_message_tokens/last_user_tokens/estimated_total_tokens/stream/max_tokens/tool_choice_present/prompt_hash/source_client/profile_source`；禁止保存原始 prompt、请求体或密钥。
- `body_size=342` 这类小非流式空输出：先看 `short_request_kind`。当前分类只用于观测，`internal_claude_code_probe` 不会自动本地 `ok`；只有 `channel_test` 且上游连续空输出时才允许本地 `ok`，普通短请求仍应返回结构化空上游错误。

必须记录：

- base URL、模型、客户端、请求类型。
- stream/non-stream 拆分。
- prompt tokens 桶、输出 tokens。
- TTFT、first_content、总耗时。
- 工具调用、subagent/Task 调用成功率。
- 失败状态码、错误分类、重试结果。
- 是否污染用户默认配置。

详细方案、字段、错误分类、通过门槛和报告模板见 `docs/06-panda-pressure-test-plan.md`。如果两份文档冲突，以 `docs/06-panda-pressure-test-plan.md` 的压测细则为准，并同步修正本手册。

## 日志格式

每轮结束追加到 `docs/logs/YYYY/YYYY-MM.md`：

```text
## YYYY-MM-DD HH:mm

目标：
已做：
验证：
结论：
未完成：
下一步：
```

## 提交前检查

1. `git status --short` 查看所有改动。
2. 不提交 `.codex_tmp/`、密钥、临时输出、大型测试产物。
3. 对 `configured`、`panda`、异常字符文件等未跟踪项先确认来源。
4. 文档、测试和代码事实同步后再提交。
