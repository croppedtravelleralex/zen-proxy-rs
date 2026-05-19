# OpenCode 深度逆向与社区研究报告

> 逆向目标：sst/opencode commit `b2baddcd`（当前 opencode.ai/zen 生产环境基线）
> 补充目标：anomalyco/opencode dev 分支（后续版本变更）
> 报告日期：2026-05-19

---

## 1. 源码获取与验证

### 1.1 Repo 现状

| 信息 | 值 |
|---|---|
| 原始 repo | `opencodenetwork/opencode` — 已不可访问 |
| 当前官方 repo | `anomalyco/opencode`（16 万 star） |
| 原始冻结版本 | `sst/opencode` commit `b2baddcd` |
| 当前生产版本 | 基于 sst/opencode b2baddcd |
| License | SST License（原始）→ Apache 2.0（anomalyco） |

### 1.2 源码结构（关键文件）

之前的 session 认为文件在 `packages/server/src/`，但 repo 已经过大规模重构：

**实际路径：**
```
packages/console/app/src/routes/zen/util/    ← Zen API 后端逻辑
├── handler.ts              (1145行)  ← 主入口：编排所有流程
├── ipRateLimiter.ts         (80行)   ← IP 限流器（免费模型）
├── keyRateLimiter.ts        (75行)   ← Key 限流器（认证模型）
├── trialLimiter.ts          (70行)   ← Trial 限流器（按 IP token 总量）
├── modelTpmLimiter.ts                ← 模型 TPM 追踪（路由用，不抛错）
├── modelTpsLimiter.ts                ← 模型 TPS 质量追踪（路由用）
├── error.ts                 (35行)   ← 所有错误类型
├── dataDumper.ts                      ← 调试数据导出
├── stickyProviderTracker.ts           ← 粘滞 provider（按 sessionId）
└── provider/
    ├── provider.ts                    ← 基类
    ├── anthropic.ts, google.ts, openai.ts, openai-compatible.ts

packages/console/core/src/              ← 核心模型与配置
├── subscription.ts                     ← getFreeLimits()（SST Secrets）
├── model.ts                            ← ZenData 模型定义（SST Secrets）
├── black.ts                            ← Black 订阅限额
├── lite.ts                             ← Lite 订阅限额
└── schema/
    ├── ip.sql.ts                       ← IpRateLimitTable + IpTable
    ├── key.sql.ts                      ← KeyRateLimitTable
    └── billing.sql.ts                  ← 账单表
```

---

## 2. 核心请求处理流程

### 2.1 handler.ts 执行顺序

```
1. URL 解析 + JSON 请求体解析
2. IP 提取（x-real-ip header）
   → IPv6 截取前 4 段
3. API key 提取（parseApiKey）
   → "public" 值视为 undefined（未认证）
4. 模型验证（validateModel）
   → 检查模型存在、格式兼容、trialEnded
5. Trial 限流器（trialLimiter.check()）
   → 不抛错，只返回 trialProviders[] 或 undefined
6. 速率限制（IP 或 Key，二选一互斥）
   → allowAnonymous ? ipRateLimiter : keyRateLimiter
   → 超限抛错（429）
7. 粘滞 provider（stickyProviderTracker.get()）
   → 按 sessionId 查上回用的 provider
8. 认证（authenticate）
   → 复杂 DB join（7 表）
9. 计费验证（validateBilling）
   → 确定 BillingSource
10. 模型 TPM 限流器（modelTpmLimiter.check()）
    → 不抛错，返回 TPM 用量数据供路由选择
11. Provider 选择（selectProvider）
    → BYOK > Sticky > Trial > Weighted Hash > Fallback
12. 请求体转换 + 上游代理（fetchWith429Retry）
    → 格式转换 + 3 次 429 重试（500ms/1s/2s）
13. 响应处理 + 追踪
    → rateLimiter.track() + trialLimiter.track() + modelTpmLimiter.track()
```

### 2.2 速率限制在认证之前执行

```typescript
// handler.ts 行 122-130
await rateLimiter?.check()    // ← 先限流检查
const authInfo = await authenticate(modelInfo, zenApiKey)  // ← 后认证
```

**即使提供有效 API key，IP 已超限照常 429 拒绝。**

---

## 3. 限流系统详解

### 3.1 IP 限流器（ipRateLimiter.ts）— 免费模型

**触发条件：** `modelInfo.allowAnonymous === true`（所有免费模型）

```typescript
export function createRateLimiter(modelId, rateLimit, rawIp, request) {
  const limits = Subscription.getFreeLimits()
  const dailyLimit = rateLimit ?? limits.dailyRequests
  const isDefaultModel = !rateLimit
  const ip = rawIp.length ? rawIp : "unknown"
  const now = Date.now()
  const lifetimeInterval = ""        // 终身计数器（永不重置）
  const dailyInterval = rateLimit
    ? `${buildYYYYMMDD(now)}${modelId.substring(0, 2)}`  // 模型+日分片
    : buildYYYYMMDD(now)                                   // 日分片

  return {
    check: async () => {
      const rows = await db.select()
        .from(IpRateLimitTable)
        .where(and(eq(ip), isDefaultModel
          ? inArray([lifetimeInterval, dailyInterval])
          : inArray([dailyInterval])))
      const lifetimeCount = rows.find(r => r.interval === lifetimeInterval)?.count ?? 0
      const dailyCount = rows.find(r => r.interval === dailyInterval)?.count ?? 0

      _isNew = isDefaultModel && lifetimeCount < dailyLimit * 7

      if ((_isNew && dailyCount >= dailyLimit * 2)
        || (!_isNew && dailyCount >= dailyLimit))
        throw new FreeUsageLimitError("Rate limit exceeded", getRetryAfterDay(now))
    },
    track: async () => { /* IpRateLimitTable daily +1; if _isNew lifetime +1 */ },
  }
}
```

**关于 `checkHeaders` 的重要更正：**

当前生产版本（b2baddcd）的 `ipRateLimiter.ts` **没有** `checkHeaders` 逻辑。`dailyRequestsFallback` 也不存在。这是 `anomalyco/opencode` dev 分支后续增加的。

### 3.2 Key 限流器（keyRateLimiter.ts）— 认证模型

**触发条件：** 有效 API key + `allowAnonymous === false`

```typescript
export function createRateLimiter(modelId, rateLimit, zenApiKey, request) {
  if (!zenApiKey) return
  const LIMIT = rateLimit ?? 500    // 默认 500 次/分钟
  const interval = `${modelId.substring(0, 27)}-${YYYYMMDDHHmm}`
  return {
    check: async () => {
      const count = await db.select()
        .from(KeyRateLimitTable)
        .where(and(eq(key, zenApiKey), eq(interval, interval)))
      if (count >= LIMIT)
        throw new RateLimitError("...", 60)  // retry-after = 60s
    },
  }
}
```

**免费模型不触发 Key 限流**——`allowAnonymous=true` 强制走 IP 限流。

### 3.3 Trial 限流器（trialLimiter.ts）

**触发条件：** 模型配置了 `trialProvider` 字段

```typescript
export function createTrialLimiter(trialProviders, ip) {
  if (!trialProviders) return
  const limit = Subscription.getFreeLimits().promoTokens  // SST Secret
  return {
    check: async () => {
      const data = await db.select({ usage: IpTable.usage })
        .from(IpTable).where(eq(IpTable.ip, ip))
      _isTrial = (data?.usage ?? 0) < limit
      return _isTrial ? trialProviders : undefined
    },
    track: async (usageInfo) => {
      // 所有 token 类型累加：input+output+reasoning+cache
      const usage = inputTokens + outputTokens + reasoningTokens
        + cacheReadTokens + cacheWrite5mTokens + cacheWrite1hTokens
    },
  }
}
```

| 区别点 | Trial 限流器 | IP 限流器 |
|---|---|---|
| 数据表 | `IpTable` | `IpRateLimitTable` |
| 计量单位 | Token 总量 | 请求次数 |
| 超限后行为 | 不再走 trial provider | 返回 429 |
| 重置机制 | 永不重置 | 每天 UTC 午夜 |

### 3.4 限流参数总览

| 限流器 | 单位 | 默认阈值 | 来源 | 重置周期 | 错误类型 |
|---|---|---|---|---|---|
| IP | 请求数/天/IP | `dailyRequests`（SST Secret）| `ZEN_LIMITS` | UTC 午夜 | `FreeUsageLimitError` |
| IP(新 IP) | 请求数/天/IP | `dailyRequests × 2` | 计算值 | UTC 午夜 | `FreeUsageLimitError` |
| Key | 请求数/分钟/模型 | `rateLimit ?? 500` | 默认/模型配置 | 每分钟 | `RateLimitError` |
| Trial | Token 总量/IP | `promoTokens`（SST Secret）| `ZEN_LIMITS` | 永不 | 无（降级路由）|

---

## 4. 错误类型与响应格式

### 4.1 错误分类

```typescript
// 401 类（无 retry-after）
class AuthError extends Error {}           // 无效/缺失 API key
class CreditsError extends Error {}        // 无余额
class MonthlyLimitError extends Error {}   // 月额度超限
class UserLimitError extends Error {}      // 用户月额度超限
class ModelError extends Error {}          // 模型不可用/试用结束

// 429 类（设 retry-after）
class FreeUsageLimitError extends LimitError {} // retry-after: 到午夜 UTC
class RateLimitError extends LimitError {}      // retry-after: 60s
class BlackUsageLimitError extends LimitError {}
class GoUsageLimitError extends LimitError {}   // retry-after: 到窗口结束
```

### 4.2 retry-after 计算方式

```typescript
function getRetryAfterDay(now: number) {
  return Math.ceil((86_400_000 - (now % 86_400_000)) / 1000)
}
```

**服务端本地计算**，不是从上游响应读取。`86,400,000ms = 24h`。客户端无法修改。

### 4.3 响应体格式

```typescript
// 429 响应
{
  type: "error",
  error: { type: "FreeUsageLimitError"|"RateLimitError"|"...", message: "..." },
  metadata: {}  // GoUsageLimitError 时有 workspace + limitName
}
// Header: retry-after: N

// 401 响应（同格式，无 retry-after）
{
  type: "error",
  error: { type: "AuthError"|"ModelError", message: "..." }
}
```

---

## 5. Provider 选择算法

### 5.1 优先级

```
BYOK（用户自带 key）
  → Sticky provider（sessionId追踪，stickyProviderTracker）
    → Trial provider（从 trialProviders 随机选）
      → 加权确定性哈希（正常情况）
        → Fallback provider（重试耗尽后）
```

### 5.2 加权哈希算法

```typescript
// 1. 过滤：disabled=0, weight!=0, 未排除, TPM未超限
// 2. 找最高优先级（priority 最小值）
// 3. 按 weight 展开数组（weight=3 出现 3 次）
// 4. sessionId 末 4 字符做确定性 hash
const identifier = sessionId.length ? sessionId : ip
let h = 0
for (let i = l - 4; i < l; i++)
  h = (h * 31 + identifier.charCodeAt(i)) | 0
const index = (h >>> 0) % weighted.length
```

同一 sessionId → 同一 provider。weight 影响选中概率。

### 5.3 上游重试

```
fetchWith429Retry:
  429 + 重试 < 3 → 等 500ms → 等 1000ms → 等 2000ms → 换 provider
  MAX_FAILOVER_RETRIES = 3
```

---

## 6. 社区研究汇总

### 6.1 社区反馈

| 来源 | 发现 | 验证 |
|---|---|---|
| LINUX DO | ~5 次请求后 429，自定义 header 可缓解 | 5 次触发确认 ✅，headers 缓解未在当前版本确认 |
| LOCDD | 免费模型日限约 5 次，24h 重置 | UTC 午夜重置确认 ✅ |
| GitHub #10404 | Big Pickle 重试循环 | opencode 自身也有 3 次重试 |

**关于 `dailyRequests` 具体数值的重要说明：**
社区报告中 "约 5 次" 的测量**无法代表 `dailyRequests` 的真实 Secret 值**，原因：
- 报告者多半也在用机场/共享 IP，一个 IP 背后多用户分食同一额度，测到的 "5 次" 是多人分食后的剩余可用量
- 如果 IP 是独享的（只有自己的 ZenProxyRS 使用），实际 `dailyRequests` 可能远高于 5（例如 20、50 甚至更多）
- `dailyRequests` 的值在 SST Secrets 中，服务端可能动态调整，不存在静态值
- **唯一确定的是：IP 日额度确实存在，UTC 午夜重置，新 IP 前 `dailyLimit × 7` 次 Lifetime 请求内额度翻倍**
| GitHub #13318 | Zen 限流报告 | 不是 ZenProxyRS 特有 |
| GitHub #202 | 限流收紧警告 | Antigravity 生态，非 Zen API |

### 6.2 社区规避技术评估

| 技术 | 有效性 | 原理 |
|---|---|---|
| IP 轮换（住宅代理） | 中等 | 换 IP 重置额度，但数据中心 IP 被 ban |
| 请求节流 | 高 | 避免触发 WAF 行为检测 |
| 多模型回退 | 中 | 不同模型 rateLimit 不同 |
| 自定义 x-opencode-* 头 | 未验证 | 社区传闻，当前版本代码无此逻辑 |
| freeride 代理 | 高 | 绕过 opencode，直连其他免费 API |

---

## 7. 实际测试验证

### 7.1 测试记录

```
时间：2026-05-19 19:56 CST（11:56 UTC）
端点：opencode.ai/zen/v1/chat/completions
客户端：通过 WebShare SOCKS5 代理池

测试 1：bare minimum（无 opencode headers）
  结果：429 FreeUsageLimitError
  retry-after: 43548（约 12h 到午夜 UTC）
  content-type: text/plain;charset=UTF-8

测试 2：+ opencode headers
  结果：同样 429，同样 retry-after
  结论：headers 不影响当前版本限流

测试 3：换模型 deepseek-v4-flash-free
  结果：同样 429 FreeUsageLimitError
  结论：免费模型共享 IP 日额度

测试 4：+ x-opencode-session
  结果：同样 429
  结论：session 不绕过 IP 限流

测试 5：非免费模型 deepseek-v4-flash（需认证）
  结果：401 ModelError "not supported"
  结论：非免费模型返回不同错误
```

### 7.2 可用免费模型（通过 `/v1/models` 获取）
big-pickle, deepseek-v4-flash-free, qwen3.6-plus-free, minimax-m2.5-free, nemotron-3-super-free

---

## 8. 对 ZenProxyRS 的设计指导

### 8.1 核心结论

**IP 的日请求额度是唯一硬限制，无法绕过。**

所有其他因素（headers、指纹、API key）都是辅助性的。策略必须围绕"最大化每个 IP 的额度利用效率"展开。

### 8.2 缺陷重新评估

| # | 缺陷 | 优先级 | 理由 |
|---|---|---|---|
| 1 | 100 IP 轮询触发 WAF | **P0** | 核心架构问题 |
| 2 | 缺少 opencode headers | **P2** | 不影响额度，仅影响指纹一致性 |
| 3 | Probe 不带 API key | **P0** | 401 → 节点永不恢复 |
| 4 | 重试选新节点 | **P0** | 加速池耗尽 |
| 5 | PROXY_API_KEY 不校验 | **P1** | 安全 |
| 6 | 原始 headers 不转发 | **P2** | 指纹丢失 |

### 8.3 粘滞路由是唯一有效手段

```
无粘滞（当前）：
  100 IP 各用 1-2 次 → 各剩 ~80% 额度 → WAF 判定爬虫

有粘滞（目标）：
  1 IP 用 80 次 → 额度用完 → 换下一个 IP → WAF 判定正常用户
```

### 8.4 不需要做的事

| 不必做 | 理由 |
|---|---|
| 每个节点不同指纹 | WAF 看并发 IP 数，不是指纹多样性 |
| Probe 加 opencode headers | 纯消耗，无帮助 |
| 用 API key 绕过免费限流 | 源码确认 `allowAnonymous=true` 强制 IP 限流 |
| 逆向 `dailyRequests` 具体值 | SST Secret，不在源码，可能动态调整 |

### 8.5 UTC 与北京时间

| UTC | 北京时间 | 说明 |
|---|---|---|
| 00:00 | 08:00 | IP 额度重置 |
| 12:00 | 20:00 | retry-after 约 12h |

---

## 9. 关键源码引用

```
ipRateLimiter.ts:     https://github.com/sst/opencode/blob/b2baddcd/packages/console/app/src/routes/zen/util/ipRateLimiter.ts
keyRateLimiter.ts:    https://github.com/sst/opencode/blob/b2baddcd/packages/console/app/src/routes/zen/util/keyRateLimiter.ts
trialLimiter.ts:      https://github.com/sst/opencode/blob/b2baddcd/packages/console/app/src/routes/zen/util/trialLimiter.ts
error.ts:             https://github.com/sst/opencode/blob/b2baddcd/packages/console/app/src/routes/zen/util/error.ts
subscription.ts:      https://github.com/sst/opencode/blob/b2baddcd/packages/console/core/src/subscription.ts
handler.ts:           https://github.com/sst/opencode/blob/b2baddcd/packages/console/app/src/routes/zen/util/handler.ts
```

社区链接：
- LINUX DO: https://linux.do/t/topic/1893505/18
- rate-limit-fallback: https://github.com/liamvinberg/opencode-rate-limit-fallback
- freeride: https://github.com/stevef1uk/freeride
- Antigravity notice: https://github.com/NoeFabris/opencode-antigravity-auth/issues/202
