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
- 当前 OpenClaw 自动识别必须优先看 body marker 和 OpenClaw 专属工具集，再看 ClaudeCode 共用工具名；`read/write/edit` 这类共用工具不能把 OpenClaw 误识别成 ClaudeCode。

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
- huge stream 日志里若出现 `ClaudeCode huge stream buffered upstream returned empty output`，先按 buffered retry 已兜底处理归类；只有最终裸透给客户端或耗尽重试才算失败。
- 如果需要临时中止压测，保留已有 `raw-results.jsonl`，再生成 partial summary，不要补写伪造的完成数。

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
