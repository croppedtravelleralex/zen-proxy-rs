# Cleanup And Structure

更新时间：2026-07-02

## 聚合结构

已建立统一入口：

```text
/home/lenovo/zen-free-model-suite
```

当前结构已经从软链接聚合升级为真实 monorepo：

```text
/home/lenovo/zen-free-model-suite
├── repos/free-model-client-rs/
└── repos/zen-proxy-rs/
```

两个子项目通过 `git subtree` 导入，目录内没有嵌套 `.git`，也没有指向旧仓库的软链接。原仓库路径仍保留为备份/回滚点：

```text
/home/lenovo/free-model-client-rs
/home/lenovo/zen-proxy-rs
```

本次没有把顶层改成 Cargo workspace，两个 Rust 项目继续保持独立构建边界。

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
- `/home/lenovo/zen-free-model-suite/repos/*` 旧软链接
- `/home/lenovo/zen-free-model-suite/artifacts/claudecode-ccswitch-smoke-runs` 旧软链接

保留但忽略：

- `test-records/runs/`：历史验收和运行证据，默认不进源码提交。
- `target/`：构建缓存，忽略，不作为源码内容。
- `/tmp/claudecode-ccswitch-smoke-runs`：本机临时验收运行目录，不在总仓中软链接提交。
- `/home/lenovo/.bun`：全局 Bun/opencode 运行时，不属于项目垃圾。

## Git 收口

- 总仓 `.gitignore` 已忽略各子项目 `target/`、`target-*`、临时目录、raw logs、环境文件和 `artifacts/` 原始产物。
- `free-model-client-rs/.gitignore` 已加入本地运行产物和测试运行目录。
- `zen-proxy-rs/.gitignore` 已加入 `.codex_tmp/`、`tmp/`，并保留 `target-1.86/` 忽略。
- `zen-proxy-rs/target-1.86` 曾被误跟踪，当前按构建产物从 git 索引移除。

## 本轮迁移验证

在真实 monorepo 路径下执行并通过：

```text
cd /home/lenovo/zen-free-model-suite/repos/free-model-client-rs && cargo fmt -- --check
cd /home/lenovo/zen-free-model-suite/repos/zen-proxy-rs && cargo fmt -- --check
cd /home/lenovo/zen-free-model-suite/repos/free-model-client-rs && cargo clippy --all-targets -- -D warnings
cd /home/lenovo/zen-free-model-suite/repos/zen-proxy-rs && cargo clippy --all-targets -- -D warnings
cd /home/lenovo/zen-free-model-suite/repos/free-model-client-rs && cargo test
cd /home/lenovo/zen-free-model-suite/repos/zen-proxy-rs && cargo test
```

测试结果：

- `free-model-client-rs`：132 个 lib 测试、132 个 `kernel_golden` 测试通过。
- `zen-proxy-rs`：196 个 unit 测试、44 个 e2e 测试通过。

## 后续边界

真实 monorepo 迁移已经完成。下一步如果要进一步收敛构建体验，可以单独评估顶层 Cargo workspace 或统一 task runner；这会改变 Cargo 锁文件和命令边界，必须另起小步验证，不和本次结构迁移混在一起。
