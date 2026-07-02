# 2026-06-22 Cache Pressure V4.113 Report

## Scope

- Client: Windows official `claude.orig.exe` through cc-switch.
- Production path: `https://sub2api.closeapi.top/` -> panda ZenProxy V4.113.
- Models: `deepseek-v4-flash`, `mimo-v2.5`.
- Constraint: no context trimming, no fake cache usage, no tool disabling, no prompt hiding, no output shortening.

## DeepSeek Result

Run:

```text
/tmp/claudecode-cache-pressure-runs/20260622-100933-claudecode-cache-pressure-deepseek-v4-flash-10k-20rpm-post-v4113-sharedprefix-full
```

Local quality:

- 100/100 ok.
- Marker seen 100/100.

Remote filtered result:

- Provider rows: 140.
- Cache observation: accepted 133, rejected 7.
- Token read pct: 94.42% using explicit read/miss tokens.
- Prefix stability: `prefix_4k_unique=1`, `prefix_32k_unique=2`, `prefix_128k_unique=2`.
- First real text/tool P50/P90/P95/P99: `2195/3055/3676/4725ms`.

Control:

- Per-request workspace negative control read pct: 7.27%.
- Negative control `prefix_32k_unique=100`.
- Interpretation: the improvement comes from stable prefix/session/workspace shape, not from usage fabrication.

## Mimo Result

Run:

```text
/tmp/claudecode-cache-pressure-runs/20260622-110328-claudecode-cache-pressure-mimo-v2.5-10k-20rpm-post-v2-safe-label-mimo-full
```

Local quality:

- 100/100 ok.
- Marker seen 100/100.
- Case coverage: text_review 35, json_summary 25, bash_inspect 25, webfetch_smoke 10, websearch_smoke 5.
- Prompt template: `cachebench-v2-safe-label`.

Remote filtered result after 60s warm-up:

- Provider rows: 139.
- Cache observation: accepted 131, rejected 8.
- Mimo explicit miss tokens are absent in accepted rows, so explicit read/(read+miss) is not meaningful.
- Conservative cache signal: `read_tokens / estimated_total_tokens = 91.08%`.
- Prefix stability: `prefix_4k_unique=10`, `prefix_32k_unique=11`, `prefix_128k_unique=11`, `prefix_256k_unique=132`.
- First real text/tool P50/P90/P95/P99: `3883/5966/6780/9000ms`.
- Total elapsed P50/P90/P95/P99: `4540/6847/7342/9611ms`.

Route validation:

- `--model mimo-v2.5` alone was not sufficient because current cc-switch provider maps Claude model fields to DeepSeek.
- Temporary provider field switch was used and restored in `finally`.
- Post-run cc-switch provider returned to DeepSeek model fields.
- No `claude.orig` orphan processes remained.

## Service Health

Post-run:

- 4001: `status=ok`, `dead=0`, `dispatch=100`.
- 4002: `status=ok`, `dead=0`, `dispatch=100`.
- 4004: `status=ok`, existing `dead=1`, `dispatch=99`; did not expand during runs.
- Public models remain: `deepseek-v4-flash`, `deepseek-v4-flash-lite`, `mimo-v2.5`.
- Critical scan found no `no proxy resources`, `lane is saturated`, `panic`, `Invalid tool parameters`, `Failed to parse JSON`, or `stream truncated before DONE`.

## Interpretation

- Controlled repeated-prefix traffic can reach 90+ without reducing quality:
  - DeepSeek: 94.42% explicit token read pct.
  - Mimo: 91.08% read/estimated after warm-up.
- This does not prove global mixed production traffic can hold 95+ or 99+.
- Current `prompt_bucket=10k` label is misleading for ClaudeCode because system/tools envelope raises remote estimated tokens to about 53k-56.5k. The next matrix must calibrate target buckets before 50rpm.

## Next Step

1. Calibrate `stable_prefix_bytes` for remote estimated token buckets near 10k, 50k, 100k, and 200k.
2. Run one model/bucket at a time at 20rpm x 5min.
3. Only consider 50rpm after local quality stays 100%, critical logs remain empty, and 4004 health does not degrade.
