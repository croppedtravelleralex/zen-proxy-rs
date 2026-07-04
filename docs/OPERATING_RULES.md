# Operating Rules

## 硬约束

- 不泄露 key/token/proxy 凭据。
- 不裁剪上下文换速度。
- 不伪造 cache usage。
- 不牺牲工具调用质量。
- 不禁用 Bash/WebFetch/WebSearch 换稳定。
- 不通过降智、缩输出、隐藏提示词或全局 disabled thinking 换首字。
- 不把 Tailscale/panda 内网链路当成普通用户可交付链路。
- 不回滚未确认的用户改动或历史 dirty changes。
- 不在 panda 生产机上编译、`cargo build`、运行 `rustc` 或执行其他高 CPU 构建任务；panda 只允许下载已构建产物、校验、替换和重启。

## 测试链路

- ClaudeCode 验收必须走 cc-switch 和 `https://sub2api.closeapi.top`。
- dev/new 当前测试域名是 `https://new.relai.asia/`，不要再用旧 `new.closeapi.top`。
- 生产 channel 69 当前只公开 `deepseek-v4-flash`、`big-pickle`、`mimo-v2.5`。
- hidden routing 模型不加入 NewAPI 公开列表。

## 部署规则

- 需要生产部署时，通过 GitHub 临时 release 上传，远端下载部署，完成后删除 release/tag。
- 不用 scp 传生产二进制。
- 禁止把 GitHub source tarball 下载到 panda 后本机编译；如 release asset 上传失败，必须改用本地/CI 构建产物或暂停部署，不能用生产 CPU 兜底。
- 部署前后记录线上 hash、服务健康、模型列表和最小 smoke。

## 文档规则

- 代码、配置、测试和真实命令输出高于文档。
- 文档冲突时，以事实为准并同轮修正文档。
- 大型 raw logs、`.codex_tmp/`、测试原始输出、密钥和完整请求/响应不提交。
- 只提交脱敏摘要、报告、runner 和必要测试。

## Monorepo 规则

- 默认开发入口是 `/home/lenovo/zen-free-model-suite`。
- 两个子项目是真实目录，不再通过软链接引用旧仓库。
- 原 `/home/lenovo/free-model-client-rs` 和 `/home/lenovo/zen-proxy-rs` 暂时只作备份/对照，不作为默认修改入口。
- 顶层暂不作为 Cargo workspace；进入对应子项目目录分别运行 `cargo fmt`、`cargo clippy`、`cargo test`。
