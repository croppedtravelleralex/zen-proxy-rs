# newapi-usage-exporter

独立 Rust sidecar，用来按 `user_id + time range` 从 NewAPI 日志数据库只读导出使用记录。

它不修改 NewAPI，不依赖 ZenProxy/free-model-client-rs 运行链路，也不导出 prompt 原文、完整响应、真实 API key 或 IP 明文。

## 当前能力

- SQLite / Postgres NewAPI 日志库只读导出。
- 单次导出时间范围最大 31 天。
- 导出文件默认保留 30 天，启动、导出和后台定时清理会删除过期导出。
- 导出 zip 包包含：
  - `usage_logs.csv`
  - `usage_summary.json`
  - `brief_analysis.md`
  - `ai_analysis_guide.md`
  - `data_dictionary.md`
- 支持 CLI 和 HTTP API。
- HTTP API 可通过 `NEWAPI_USAGE_EXPORTER_ADMIN_TOKEN` 启用 `Authorization: Bearer ...` 或 `x-api-key` 保护。

## 配置

```bash
export NEWAPI_USAGE_SQLITE_PATH=/path/to/newapi.db
export NEWAPI_USAGE_EXPORT_DIR=/var/lib/newapi-usage-exports
export NEWAPI_USAGE_RETENTION_DAYS=30
export NEWAPI_USAGE_EXPORTER_ADMIN_TOKEN=change-me
```

Postgres：

```bash
export NEWAPI_USAGE_DATABASE_URL='postgresql://user:password@host:5432/new-api'
```

可选：

```bash
export NEWAPI_USAGE_LOG_TABLE=logs
export NEWAPI_USAGE_BIND=127.0.0.1:8098
```

## CLI

```bash
cargo run --manifest-path tools/newapi-usage-exporter/Cargo.toml -- export \
  --user-id 123 \
  --from 2026-06-01 \
  --to 2026-06-05
```

一句话导出：

```bash
cargo run --manifest-path tools/newapi-usage-exporter/Cargo.toml -- export \
  --instruction '导出用户123从2026年6月1日~2026年6月5日的数据并做简要分析'
```

清理过期导出：

```bash
cargo run --manifest-path tools/newapi-usage-exporter/Cargo.toml -- cleanup
```

## HTTP API

启动：

```bash
cargo run --manifest-path tools/newapi-usage-exporter/Cargo.toml -- serve
```

端点：

```text
GET    /health
POST   /v1/usage-export
POST   /v1/usage-export/instruction
GET    /v1/usage-export/{id}
GET    /v1/usage-export/{id}/download
DELETE /v1/usage-export/{id}
```

创建导出：

```bash
curl -sS http://127.0.0.1:8098/v1/usage-export \
  -H 'content-type: application/json' \
  -H 'authorization: Bearer change-me' \
  -d '{
    "user_id": "123",
    "from": "2026-06-01T00:00:00+08:00",
    "to": "2026-06-05T00:00:00+08:00",
    "include_brief_analysis": true
  }'
```

一句话导出：

```bash
curl -sS http://127.0.0.1:8098/v1/usage-export/instruction \
  -H 'content-type: application/json' \
  -H 'authorization: Bearer change-me' \
  -d '{"instruction":"导出用户123从2026年6月1日~2026年6月5日的数据并做简要分析"}'
```

## 数据边界

导出器只导出 NewAPI usage/log 表中已经存在的计量字段。当前适配会自动识别常见字段名，例如 `created_at/user_id/model_name/channel_id/prompt_tokens/completion_tokens/quota/status/type/error_message/use_time/stream`。

Postgres 路径只会选择字段候选表里的安全字段，不会选择 `content`、`ip`、`other`、`request_id` 等原始内容字段。

MySQL adapter 尚未实现。若生产 NewAPI 使用 MySQL，优先通过只读副本或安全导出快照给本 sidecar 使用；确认真实库配置后再补 adapter。
