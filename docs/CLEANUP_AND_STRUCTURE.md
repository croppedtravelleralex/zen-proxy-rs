# Cleanup And Structure

更新时间：2026-07-02

## 聚合结构

已建立统一入口：

```text
/home/lenovo/zen-free-model-suite
```

真实仓库仍在：

```text
/home/lenovo/free-model-client-rs
/home/lenovo/zen-proxy-rs
```

聚合目录通过软链接引用两个仓库，不复制、不搬迁、不改变 git root。

## 本轮清理

已清理或隔离：

- `free-model-client-rs/.bun/`
- `free-model-client-rs/.codex_tmp/`
- `free-model-client-rs/~`
- `free-model-client-rs/\`
- `free-model-client-rs/""`
- `free-model-client-rs/north-mini-code`
- `free-model-client-rs/tmpcc-zenprobe-wsl-*`
- `zen-proxy-rs/.codex_tmp/`
- `zen-proxy-rs/tmp/`
- `zen-proxy-rs/target-1.86/`
- 两仓 `__pycache__` 和 target 临时碎片

保留但忽略：

- `test-records/runs/`：历史验收和运行证据，默认不进源码提交。
- `target/`：构建缓存，忽略，不作为源码内容。

## Git 收口

- `free-model-client-rs/.gitignore` 已加入本地运行产物和测试运行目录。
- `zen-proxy-rs/.gitignore` 已加入 `.codex_tmp/`、`tmp/`，并保留 `target-1.86/` 忽略。
- `zen-proxy-rs/target-1.86` 曾被误跟踪，当前按构建产物从 git 索引移除。

## 后续边界

如果未来要做真正 mono-repo 迁移，需要先写迁移 RFC，审计 systemd/nginx/deploy/cargo/docs 全部路径，再在独立分支执行。当前不建议直接搬仓库。

