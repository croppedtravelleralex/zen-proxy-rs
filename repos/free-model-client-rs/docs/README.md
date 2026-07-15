# free-model-client-rs 维护入口

这是本仓库的 AI 维护真相源。对外介绍看根目录 README.md；涉及当前状态、未实现待办、验收记录和后续接手，先读这里。

## 跨仓统一入口

2026-07-02 起，`free-model-client-rs` 与 `/home/lenovo/zen-proxy-rs` 的协同维护入口为：

```text
/home/lenovo/zen-free-model-suite
```

该目录已通过 `git subtree` 导入两个项目的真实源码目录，不再使用软链接。跨仓接手优先读 `/home/lenovo/zen-free-model-suite/README.md`、`docs/PROJECT_HANDOFF.md` 和 `docs/CLAUDECODE_STABILITY_HANDOFF_2026-07-15.md`。

## 阅读顺序

1. docs/02-current-state.md：当前已实现、运行边界、最新验证结果。
2. docs/03-roadmap.md：Now / Next / Later 任务顺序。
3. docs/04-improvement-backlog.md：未实现、风险、优化池。
4. docs/05-ai-maintenance-playbook.md：后续 AI 接手和测试纪律。
5. docs/06-panda-pressure-test-plan.md：panda-only 四客户端压测方案和报告模板。
6. docs/07-client-profile-policy-plan.md：客户端识别、策略隔离和 ClaudeCode 误伤修复方案。
7. docs/08-newapi-usage-exporter.md：独立 NewAPI 使用日志导出 sidecar。
8. docs/reports/2026-06-13-claudecode-web-tool-handoff.md：ClaudeCode WebSearch/WebFetch、cc-switch provider、502/parse JSON 调查交接。
9. docs/reports/2026-07-02-cross-repo-suite-handoff.md：跨仓聚合入口、链路、完成事项、约束和后续建议。
10. docs/logs/YYYY/YYYY-MM.md：按时间追加的工作记录。

最新生产交接：`/home/lenovo/zen-free-model-suite/docs/CLAUDECODE_STABILITY_HANDOFF_2026-07-15.md`。

## 真相来源优先级

1. 代码、配置、测试、命令输出。
2. panda/NewAPI/Hermes/OpenClaw 的真实运行结果。
3. 本目录维护文档。
4. 根目录 README.md 和聊天记录。

如果文档和代码冲突，以代码和验证结果为准，并在同一轮更新文档。

## 当前核心链路

本仓库是 Rust 内核级反代适配层，当前目标是把 OpenAI/Anthropic 客户端请求规范化后转发到 OpenAI-compatible 上游。

已确认 panda 直连最小链路：

```text
client -> panda NewAPI http://100.69.228.93:8081 -> configured upstream
```

本轮没有直接证明 panda NewAPI 后面的具体服务名；后续报告必须用 NewAPI 管理端/日志或响应证据再确认，不允许凭印象下结论。

## 最终目标

当前 goal 的最终状态：

```text
free-model-client-rs 当前 panda 联调完成收尾，并进入 panda-only 四客户端压测准备完成状态。
```

完成标准：

1. 维护文档和根 README 与真实代码、测试和 panda 运行结果一致。
2. panda NewAPI、WSL Hermes、WSL OpenClaw 的小矩阵结果已经脱敏记录。
3. `.codex_tmp/`、临时配置、未跟踪文件和敏感输出有明确归类，不混入提交。
4. Git 状态清晰，代码改动和文档改动可审查、可提交。
5. panda-only 压测执行器可复用，且 dry-run 红旗已被明确归类；full run 只在 dry run 通过后执行。

## 当前剩余待办

2026-06-13 最新交接状态：

1. 用户已要求停止继续测试/修复，本轮只做文档收口和交接。
2. Windows ClaudeCode 真实基础工具链 `Bash -> Write -> Read` 已通过，未复现“ZenProxy 清空工具参数”。
3. WebSearch/WebFetch 专项显示：工具参数完整；WebSearch 返回空结果；WebFetch 失败在 ClaudeCode 本地安全验证/`claude.ai` 链路，不是本轮样本中的工具参数转换错误。
4. 当前 Windows cc-switch Claude provider 是 `closedeepseek -> https://sub2api.closeapi.top -> deepseek-v4-flash`，不是 panda NewAPI `100.69.228.93:8081`；后续分析必须先确认 provider，不得混用 cc-switch 与 panda channel 69 数据。
5. 详细交接见 `docs/reports/2026-06-13-claudecode-web-tool-handoff.md`。

P0 已完成但待文档收尾：

1. panda NewAPI OpenAI/Anthropic 最小请求已通过，需要保持记录同步。
2. Hermes 临时接入 panda NewAPI 已完成短回复和文件/终端工具验证；web 用例命令成功但返回 `WEB_FAIL`，需要作为残留观察点。
3. OpenClaw 已通过隔离 Node 22.21.1、临时配置、models list、infer、agent 文件工具和 web_fetch 验证。

当前压测状态：

1. 90 分客户端识别和 policy 隔离已落地并部署到 panda。
2. 无密钥 panda 压测执行器已落地到 `scripts/panda_pressure_runner.py`。
3. 2026-06-02 exact-output TTL guard 已部署到 panda，ZenProxy 4000/4001/4002/4004 直连健康。
4. 2026-06-03 channel 69 空输出/健康测试误判已从源头修复并部署到 panda；`Zenproxyrs4.3` channel 69 当前启用，`vip` 组，指向 `http://172.17.0.1:4000`。
5. 2026-06-03 已补第二层兜底：NewAPI 管理端测渠道常见的极短 `echo hi` 流式探测，如果上游只吐空流，会由 ZenProxy/free-model-client-rs 返回本地 `ok`，不再裸透 `upstream returned no assistant content or tool call`。
6. `sk-dev` 已不是有效压测 token，且属于历史 default 组；测试 channel 69 必须使用 NewAPI 中有效的 `vip` 组 token，报告中不得打印明文 key。
7. preflight 已补充 `auth_error`、`channel_unavailable`、`blocker` 分类；无有效 NewAPI token 或目标渠道不可用时会阻断，不进入 smoke/dry/full。
8. OpenClaw body/profile 识别修复已部署到 panda，OpenClaw-only smoke 5/5 通过，WSL 三客户端 smoke 15/15 通过。
9. dry run 仍未重新通过：huge_context、`deepseek-v4-flash-lite` 长上下文语义漂移、Hermes 慢路径和上游 overload/502/524 仍需处理。

仍需执行：

1. 2026-07-02 已清理或归类 `.codex_tmp/`、异常字符文件、根目录孤儿文件和本地运行产物；后续若重新生成，继续按 `.gitignore` 与提交前检查处理。
2. 用有效 `vip` 组 token 复跑 panda-only smoke/dry；不要再用 `sk-dev` 判断 channel 69 状态。
3. 先处理/复测 dry-run 红旗，不直接启动 4 客户端 x 500 full run。
4. 针对 huge_context lane、lite 长上下文策略和 Hermes 慢路径做修复或降级，再重新跑 dry run。
5. 跑 full run 前再次确认不会污染 Hermes/OpenClaw/ClaudeCode 用户默认配置。
6. 提交前复跑必要验证，并确认 README 与维护文档无旧状态残留。
