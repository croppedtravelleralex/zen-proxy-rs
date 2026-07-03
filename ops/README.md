# Ops

总仓级运维脚本与 Cache 99+ 验收 runbook。

## 部署 vs 生效（禁止混用）

| 说法 | 何时成立 |
|------|----------|
| **运维部署成功** | `deploy-panda-tmcc.sh` exit 0；磁盘二进制 hash 更新；`@1/@2/@3` active；`:4000/health` ok |
| **TMCC 2.0 / CCP 生产生效** | 部署后 audit **绝大多数行**含 `usk` 等新字段；R2 向 95% 收敛 |
| **99+ 验收通过** | `cache_quality_acceptance.py --strict` 全绿 |

2026-07-03 现状：**运维部署 ✅**（`572cba42…`）；**生效 ❌ 未证实**（13:57 后 deepseek audit 仅 **6.4%** 含 `usk`）；**验收 ❌**（ccswitch ~41%，NewAPI ~54%）。

详见 `plan.md` 与 `docs/cache-95plus-architecture.md` §「部署 vs 生效」。

## Cache 99+ 部署

```bash
bash ops/deploy-panda-tmcc.sh
```

脚本会：release 构建 → strip 上传 → nginx 粘性 → 重启 `@1/@2/@3` → 追加 `CCP_*` env → smoke。
**跑通 ≠ 验收通过。**

## 验收与对账

| 脚本 | 用途 |
|------|------|
| `tri_cache_report_v2.py` | ccswitch / NewAPI / ZenProxy 三方 R1（`--since` 默认 13:57） |
| `deploy_schema_forensics.py` | audit 是否含 `usk` 键（新 schema 代际） |
| `cache_quality_acceptance.py` | R1/R2/R3；`--strict` 为 99+ 门槛 |
| `cache_join_report.py` | D1–D5 归因 |
| `post_deploy_audit_check.sh` | scp audit + 抽样 |

```bash
python3 ops/tri_cache_report_v2.py
python3 ops/deploy_schema_forensics.py

# pid → exe 补证（是否三实例均跑新二进制）
ssh panda 'for p in 4001 4002 4004; do
  pid=$(curl -sf http://127.0.0.1:$p/health | python3 -c "import sys,json; print(json.load(sys.stdin)[\"pid\"])")
  echo port=$p sha=$(sha256sum /proc/$pid/exe | awk "{print \$1}")
done'
```

## 环境变量（CCP）

| 变量 | 默认 | 说明 |
|------|------|------|
| `CCP_ICP_ENABLED` | on | ICP 管线 |
| `CCP_PROMPT_CACHE_KEY` | on | 上游 `prompt_cache_key` |
| `CCP_REASONING_SIDECAR` | on | CacheBody |
| `CCP_TRF_STRICT` | on | tools epoch |
| `CCP_ANTHROPIC_BP` | on | BBM（**未接线**） |
| `CCP_SESSION_PIN_REDIS_URL` | 空 | Redis pin |

部署脚本向 `/etc/default/zen-proxy-rs@N` 追加 `CCP_*=1`；须确认 systemd unit 是否 `EnvironmentFile=` 加载。

## panda 路径

| 路径 | 说明 |
|------|------|
| `/opt/zen-proxy-rs/zen-proxy-rs` | 二进制 |
| `/var/log/zen-proxy-rs/audit/requests-YYYY-MM-DD.jsonl` | 验收数据源 |
| `/etc/nginx/sites-available/zen-balancer` | 粘性负载均衡 |
