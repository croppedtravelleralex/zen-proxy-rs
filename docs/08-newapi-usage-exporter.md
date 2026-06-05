# NewAPI 使用日志导出 Sidecar

更新时间：2026-06-05

## 目标

新增一个与 ZenProxy/free-model-client-rs 主链路解耦的 Rust sidecar，用来按 `user_id + time range` 导出 NewAPI 使用日志，并生成轻量分析包。

这个工具解决的是“用户用量与行为分析”问题，不参与模型反代，不修改 NewAPI，不读取 ZenProxy 内部观测数据。

## 边界

必须遵守：

- 只读 NewAPI 日志数据库。
- 不修改 NewAPI 源码。
- 不接入 ZenProxy 请求链路。
- 不导出 prompt 原文、完整 response、真实 API key 或 IP 明文。
- 不根据 token 形态武断推断用户用途。
- 不做套餐推荐；当前业务模式是按量充值，用多少充多少。
- 导出文件只保留 30 天，过期后清理。

当前第一版只实现 SQLite 只读适配。MySQL/Postgres 需要确认真实 NewAPI 数据库配置后再补 adapter。

## 代码位置

```text
tools/newapi-usage-exporter/
```

该目录是独立 Cargo crate，不加入根 workspace，不影响主库构建。

## 输出文件

每次导出生成一个 zip：

```text
analysis_pack.zip
```

包含：

```text
usage_logs.csv
usage_summary.json
brief_analysis.md
ai_analysis_guide.md
data_dictionary.md
```

说明：

- `usage_logs.csv` 是脱敏明细。
- `usage_summary.json` 是机器可读汇总。
- `brief_analysis.md` 只做客观简析和待确认问题。
- `ai_analysis_guide.md` 给低能力模型使用，指导它先问用途再分析。
- `data_dictionary.md` 解释字段含义和隐私边界。

## API 覆盖

HTTP API：

```text
GET    /health
POST   /v1/usage-export
GET    /v1/usage-export/{id}
GET    /v1/usage-export/{id}/download
DELETE /v1/usage-export/{id}
```

CLI：

```text
serve
export
cleanup
```

## 配置

```text
NEWAPI_USAGE_SQLITE_PATH                 必填，NewAPI SQLite DB 路径
NEWAPI_USAGE_EXPORT_DIR                  可选，默认系统临时目录 newapi-usage-exports
NEWAPI_USAGE_RETENTION_DAYS              可选，默认 30
NEWAPI_USAGE_LOG_TABLE                   可选，默认自动识别 logs/log/usage_logs/newapi_logs
NEWAPI_USAGE_BIND                        可选，默认 127.0.0.1:8098
NEWAPI_USAGE_EXPORTER_ADMIN_TOKEN        可选，但非 localhost 暴露时必须设置
```

## 当前字段

导出字段：

```text
log_id
time
user_id
username
token_id
token_name
model
channel_id
channel_name
group
prompt_tokens
completion_tokens
total_tokens
quota_cost
status
error_message_class
duration_ms
stream
endpoint
```

错误只导出分类，不导出原始错误全文。当前分类包括：

```text
ok
rate_limited
timeout
channel_error
model_error
empty_output
protocol_error
bad_request
upstream_error
other_error
```

## 分析方法

第一版只做“简要分析”，重点是把事实整理出来，而不是替用户下结论。

允许输出：

- 请求数、成功率、token 汇总、额度消耗。
- 长输入、长输出、高输入低输出的数量。
- 模型、渠道、错误分类分布。
- 应该向用户追问的问题。

不允许输出：

- “用户在写小说”这类只凭长输出猜用途的判断。
- 套餐推荐。
- 与日志证据无关的优化建议。

建议流程：

1. 先导出用户某个时间段的使用包。
2. 先看 `usage_summary.json` 和 `brief_analysis.md`。
3. 把 `ai_analysis_guide.md` 和脱敏 CSV 交给另一个 AI。
4. 先让用户说明主要用途，例如通信协议、编程、文档、写作、客服。
5. 再结合用途提出节省成本、提升稳定性或改善输出质量的建议。

## 验收

当前本地验证：

```bash
cd /home/lenovo/free-model-client-rs
CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/newapi-usage-exporter-target cargo fmt --manifest-path tools/newapi-usage-exporter/Cargo.toml -- --check
CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/newapi-usage-exporter-target cargo clippy --manifest-path tools/newapi-usage-exporter/Cargo.toml --all-targets -- -D warnings
CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/newapi-usage-exporter-target cargo test --manifest-path tools/newapi-usage-exporter/Cargo.toml
```

已覆盖：

- 超过 31 天范围会拒绝。
- SQLite fixture 能生成分析包。
- SQLite 整数列能正确导出为 CSV 字符串。
- 可关闭 `brief_analysis.md`。
- 过期导出目录会被清理。

## 未实现

待真实生产配置确认后再做：

- MySQL/Postgres 只读 adapter。
- NewAPI 真实库 schema 对齐报告。
- 按用户/时间的分页预览 API。
- 更丰富的维度统计，例如日趋势、小时热力、模型成本占比、错误 Top N。
- 独立部署单元、systemd 文件和反代安全策略。
- 端到端测试：读取生产只读快照、创建导出、下载、过期清理。
