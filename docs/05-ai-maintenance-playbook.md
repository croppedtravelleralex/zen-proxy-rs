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

## NewAPI 使用日志导出器纪律

- 入口文档：`docs/08-newapi-usage-exporter.md`。
- 代码目录：`tools/newapi-usage-exporter/`。
- 它是独立 Rust sidecar，不是 ZenProxy/free-model-client-rs 主链路的一部分。
- 只读 NewAPI 日志数据库；不得修改 NewAPI 源码或生产库数据。
- 不得导出 prompt 原文、完整响应、真实 API key 或 IP 明文。
- 单次导出范围最大 31 天，导出文件默认 30 天清理。
- 简要分析只能写数据事实和待确认问题；不得凭 token 长度猜用户用途，不做套餐推荐。
- HTTP API 若不只绑定 localhost，必须设置 `NEWAPI_USAGE_EXPORTER_ADMIN_TOKEN`，并走内网/管理网。
- 当前覆盖 SQLite/Postgres；MySQL 未实现。
- panda NewAPI 当前是 Postgres，真实连接来自 NewAPI 容器 `SQL_DSN`；报告中不得打印 DSN 密码。
- 生产部署前应创建专用只读 DB 用户，不要长期复用 NewAPI 主连接权限。
- panda 已部署 `newapi-usage-exporter.service`，本地 API 为 `http://127.0.0.1:8098`。
- 优先用 helper 执行一句话导出：`newapi-usage-export '导出用户1从2026年6月5日~2026年6月5日的数据并做简要分析'`。
- HTTP 指令入口为 `POST /v1/usage-export/instruction`；token 在 `/etc/newapi-usage-exporter.env`，不要打印明文。

验证命令：

```bash
cd /home/lenovo/free-model-client-rs
CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/newapi-usage-exporter-target cargo fmt --manifest-path tools/newapi-usage-exporter/Cargo.toml -- --check
CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/newapi-usage-exporter-target cargo clippy --manifest-path tools/newapi-usage-exporter/Cargo.toml --all-targets -- -D warnings
CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/newapi-usage-exporter-target cargo test --manifest-path tools/newapi-usage-exporter/Cargo.toml
```

## 客户端策略纪律

- ClaudeCode、Hermes、OpenClaw 不应再共享一套高侵入兼容策略。
- 90 分客户端识别和策略隔离已实现；下一步要用真实 ClaudeCode/Hermes/OpenClaw 小矩阵验证效果。
- 显式识别头为 `x-fmc-client`，优先级高于 User-Agent 和请求体推断。
- ClaudeCode 默认策略：不因 tools 禁用 thinking，保留流式空白 delta，只做硬协议工具历史修复。
- Hermes/OpenClaw 默认策略：保留协议兼容修复，但任何补齐或降级都必须记录 profile 和 repair count。
- unknown 默认策略：不禁 thinking，只做最小协议修复。
- 2026-06-04 起，识别 profile 和有效策略 profile 分开：日志仍记录真实 `source_client/profile_source`，同时记录 `effective_client/effective_profile_source`；`deepseek-v4-flash/deepseek-v4-flash-free` 不再套 Hermes/OpenClaw 兼容策略，只保留 ClaudeCode 深度适配，且在 `free-model-client-rs` 和 `zen-proxy-rs` 外层都取消输入 token 墙、只观测/告警不压缩；`deepseek-v4-flash-lite/big-pickle` 不再套 ClaudeCode 适配，只保留 Hermes/OpenClaw 适配。
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
PANDA_NEWAPI_KEY=<redacted> python3 scripts/panda_pressure_runner.py --mode policy-smoke
PANDA_NEWAPI_KEY=<redacted> python3 scripts/panda_pressure_runner.py --mode policy-dry --timeout-ms 300000
PANDA_NEWAPI_KEY=<redacted> python3 scripts/panda_pressure_runner.py --mode smoke
```

执行纪律：

- Windows ClaudeCode 必须从 Windows Python/PowerShell 启动同一个 runner，不要从当前 WSL 会话调用 Windows interop。
- dry run 没通过前不启动 full run；2026-05-31 历史 dry run 曾暴露 huge_context、OpenClaw subagent 和上游 overload 红旗。
- 2026-05-31 晚间 OpenClaw subagent 的 profile 误识别已修复，OpenClaw-only smoke 5/5、WSL 三客户端 smoke 15/15；但仍需重新跑 dry run 后才能进入 full run。
- 2026-06-01 已在 panda 部署 ClaudeCode huge_context final-anchor 修复；panda 本机 `/v1/messages` 约 1.0MB source-side smoke 中 flash 3/3、lite 3/3 均返回 `HUGE_OK`，但这不是四客户端真实 dry run。
- 2026-06-03 已在 panda 部署 channel 69 健康测试误判修复；空内容无工具探测应短路为 `ok`，普通小 `max_tokens` 请求不得误进 huge buffered retry。
- 2026-06-03 已补管理端测渠道第二层兜底：`echo hi`/`hi`/`hello`/`test` 类极短流式无工具探测，只有在上游空输出时才降级为本地 `ok`；普通请求不应提前短路。
- 2026-06-03 晚间已部署源码层非流式第二层兜底：同类极短非流式无工具探测在上游连续空输出后返回本地 `ok`；普通请求仍返回结构化空输出错误。
- 2026-06-03 晚间已部署 ClaudeCode 格式误伤修复：`web_fetch`/`web_search` 和普通 OpenClaw/Hermes 文本引用不再把请求判为 OpenClaw；受控 `Task + web_fetch` `/v1/messages` 新 pid 日志为 `source_client=claude-code`。
- 2026-06-04 18:54 已部署最新策略到 panda：输出限制已完全取消，缺省 `max_tokens` 不再补 1024/2048，显式 `max_tokens` 原样透传；OpenAI/Anthropic 只有显式值才写上游。ZenProxy 外层 context compactor 对 flash/free 只 warn/pass，对 lite 仍可 compact。已通过手工 NewAPI smoke 和大上下文不折叠 smoke；真实 panda `policy-smoke/policy-dry` 尚未跑，不能把该策略写成生产压测已验证。
- 2026-06-05 已部署 ClaudeCode Anthropic stream idle ping：只对 ClaudeCode Anthropic SSE 15 秒下游无可转发事件时发送协议 `ping`，用于缓解 `client_gone/use_time≈64s/completion=0`；不得把 ping 当 first content 或成功输出。
- 2026-06-06 已部署 V4.99 Stream Guard：只对 ClaudeCode Anthropic stream 的失败路径生效。真实 text/tool 输出前遇到上游截断或 60 秒无可转发内容可原地重试，最多 3 次；最后一次仅在工具请求中禁用 thinking。正常请求不改 prompt、不裁剪输入、不限制输出、不默认禁用 thinking。Anthropic tool `input_json_delta` 已按 4KB 分片；ClaudeCode forced `tool_choice` 首跳禁用 thinking，避免上游 `Thinking mode does not support this tool_choice`。
- 2026-06-06 已部署 provider `reasoning_content` 缺失兜底到 panda：若上游返回 `The reasoning_content in the thinking mode must be passed back to the API`，OpenAI/Anthropic 非流式、OpenAI 流式、ClaudeCode Anthropic 流式和 buffered huge-stream 只重试一次 `thinking: disabled`；这是 provider 400 后的窄兜底，不是全局关闭 ClaudeCode tools auto thinking。上游错误 public response 已脱敏，不再输出 `opencode zen` 或原始 provider body。线上 stripped hash `d5b7558c9f8f9fc7ea6faa802634dba85435868f1e338a4830f77079c3a1fc8e`，旧版本备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260606-124001.pre-68bf538`。
- 2026-06-06 已部署 `FREE_MODEL_TRUE_FIRST_TOKEN_FRT` 到 panda，默认开启：OpenAI/Anthropic 普通流式响应不再用空 role delta、`message_start`、`content_block_start` 或 pre-first ping 提前触发 NewAPI FRT；只有真实文本 token 或真实工具调用准备好时，才连同必要协议帧一起下发。若必须恢复旧保活口径，可临时设为 `0`，但 NewAPI FRT 会再次变成协议首包，不代表真实首字。线上 stripped hash `4233b08cdeb7bf18c76d7528d837c38e938c72e6f6ead13fe3c3d4b018aaefe3`，旧版本备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260606-162635.pre-true-frt-34c2f7d`；panda NewAPI smoke：OpenAI stream FRT 2112ms，Anthropic stream FRT 1990ms，均返回目标 marker。
- 2026-06-08 22:27 CST 已部署 V4.101 质量保全低延迟优化到 panda 三实例：ClaudeCode Anthropic stream 会在工具参数完整可解析后提前发送 `tool_use`，并记录 `first_tool_emit_ms`；no-forwardable retry 会按输入桶自适应；ZenProxy affinity key 已加入稳定前缀/tools/tool_choice hash；tool-heavy lane 阈值已下调到 `tools>=8` 或 `tool_markers>=6`。线上 stripped hash `149dd2f65c8b33228498bcc1f2e94f6742e1e1a5417592c0eb6921e7cc7deb49`，旧版备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260608-222704.pre-v4101`。部署后最小 smoke 已通过；仍需观察 `first_tool_emit_ms`、`first_tool_call_ms`、`first_content_ms`、`attempts_used`、`cache_observation` 和工具重复/缺参错误。
- 2026-06-09 已部署 V4.102 ClaudeCode 工具参数完整性门控到 panda 三实例：缺必填参数、空 `{}`、重复 repaired 工具调用和明显坏 `file_path` 均在源头拦截或窄修复。线上 stripped hash `ebe41572fe76a5f99783ba5e4308e164368415b00277432cd9829e60ecc651dd`，旧版备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260609-111046.pre-v4102-tool-input-guard`。Windows ClaudeCode 真实 Bash/Write/Read、ToolSearch、Task/Agent、Markdown 小矩阵通过；NewAPI 近窗口无错误类型日志。
- 2026-06-09 17:28 CST 已部署 V4.105 ClaudeCode true-stream/cache-hit 修复到 panda 三实例：ClaudeCode 带 tools 或多行 Markdown/JSON/代码块 exact-output 不再进入 `anthropic_buffered`；DeepSeek `prompt_cache_hit_tokens/prompt_cache_miss_tokens` 会进入统一 cache usage/观测路径。线上 stripped hash `a52f4d6add0a93fe0070a59c3a3ec9ee3b4bc0a9172047c7b3ec5855e67ff7e8`，旧版备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260609-172830.pre-v4105-08d9064600e6`；panda 4000 `/health`、`/v1/models`、NewAPI OpenAI/Anthropic `PONG` smoke 通过。
- 2026-06-09 21:37 CST 已部署 V4.106 质量保全 cache-friendly 中等上下文 session 优化到 panda 三实例：10k+ 且 material 小于等于大前缀阈值的请求使用默认 32KB 稳定前缀计算上游 session/project，大上下文仍用默认 256KB；只改 header 分组，不改正文、不裁剪、不限输出、不改提示词。线上 stripped hash `b401c9463e29788e67aaecbe53c02b8743b2e25970e135e767410df9d4e0edab`，旧版备份 `/opt/zen-proxy-rs/backups/zen-proxy-rs.20260609-213725.pre-v4106-a52f4d6add0a`；4000 `/health`、`/v1/models`、ZenProxy 直连 OpenAI/Anthropic 和 panda NewAPI -> channel 69 OpenAI/Anthropic smoke 均通过。部署后前几分钟 cache rejected 可能是 `large_prefix_v4106` warm-up，必须看 15-30 分钟以上真实流量窗口；同时早期仍有 `reasoning_only_length` 首跳空输出重试和少量 NewAPI 500，不得写成零错误。
- panda 当前没有 Rust 工具链；上线源码补丁时优先在本机/WSL 构建 Linux release，再上传 strip 后二进制，不要在生产机上临时高负载编译。
- Windows `ssh panda` 使用 `C:\Users\Lenovo\.ssh\config` 中的 `root@100.69.228.93`；WSL 默认 `ssh panda` 可能没有该配置。WSL 需要显式使用 `/mnt/c/Users/Lenovo/.ssh/id_ed25519`。
- 本机到 panda 的 Tailscale/SSH 可能临时抖动：表现为 `scp`/SSH stdin/HTTP taildrop 大文件传输 reset，但短 SSH 和端口检测偶尔正常。遇到时先跑 `tailscale ping` 和 `tailscale netcheck`，优先传 xz 压缩包；不要在服务半重启状态下继续折腾。部署脚本从 PowerShell 调远端 shell 时，避免 `$()` 和 `$var` 被 PowerShell 提前展开，复杂远端脚本用 `bash -s` 且去掉 CRLF。
- huge stream 日志里若出现 `ClaudeCode huge stream buffered upstream returned empty output`，先按 buffered retry 已兜底处理归类；只有最终裸透给客户端或耗尽重试才算失败。最新输出策略不再通过 `max_tokens` cap 控制长输出，20k/32k 等显式长输出应原样透传给上游。
- 如果需要临时中止压测，保留已有 `raw-results.jsonl`，再生成 partial summary，不要补写伪造的完成数。
- WSL ClaudeCode 当前不能直接作为有效客户端样本：`/home/lenovo/.local/bin/claude` 和 `claude-deepseek-free` 是 clawgod launcher，实际启动 `/root/.clawgod/cli.cjs`，不是 Anthropic ClaudeCode CLI；正式四客户端压测前必须修复或替换。
- Windows ClaudeCode 经过 cc-switch 时必须先确认 current provider。2026-06-04 发现 current Claude provider 是 `closedeepseek -> https://sub2api.closeapi.top`，不是 `LocalNewapi -> http://127.0.0.1:8081`；普通 Windows 使用记录不能直接归因到 panda NewAPI/ZenProxy。
- OpenClaw 若输出固定 `HEARTBEAT_OK` 且 stderr 有 local secrets gateway `1006 abnormal closure`，先归类为 OpenClaw 本地 agent/gateway/harness 问题，不要先改 ZenProxy。

## 常见现象解释

- prompt tokens 稳定在约 60k：先确认时间点和模型。历史版本曾有流式 compactor/输出 cap；最新 `deepseek-v4-flash/deepseek-v4-flash-free` 已取消输入 token 墙，`free-model-client-rs` 和 `zen-proxy-rs` 外层都只观测/告警不压缩，不能再默认按旧 compactor 解释。
- prompt tokens 稳定在约 330k：不要先判定输入墙，也不要先裁剪。先对齐 ZenProxy `context_action/effective_body_size`、free-model-client-rs `messages_tokens`、NewAPI `prompt_tokens`；如果 flash/free 是 `pass` 且 tokens 持续增长，说明在吃完整长上下文。
- NewAPI cache tokens 几乎为 0：先看 `cache_observation=attempted/accepted/rejected/ignored`，再看 provider header/body usage 信号。`ignored` 代表没有可用 cache 字段，`attempted` 代表 provider 返回了 cache 字段但值为 0，`accepted` 才是 cache token > 0，`rejected` 代表 provider 明确拒绝 cache 控制。
- V4.98 cache 排查顺序：先比较 `prefix_4k_hash/prefix_32k_hash/prefix_128k_hash/prefix_256k_hash` 是否稳定；若 prefix 稳定但 cache 仍为 0，再查上游 session、代理节点、账号和 provider cache 行为；若 prefix 本身不稳，先定位 ClaudeCode 是否把易变内容放在前缀，而不是直接缩上下文。
- ClaudeCode CLI Markdown/表格/代码块/列表显示异常：先抓 raw SSE 和 `source_client`。若新 pid 仍不是 `claude-code`，优先查 profile 识别；若 raw SSE 正确但终端显示错，再归类为 CLI 渲染问题。
- NewAPI 偶发 `status_code=500, upstream returned no assistant content or tool call`：先按空上游保护排查，不要先改 NewAPI。历史样本曾因流式 `max_tokens` 被 768/1024 cap 后绕过 buffered retry；最新策略已经完全取消输出限制，后续应重点看真实上游空输出、客户端断流、lane/pool 调度或非流式 fallback。
- Web search 用不了：先分清“模型原生联网搜索”和“客户端工具搜索”。本仓库不自带搜索引擎，只转发 tools/tool_calls/tool results。排查顺序是：请求是否带 `WebSearch/WebFetch` 或 `web_search/web_fetch` 工具定义、模型是否发出 tool call、ZenProxy 是否把上游工具名 canonicalize 回客户端注册名、客户端/工具执行器是否执行联网、工具结果是否回到模型上下文。Hermes/OpenClaw/ClaudeCode 要分开验收。2026-06-04 已确认直连 panda NewAPI 带 `web_search` tool 能返回 tool call；用户截图也证明 Windows ClaudeCode 官方 Claude 路径可真实执行 `WebSearch/WebFetch`。不要再把“某次 ZenProxy 样本没有执行 web 工具”写成 ClaudeCode 不支持 web 工具。
- ClaudeCode 工具名大小写/别名：ClaudeCode 注册的是 `WebSearch`、`WebFetch`、`Task` 等真实工具名；如果上游返回 `web_search`、`web_fetch`、`task`，客户端可能不执行。free-model-client-rs 已补 canonicalization 回归，并已部署到 panda stripped hash `0f6cdf6e5cd2dd1946a69707c97591cca865b47178ff63846f04bbdf283f2314`；线上直连 smoke 已验证 `WebSearch` 和 `Task` 工具名。
- NewAPI 看到 70k-90k input tokens：不要直接判定为 NewAPI 输入墙。先对齐三种口径：ZenProxy `body_size` 是 JSON 字节数；free-model-client-rs `prompt_tokens` 是估算/策略口径；NewAPI/cc-switch usage 是最终账单口径。最新 flash 策略只观测不压缩；如果日志里还看到 `compacted` 或 `capped` 字样，必须先确认是历史日志、lite/其他模型路径，还是旧二进制。
- Windows ClaudeCode 当前可能先走 cc-switch：检查 `C:\Users\Lenovo\.claude\settings.json` 里的 `ANTHROPIC_BASE_URL`，再查 cc-switch provider。不要把 `ClaudeCode -> cc-switch -> closeapi` 的记录和 `panda NewAPI channel 69 -> ZenProxy` 的记录合并归因。
- ClaudeCode 表面短 prompt 不等于短请求：ClaudeCode 会带系统提示、工具 schema、plugins/skills、agent 信息、历史上下文和模型别名。当前源码已增加并部署脱敏 request-shape 采样，字段包括 `system_tokens/messages_tokens/tools_tokens/tool_count/message_count/largest_message_tokens/last_user_tokens/estimated_total_tokens/stream/max_tokens/tool_choice_present/prompt_hash/source_client/profile_source`；禁止保存原始 prompt、请求体或密钥。
- `body_size=342` 或 `/context` 这类小非流式空输出：先看 `short_request_kind`、`tool_count`、`max_tokens` 和 `empty_output_class`。2026-06-05 已确认一类 ClaudeCode 内部工具探针会带 1 个小工具、`max_tokens=1/16`，上游 DeepSeek 容易返回 `reasoning_only_length` 且正文/工具调用为空；当前源码和 panda 线上均已对这类低预算工具探针做 thinking disabled + `max_tokens` 最小 64 的窄保护，不返回本地假答案。`channel_test` 且上游连续空输出时才允许本地 `ok`，普通短请求仍应返回结构化空上游错误。
- NewAPI 红行但 `type=2`、`stream=true`、`stream_status.end_reason=client_gone`：不要按上一类非流式 502 处理。先查是否 `completion=0`、`use_time≈60-65s`、ZenProxy 同窗口是否没有空上游/截断/retry 错误；若吻合，优先归类为下游流式读空闲断开。2026-06-05 起 ClaudeCode Anthropic 流式会发协议 ping 保活，但如果客户端要求真实内容在固定时间内到达，仍需 first-content watchdog，而不是伪造文本。
- NewAPI FRT 口径：`FREE_MODEL_TRUE_FIRST_TOKEN_FRT=1` 时，普通流式请求的 NewAPI FRT 应接近真实首个文本 token 或首个 tool call 可转发时间；`FREE_MODEL_TRUE_FIRST_TOKEN_FRT=0` 或历史版本中，FRT 可能只是空协议首包。ZenProxy admin 的 `first_content_token_ms/first_tool_call_ms` 仍是更细粒度事实来源；NewAPI UI 只显示自己的 FRT。
- ClaudeCode 150k+ 大上下文偶发慢首字：先查 free-model-client-rs 日志里的 `ClaudeCode stream guard completion summary`，不要只看 NewAPI FRT。关键字段是 `attempts_used/retry_count/first_upstream_response_ms/first_upstream_event_ms/first_reasoning_ms/first_content_ms/first_tool_call_ms/idle_ping_count/cache_observation/cache_read_input_tokens/estimated_total_tokens/prompt_hash_hex`。若 `attempts_used>1` 且前一跳有 `upstream provider error (status=520)` 或 `upstream initial stream fetch timeout`，归类为上游慢失败+重试；若 cache accepted 但 `first_reasoning_ms` 早、`first_content_ms` 晚，归类为上游长思考/无可转发内容；若 `cache_observation=rejected`，再看 `prefix_*_hash` 是否稳定。
- ClaudeCode 大上下文首包保护调参：`FREE_MODEL_CLAUDE_CODE_STREAM_INITIAL_FETCH_TIMEOUT_SECS` 默认 30 秒，设 0 关闭；`FREE_MODEL_CLAUDE_CODE_STREAM_SLOW_GUARD_MIN_INPUT_TOKENS` 默认 150000；`FREE_MODEL_CLAUDE_CODE_STREAM_NO_FORWARDABLE_RETRY_SECS` 默认 45 秒。调小会降低部分长尾但可能增加上游重复请求成本；调大更保守但慢首字改善较弱。
- V4.101 后 ClaudeCode no-forwardable retry 实际等待时间不是单纯的 env 值，而是 `min(env, token_bucket_default)`：`<50k=10s`、`50k-100k=14s`、`100k-200k=22s`、`200k-400k=32s`、`400k+=45s`。如果仍看到 50k-100k 请求无真实 text/tool 等到 45s 以上，优先确认线上 hash 是否包含 V4.101。
- V4.101 后工具慢首字要分三层看：`first_tool_call_ms` 是上游第一次出现工具 delta，`first_tool_emit_ms` 是 free-model-client-rs 第一次向下游发出完整可执行 tool_use，NewAPI FRT 是下游看到的真实首个文本或工具事件。若 `first_tool_call_ms` 早但 `first_tool_emit_ms` 晚，说明工具 JSON 分片很久才完整；若二者都早但 NewAPI FRT 晚，查 ZenProxy metered stream 和 NewAPI 链路。
- ClaudeCode `API Error: Failed to parse JSON`：先查 NewAPI channel 69 是否对应 `status_code=500, stream truncated before DONE or finish_reason`。如果是 2026-06-06 V4.99 之后的新样本，继续查 ZenProxy 日志里的 `ClaudeCode stream guard observed upstream stream error`、`retrying after no forwardable upstream output`、`enabling disabled-thinking fallback`、`refusing to emit possibly partial tool calls`。不要再直接归因成 30KB Write JSON 溢出；除非日志证明 tool JSON 已经完整生成但客户端解析失败。
- ClaudeCode `Invalid tool parameters` / `missing parameters`：V4.102 后不应再把缺必填字段或空 `{}` 的 tool_use 下发给客户端。排查顺序：确认线上 hash 是否为 `ebe41572fe76a5f99783ba5e4308e164368415b00277432cd9829e60ecc651dd` 或更新；查 `repaired ClaudeCode tool call arguments from latest user instruction`、`refusing to repair duplicate ClaudeCode tool call already completed in history`、`received only incomplete tool calls`、`upstream returned incomplete tool call arguments`；如果客户端仍报缺参，抓 raw SSE 看是否有缺必填字段穿透。
- ClaudeCode 文件工具坏路径：如果看到 `file_path="\\"`、`"/"`、`"."` 导致 `EISDIR` 或写根目录，先查 V4.102 路径门控是否生效。该门控只对 `Read/Write/Edit/MultiEdit/Notebook*` 的明显非文件路径生效；`LS` 的目录 path 不在此规则内。
- ClaudeCode Task/subagent：Windows ClaudeCode stream-json 中上游 tool name 可能显示为 `Agent`，随后 ClaudeCode 本地事件是 `task_started` / `local_agent`。验收 subagent 时不要只 grep `name="Task"`；应同时识别 `Agent` tool_use、`task_started`、`task_notification` 和最终 tool_result。
- ClaudeCode forced `tool_choice` 工具调用报 400：先看是否包含 `Thinking mode does not support this tool_choice`。V4.99 后 ClaudeCode forced `tool_choice` 应该首跳禁用 thinking；若仍出现，检查 `source_client/effective_client` 是否为 ClaudeCode、`tool_choice_present` 是否为 true，以及线上 hash 是否为 `39dc0bb94092597a00518abf83e80f8c32a91e8c60682c169942bf16bf70017d` 或更新版本。
- NewAPI 日志出现 `reasoning_content in the thinking mode must be passed back`：这是 DeepSeek provider 对 OpenAI-compatible thinking/tool history 的硬拒绝。2026-06-06 源码已加一次性 disabled-thinking 重试；若部署后仍出现，检查日志中是否有 `retrying upstream missing reasoning_content error with disabled thinking`，以及失败是否来自未覆盖路径。public 日志不得再含 `opencode zen`，否则先修错误映射。

必须记录：

- base URL、模型、客户端、请求类型。
- stream/non-stream 拆分。
- prompt tokens 桶、输出 tokens。
- TTFT、first_content、总耗时。
- 工具调用、subagent/Task 调用成功率。
- 失败状态码、错误分类、重试结果。
- provider header/body usage 信号和 cache `attempted/accepted/rejected/ignored` 四态。
- V4.98 prefix hash：`prefix_4k_hash/prefix_32k_hash/prefix_128k_hash/prefix_256k_hash/cache_material_bytes`。
- 输出限制取消后的 input/output wall 结果，尤其是 413、超时、空输出、长尾延迟和成本风险。
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
