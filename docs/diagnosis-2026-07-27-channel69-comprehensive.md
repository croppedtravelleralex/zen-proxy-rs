# 渠道 69 全量诊断报告

- 日期：2026-07-27
- 范围：NewAPI channel 69 → nginx `:4000` → zen-proxy-rs@1/@2/@4 → 100 Webshare 节点 → opencode.ai/zen
- 数据来源：NewAPI PostgreSQL logs（7d 19,654 行）+ zen-proxy-rs audit `/var/log/zen-proxy-rs/audit/requests-*.jsonl`（7d 19,622 条 per-request 记录，62 字段 + 4 嵌套对象）
- **修正**：本文档推翻了 [plan-2026-07-22-zenproxy-reasoning-only-retry.md](./plan-2026-07-22-zenproxy-reasoning-only-retry.md) 的三处错误结论（见 §0）

---

## 0. 上一轮结论修正

|上一轮结论|本轮 7d per-request audit 数据|裁决|
|---|---|---|
|「TTFB 慢在 reasoning 阶段，中间 2.4s 花在思维链」|`first_content_ms − first_chunk_ms` = **322ms**。瓶颈不在 reasoning，在上游出第一个 SSE chunk 之前|**错**|
|「10 秒是重试等待，每天浪费 26.5 分钟」|4,294 条 `empty_output` 请求的 `retry_count` = **100% 为 0，从不重试**；10s 是 big-pickle 的 `upstream_response_ms` p50 = 10,006ms|**错**|
|「缓存命中率低」|7 天 `cache_tokens/prompt_tokens` 稳定 **77–85%**，无一天下滑|**错**|

**根因**：上一轮只用了 metrics 聚合（`zen_proxy_requests_by_model_total` 等），而 `by_model` / `by_outcome` / `by_body_bucket` 是三个**互斥的独立计数器**，无法交叉分析。本轮改用 per-request audit 日志，才拿到真正可交叉的数据。

---

## 1. 渠道 69 模型清单

|模型|7d 请求数|占比|
|---|---|---|
|deepseek-v4-flash|19,383|98.6%|
|mimo-v2.5|145|0.7%|
|big-pickle|73|0.4%|
|hy3|53|0.3%|

deepseek-v4-flash 占绝对主体。mimo / big-pickle / hy3 样本量极小（<150），分位数不具统计意义，本报告结论仅基于 deepseek。

`channel_name` 在库里为空字符串（"ocrs" 只是 UI 显示名）。`cache_creation_tokens` 全模型恒为 0 —— 缓存创建从不记账。

---

## 2. 延迟分解：TTFB 是瓶颈

### 2.1 时间去哪了（deepseek, 7d, ms）

|阶段|p50|p90|p99|说明|
|---|---|---|---|---|
|`upstream_response_ms`|**26**|154|10,029|zen-proxy → 代理节点 → 上游，TCP+TLS 全程|
|`first_chunk_ms`|**4,753**|13,923|61,207|上游吐出第一个 SSE chunk|
|`first_content_token_ms`|5,075|11,486|39,660|第一个正文 token|
|`total_ms`|7,080|17,718|67,680|全程|

**链路 26 毫秒**（p50）。首字的 4.7 秒全部在上游模型出第一个 chunk 之前，zen-proxy 侧无事可做。

`first_content − first_chunk` = 322ms（p50）。**不存在 reasoning 瓶颈。**

### 2.2 NewAPI 侧 TTFB 口径

|指标|deepseek p50|
|---|---|
|TTFB（`frt`，无首包哨兵 -1000 剔除）|5,070ms|
|use_time（全程，秒）|8s|
|TTFB 占全程比例|**73.8%**|
|生成段（use_time - TTFB）|2.2s|

### 2.3 按天 TTFB 趋势

|日期|TTFB p50|TTFB p95|
|---|---|---|
|07-19|5,927|15,279|
|07-20|4,781|12,160|
|07-21|5,612|14,606|
|07-22|4,789|17,031|
|07-23|4,873|18,006|
|07-24|5,620|**26,399**|
|07-25|4,576|13,547|
|07-26|5,224|14,591|

P50 稳定在 4.5–5.9 秒无恶化。波动的在 P95（07-24 冲高到 26.4 秒）。**"慢"是恒定基线，"抽风"只影响尾部。**

---

## 3. Empty Output 深度分析（核心问题）

### 3.1 总览

|窗口|total|success|empty_output|EO 占比|
|---|---|---|---|---|
|7d|19,359|15,235|**4,055**|**20.9%**|
|24h|3,789|3,284|499|**13.2%**|

### 3.2 EO 完整机制

从 audit `retry_chain` 字段确认（7d 全部 4,294 条 EO 只有这一种形态）：

```json
[{"attempt":0,"node_id":"72254553","status":200,"latency_ms":9,   "outcome":"success"},
 {"attempt":0,"node_id":"72254553","status":200,"latency_ms":3772,"outcome":"empty_output"}]
```

两个 entry 的 `attempt` 都是 0、`node_id` 相同。**这不是重试**，是同一次尝试的响应头阶段（9ms, 200 OK）和响应体阶段（3,772ms, 空流）分别记账。

真实流程：
1. 上游 9ms 即返回 HTTP 200
2. SSE 流跑了 3,772ms（p50）
3. 一个正文 token 都没有
4. zen-proxy **直接放弃，不换节点、不重试**

### 3.3 EO 关键特征

|特征|数据|含义|
|---|---|---|
|流式|**100%** `is_streaming=true`（4,065/4,065）|非流式 869 条零 EO|
|`completion_tokens` p50|**0**|确实一个 token 都没产出|
|`prompt_tokens` p50（EO）|28,595|成功请求的 27%（106,790）|
|重试次数|**100% 为 0**|从不自动恢复|
|`empty_output_class` 字段|**不存在于 audit schema**|无法从运行时侧区分 reasoning_only vs 其它|

NewAPI 侧的错误分类（7d）：

|错误类别|次数|占比|24h 趋势|
|---|---|---|---|
|reasoning_only|2,024|48.9%|**单调上涨**（107→413）|
|No provider available|1,033|24.9%|已解决（419→2）|
|context_length_exceeded|571|13.8%|波动（11→78）|
|其它|312|7.5%|存疑|
|stream fetch timeout|84|2.0%|已归零|
|empty_output|61|1.5%|已归零|
|do request failed|50|1.2%|已归零|

reasoning_only 是**唯一还在涨的类别**。24h 内错误已收敛到单独一个问题：**reasoning_only 占 83.4%**。

### 3.4 按请求大小的 U 型分布（deepseek 7d）

|body_bucket|请求数|缓存命中率|EO 率|EO 贡献数|
|---|---|---|---|---|
|tiny|5,075|22.5%|**32.1%**|1,627（40%）|
|small|2,757|66.5%|17.8%|490|
|medium|3,169|67.1%|15.2%|482|
|large|3,361|82.9%|**10.3%**|347|
|huge|4,998|74.7%|22.2%|1,110|

EO 不是单调的：极短和极长都容易空输出。tiny 桶单独贡献 40% 的 EO。huge 桶 EO 率 22.2% 是第二个异常峰。

### 3.5 复发统计

|维度|Top1|Top1 值|Top10 合计|特征|
|---|---|---|---|---|
|prompt_hash|`aaee68640ae3a62a`|41 次|168 次|长尾为主（1,108 唯一 hash）|
|session_id|`ses_90168b8b22cdbbca`|144 次|599 次|特定会话会密集踩坑|

### 3.6 失败的耗时

|错误类别|p50|p90|p99|
|---|---|---|---|
|reasoning_only|**3s**|7s|54s|
|no_provider|**2s**|4s|16s|
|context_length_exceeded|**17s**|29s|52s|
|stream_timeout|**88s**|92s|94s|
|其它|**24s**|134s|300s|

reasoning_only 和 no_provider 都是快速失败（p50 2–3 秒），对体感冲击小。真正折磨人的 stream_timeout（p50 88 秒卡满超时）和"其它"类（p99 300 秒撞硬上限）24h 内已归零。

### 3.7 重试预算耗尽

|指标|值|
|---|---|
|发生数|12 条|
|`total_ms` p50|**264,823ms（4.4 分钟）**|
|后果|长时间占住连接，是最昂贵的失败模式|

只有 12 条，但一旦发生就极其痛苦。重试逻辑需要总预算上限。

---

## 4. 缓存分析（命中率不低，但有一个真缺陷）

### 4.1 口径

渠道 69 的 `prompt_tokens` **已包含**缓存读取 token（7d 中 11,771 条 `cache_tokens>0` 的记录里，`cache_tokens > prompt_tokens` 的行数为 **0**）。命中率 = `cache_tokens / prompt_tokens`。

`cache_creation_tokens` 全模型恒为 0，缓存创建从不记账。

### 4.2 按模型

|模型|请求数|命中率|0 命中占比|高命中(>80%)占比|
|---|---|---|---|---|
|deepseek-v4-flash|15,272|**81.89%**|23.91%|68.29%|
|mimo-v2.5|142|**98.72%**|16.90%|78.87%|
|hy3|44|**49.34%**|45.45%|38.64%|
|big-pickle|61|**0.46%**|86.89%|1.64%|

### 4.3 按天趋势（deepseek）

|日期|请求数|命中率|0 命中占比|
|---|---|---|---|
|07-19|36|73.56%|50.00%|
|07-20|996|77.06%|41.87%|
|07-21|1,460|84.09%|27.12%|
|07-22|2,371|80.87%|22.61%|
|07-23|2,012|77.08%|27.83%|
|07-24|2,765|83.00%|23.22%|
|07-25|2,554|84.58%|18.13%|
|07-26|3,324|81.66%|21.54%|

**没有任何一天掉下去。** 用户"缓存命中率低"的主观感受与数据不符。

### 4.4 双峰分布

23.91% 请求 0 命中 + 68.29% 高命中（>80%），中间地带仅约 7.8%。均值的 81.89% 掩盖了"要么全中、要么全不中"的真实形态。用户撞上零命中那一拨就会觉得缓存没生效。

### 4.5 按 prompt 大小分桶（deepseek）

|prompt 区间|请求数|命中率|0 命中占比|
|---|---|---|---|
|<1k|708|**6.15%**|**94.63%**|
|1k–10k|569|70.18%|36.91%|
|10k–50k|3,532|**53.20%**|**50.06%**|
|>50k|10,709|**82.88%**|**10.27%**|

"小请求本来就命不中"成立（<1k 桶 94.63% 零命中）。但 **10k–50k 桶异常**：50.06% 零命中，比更小的 1k–10k 桶（36.91%）还差，打破了单调规律。这个桶值得单独排查。

### 4.6 cache key 稳定性

|模型|unique usk|unique session_id|比值|
|---|---|---|---|
|deepseek-v4-flash|1,699|1,699|**1.00**|
|hy3|18|18|1.00|
|big-pickle|15|15|1.00|
|mimo-v2.5|2|2|1.00|

四模型全部 1:1 精确相等。`prefix_drift` 漂移率仅 0.23%。**上一轮担心的 `DefaultHasher` 和 cache key 漂移问题不存在。** 优先级应下调。

### 4.7 `ccp_raw_prefix_match_32k` = 0%（真缺陷）

7d 19,623 条记录，**没有任何一条为 True**。7 月 6 日文档记录的「三模型均为 0%」到今天变成「四模型均为 0%」，二十天没变过。

附带发现：短请求下 `ccp_prefix_4k/32k/128k/256k` 四个哈希取值**完全相同**，分层前缀匹配对 tiny 桶没有任何区分能力。

### 4.8 affinity 几乎不工作

|模型|session_pin 命中率|affinity 命中率|
|---|---|---|
|deepseek-v4-flash|**68.1%**|**12.1%**|
|mimo-v2.5|**95.9%**|**0.0%**|
|big-pickle|**64.6%**|**0.0%**|
|hy3|**50.9%**|**3.8%**|

pin 走 Redis（跨实例共享）→ 68%；affinity 走进程内 `RwLock<HashMap>` → 12%。这个对比证实了 `dispatch.rs:579` 的代码层面判断。**pin 是唯一有效的路由稳定机制。**

---

## 5. 节点行为

### 5.1 负载分布不均

|指标|值|
|---|---|
|活跃节点|94/100（6 个 7d 零流量）|
|头号节点（de3e98b8）|1,360 请求|
|均值（均匀分布）|209|
|头号/均值比|**6.5 倍**|

头号节点拿到 6.5 倍于均值的流量。T50 及以上节点的分布明显偏向头部。

### 5.2 坏节点明细（EO 率 ≥50 请求节点 Top12）

|node_id|请求数|empty_output|EO 率|相对全局倍数|
|---|---|---|---|---|
|5fdb9c57|62|37|**59.7%**|2.9×|
|263ea40b|92|48|**52.2%**|2.5×|
|444ed780|70|34|**48.6%**|2.3×|
|86437f51|81|37|**45.7%**|2.2×|
|440da187|203|87|**42.9%**|2.0×|
|336178b2|78|32|**41.0%**|2.0×|
|29204ffd|127|52|**40.9%**|2.0×|
|4ad51ff3|74|30|**40.5%**|1.9×|
|e141fcd3|246|97|**39.4%**|1.9×|
|e9101d22|168|64|**38.1%**|1.8×|
|adce09ce|132|47|**35.6%**|1.7×|
|3981066c|332|114|**34.3%**|1.6×|

全局 EO 率 20.9%。最差节点 59.7%（2.9 倍）。

### 5.3 无摘除机制

`zen_proxy_pool_transitions` 三实例全为 **0**。dead 池只有 0–2 个。

原因：上游返回的是 HTTP 200，健康检查看不出任何问题。`pool_transitions=0` 说明没有任何节点被移出过 dispatch。坏节点会被永久留在池子里持续分发。

---

## 6. 错误收敛趋势

|错误类别|07-20|07-21|07-22|07-23|07-24|07-25|07-26|
|---|---|---|---|---|---|---|---|
|reasoning_only|107|165|310|336|284|407|**413**|
|No provider available|0|0|274|284|419|54|**2**|
|context_length_exceeded|11|22|104|69|162|126|78|
|stream fetch timeout|15|0|0|11|53|5|**0**|
|empty_output|10|0|0|2|20|29|**0**|
|do request failed|0|50|0|0|0|0|**0**|

整体错误率：27.33%（07-24）→ 12.87%（07-26）。**已有修复全部生效。** 唯一还在涨的是 `reasoning_only`。

---

## 7. 长尾事件

Top 18 条长尾（`completion_tokens=0`、`type=5`、非流式、`use_time` 撞 300 秒硬超时）**全部集中在 07-20 10:17–11:37 这 80 分钟**。是一次孤立故障爆发，之后 6 天未复现。

与负载无关的证据：

|时段|请求数|错误率|
|---|---|---|
|13–14 点（峰值，382/402 请求）|382–402|9.4%–11.4%|
|17 点（181 请求）|181|**30.9%**|

**不是过载导致的抽风。**

---

## 8. 三实例一致性

|指标|@1|@2|@4|
|---|---|---|---|
|请求量|1,974|1,964|1,963|
|EO 率|13.5%|14.6%|13.6%|
|avg_latency_ms|9,419|8,877|9,180|

极差 <1.1pp。负载均衡正常，**不是实例级问题。**

---

## 9. 根因优先级矩阵

|优先级|问题|证据|影响|修改量|前置条件|
|---|---|---|---|---|---|
|P0|部署链路断裂|`version: "0.2.0"`，无 commit|所有修复卡在上线阶段|小|—|
|P0|EO 不重试、不换节点|4,294 条 EO `retry_count=0`|20.9% 直接失败|中|P0 部署|
|P0|坏节点无熔断|`pool_transitions=0`，最差节点 59.7%|持续污染 20–60% 节点|中|P0 部署|
|P1|10k–50k 桶缓存异常|50.06% 零命中，比 1k–10k 桶（36.91%）差|缓存感受偏差|待归因|P0|
|P1|`ccp_raw_prefix_match_32k=0%`|19,623/19,623 False，20 天未变|缓存上限被锁死|中|P0|
|P2|affinity 12% vs pin 68%|进程内 HashMap vs Redis|路由不够稳定|中|P1 缓存|
|P2|节点负载 6.5 倍倾斜|头号 1,360 vs 均值 209|放大坏节点影响|小|P0|
|P3|tiny 桶双重恶化|EO 32.1% + 命中率 22.5%|40% EO 源于此|需先归因|P0|
|—|TTFB 4.7s 在上游|`upstream_response_ms` p50 = 26ms|体感主因|**不可修**|—|
|—|reasoning 瓶颈|`first_content − first_chunk` = 322ms|不存在这个瓶颈|—|—|
|—|10s 硬编码 sleep|不存在，是 big-pickle upstream 超时|不存在这个 bug|—|—|
|—|三实例差异|EO 极差 1.1pp|不存在|—|—|
|—|过载问题|峰值时段错误率最低（9.4%）|不是过载|—|—|

---

## 10. 实施路线

### 阶段 0：部署链路恢复（阻塞项）

1. 把线上二进制对应的工作区改动回流成 commit（HEAD 在 `250043d`，panda 二进制 mtime 07-15，源码 HEAD 07-03）
2. 在 `Cargo.toml` 加 vergen，把 git hash + 构建时间嵌入 `/health` 和 `/metrics`
3. 确认 GitHub Actions → GHCR → panda pull 的通路或恢复 GitHub release asset 发布
4. 用一次无害改动验证完整链路

**不做完这个，后面所有代码修复都可能被下次部署静默覆盖。** 这是"过往都失败"的机制性解释。

### 阶段 1：EO 治理（收益最大）

**1.1 EO 触发换节点重试**
- 检测到 SSE 流结束但 `content_tokens == 0` → 换一个节点重试（非原地）
- 最多重试 2 次，设总 budget 上限，参考现有 `retry_budget_exhausted` 4.4 分钟的教训
- 理论上可将 20.9% → ~4.4%

**1.2 补 `empty_output_class` 字段**
- audit 日志里加这个字段，区分 `reasoning_only` vs 其它
- 解决当前无法交叉验证 2,024 次 reasoning_only 和 EO 总量关系的问题

**1.3 节点熔断**
- 按 node 统计滑动窗口 EO 率
- 超阈值（建议 40%，覆盖现有 8 个坏节点）临时移出 dispatch 池
- 冷却后半开恢复

**1.4 修负载倾斜**
- dispatch 权重检查

### 阶段 2：缓存专项（P1）

**2.1 `ccp_raw_prefix_match_32k=0%`**
- 取真实请求，本地 canonical prefix 与实际 raw body 逐字节 diff
- 归因：字段顺序？空白？动态字段混入？

**2.2 10k–50k 桶异常**
- 归因：这个桶的请求结构有什么特殊之处？
- 先查请求模式再做修复

### 阶段 3：路由优化（P2）

**3.1 affinity 接 Redis**
- 与 session_pin 统一，从进程内 HashMap 迁移到 Redis
- 12.1% → 有望接近 pin 的 68%

**3.2 `DefaultHasher` → SHA-256**
- 优先级低：实测 USK 1:1 稳定

### 明确不做

|方向|理由|
|---|---|
|优化 TTFB 本身|链路 26ms，4.7s 在上游，改不动|
|优化 reasoning 阶段|只有 322ms，不存在这个瓶颈|
|找 10s 硬编码 sleep|不存在，是 big-pickle 超时|
|排查三实例差异|极差 1.1pp|
|按负载扩容|峰值错误率最低（9.4%）|
|重写 Rust 架构|38h 零重启零 panic|
|为 mimo/big-pickle/hy3 单独优化|7d 样本 145/73/53，占 1.4%|
|硬修 hy3|24h 零流量|
|修 cache key 漂移|USK 1:1 稳定，不存在|

---

## 11. 数据源缺口

当前 audit 日志里**不存在**的几个字段，导致以下问题无法回答：

|缺失字段|影响|
|---|---|
|`empty_output_class`|无法验证 EO 中有多少是 reasoning_only|
|`first_reasoning_ms`|无法精确测量 reasoning 阶段耗时（只能用 `first_content − first_chunk` 间接估）|
|实例标识（port/pid）|三实例对比只能靠 `/metrics`，无法 per-request 归因|
|accepted/rejected 缓存分类|无法按模型拆分缓存拒绝原因|
|延迟直方图|metrics 只有 `avg_latency_ms` 单一 gauge，无 p50/p90/p99|
|per-node metrics|无法从 metrics 获取 per-node EO 率|

---

## 12. 发布清单

|操作|命令|
|---|---|
|健康检查|`for p in 4000 4001 4002 4004; do echo -n ":$p "; curl -sS -m 3 http://127.0.0.1:\$p/health; echo; done`|
|服务状态|`systemctl is-active zen-proxy-rs@1 zen-proxy-rs@2 zen-proxy-rs@4`|
|重试日志|`journalctl -u 'zen-proxy-rs@*' --since '30 min ago' --no-parser \| grep -E 'retrying after completed reasoning-only' \| tail -40`|
|EO 日志|`journalctl -u 'zen-proxy-rs@*' --since '30 min ago' --no-pager \| grep 'empty_output_class="reasoning_only"' \| tail -20`|
|审计查询|`python3 -c "import json, sys; from collections import Counter; c=Counter(); [c.update([l.get('outcome')]) for l in map(json.loads, sys.stdin) if '2026-07-27' in l.get('ts','')]; print(c)"`|

---

## 13. 变更日志

|时间|更新|
|---|---|
|2026-07-27|初版。基于 NewAPI 渠道 69 的 7d 数据 + zen-proxy-rs audit per-request 分析。修正上一轮 3 处错误。|
