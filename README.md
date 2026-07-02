# Zen Free Model Suite

这是 `free-model-client-rs` 与 `zen-proxy-rs` 的统一 WSL dev 入口。

```text
/home/lenovo/zen-free-model-suite
```

该目录只做软链接聚合和交接文档，不移动两个真实 git 仓库，避免破坏 cargo、systemd、nginx、脚本和历史文档路径。

## 目录

```text
/home/lenovo/zen-free-model-suite
├── repos/
│   ├── free-model-client-rs -> /home/lenovo/free-model-client-rs
│   └── zen-proxy-rs        -> /home/lenovo/zen-proxy-rs
├── docs/
│   ├── PROJECT_HANDOFF.md
│   ├── CLEANUP_AND_STRUCTURE.md
│   └── OPERATING_RULES.md
├── ops/
└── artifacts/
    └── claudecode-ccswitch-smoke-runs -> /tmp/claudecode-ccswitch-smoke-runs
```

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
```

`deepseek-v4-flash-lite` 已撤下公开名；其它免费模型只做 hidden routing，不加入 NewAPI 公开列表。

