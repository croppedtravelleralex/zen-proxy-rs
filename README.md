# Zen Free Model Suite

这是 `free-model-client-rs` 与 `zen-proxy-rs` 的统一 WSL dev monorepo。

```text
/home/lenovo/zen-free-model-suite
```

该目录现在是真实总仓库，不再使用软链接聚合。两个项目源码已经作为真实目录导入到 `repos/` 下，并通过 `git subtree` 保留原仓库历史。

原路径 `/home/lenovo/free-model-client-rs` 和 `/home/lenovo/zen-proxy-rs` 暂时保留为备份/回滚点；后续默认从本目录继续开发。

## 目录

```text
/home/lenovo/zen-free-model-suite
├── repos/
│   ├── free-model-client-rs/
│   └── zen-proxy-rs/
├── docs/
│   ├── PROJECT_HANDOFF.md
│   ├── CLEANUP_AND_STRUCTURE.md
│   └── OPERATING_RULES.md
├── ops/
└── artifacts/
```

注意：顶层暂不声明 Cargo workspace。两个 Rust 项目保持原有构建边界，分别在各自目录运行 `cargo` 命令，避免本次结构迁移改变依赖解析和锁文件行为。

## 先读顺序

1. `docs/PROJECT_HANDOFF.md`
2. `docs/OPERATING_RULES.md`
3. `docs/CLEANUP_AND_STRUCTURE.md`
4. `repos/free-model-client-rs/docs/README.md`
5. `repos/zen-proxy-rs/docs/README.md`

## 当前生产公开模型

```text
deepseek-v4-flash
big-pickle
mimo-v2.5
hy3
```

最近一次生产核验时间为 2026-07-08。公开名与上游映射为：

```text
deepseek-v4-flash -> deepseek-v4-flash-free
big-pickle        -> big-pickle
mimo-v2.5         -> mimo-v2.5-free
hy3               -> hy3-free
```

`deepseek-v4-flash-lite` 已撤下公开名；其它自动发现的免费模型默认只作为 candidate/hidden routing，不自动加入 NewAPI 公开列表。当前状态、部署证据和未提交工作以 `docs/PROJECT_HANDOFF.md` 为准。
