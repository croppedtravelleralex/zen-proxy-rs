# 2026-06-14 ClaudeCode Production Prep

## Scope

This is a production-prep checkpoint for ClaudeCode compatibility on the
dynamic V4.109 models.

2026-06-22 update: the user has since authorized production changes when they
are explicitly requested and bounded. The dev/new test domain has moved from
the old `new.closeapi.top` host to `new.relai.asia`; do not use the old host for
current testing.

Hard line:

```text
dev/test base URL: https://new.relai.asia/
production base URL: https://sub2api.closeapi.top/
```

All live testing in this phase must stay on `new.relai.asia` and the isolated
`zen-proxy-rs-v4108-test.service` path. Do not change production NewAPI,
production channel 69, or production ZenProxy service state until the user gives
a separate explicit production approval.

## Current Dev/Test Facts

Read-only checks on 2026-06-14:

```text
service: zen-proxy-rs-v4108-test.service
state:   active
health:  http://172.17.0.1:4010/health -> status=ok
channel: 83 Zenproxyrs4.108-test
base:    http://172.17.0.1:4010
models:  deepseek-v4-flash,deepseek-v4-flash-lite,
         mimo-v2.5,nemotron-3-ultra,north-mini-code,claude-haiku-4-5
```

Test service environment:

```text
NODES_FILE=/dev/null
ALLOW_DIRECT_FALLBACK=true
DYNAMIC_MODEL_PUBLIC_ALLOWLIST=mimo-v2.5,nemotron-3-ultra,north-mini-code
DYNAMIC_MODEL_CLAUDECODE_COMPAT_ALLOWLIST=mimo-v2.5,nemotron-3-ultra,north-mini-code
```

Common/prod-like environment remains conservative:

```text
NODES_FILE=/opt/zen-proxy-rs/nodes.json
ALLOW_DIRECT_FALLBACK=false
```

Implication: current dev acceptance proves model/protocol/ClaudeCode behavior
through the isolated test path, including direct fallback. It does not yet prove
that the same dynamic models are healthy through the production proxy-node pool.
That proxy-pool proof is a production gate, not a detail to hide.

## ClaudeCode Helper Alias

`claude-haiku-4-5` is intentionally a hidden helper alias that maps to
`deepseek-v4-flash-free`.

Reasoning:

- ClaudeCode may send small-model/helper requests during WebFetch, WebSearch,
  planning, summarization, or tool orchestration even when the user-selected
  main model is `mimo-v2.5`, `north-mini-code`, or `nemotron-3-ultra`.
- Mapping the helper to the already stable `deepseek-v4-flash-free` keeps
  ClaudeCode helper calls off unproven dynamic models.
- The alias is hidden from ZenProxy public `/v1/models`, but NewAPI test channel
  83 includes it so helper requests are not rejected before they reach ZenProxy.
- This is safer than mapping helper calls to the current dynamic model because
  helper traffic needs predictable short responses more than model identity.

Production note: if a production ClaudeCode route is approved, the target NewAPI
channel must either allow `claude-haiku-4-5` as a routing-only helper alias or
set ClaudeCode's `smallModel` to a model already allowed by that route. If the
channel is user-visible, adding the helper alias may make it selectable in
NewAPI UI/client model lists; prefer a ClaudeCode-specific production channel
if hiding the helper matters operationally.

## Acceptance Runner

Use the repeatable runner:

```bash
python3 scripts/run_claudecode_acceptance.py
```

Default mode is dry-run. It discovers local entrypoints and writes:

```text
test-records/runs/<run_id>/claudecode-acceptance.md
test-records/runs/<run_id>/claudecode-acceptance.json
```

Suites:

```text
smoke: Bash + WebFetch + WebSearch, useful for quick routing checks
core:  local ClaudeCode tools only, excluding WebFetch/WebSearch/Task
full:  27 default base tool scenarios expanded across text/json/stream-json
```

The full dry-run matrix is 81 selected cases before model/platform expansion,
or 486 planned items for the default 3 models across Windows + WSL. Covered
default tool names are Bash, Read, Glob, Grep, Write, Edit, MultiEdit,
TodoWrite, NotebookRead, NotebookEdit, WebFetch, WebSearch, and Task. `LS`
remains in the runner as an explicit diagnostic case, but is excluded from the
default suites because ClaudeCode 2.1.143 did not register any tool schema for
`--tools LS`, `--tools Ls`, or `--tools ls` in print mode. Mutable file tools
run in a per-case temporary workspace and write/edit cases include post-run
workspace checks. For core/full file-tool execution, prefer
`--permission-mode bypassPermissions`; the workspace is temporary and this
avoids non-interactive permission prompts being mistaken for model failures.

Execute WSL-only quick smoke:

```bash
ANTHROPIC_API_KEY="[runtime env]" python3 scripts/run_claudecode_acceptance.py \
  --execute \
  --platform wsl \
  --suite smoke \
  --output-formats json \
  --models mimo-v2.5 north-mini-code nemotron-3-ultra \
  --base-url https://new.relai.asia
```

Execute the Windows official ClaudeCode matrix from Windows PowerShell:

```powershell
cd \\wsl.localhost\HermesUbuntu\home\lenovo\zen-proxy-rs
$env:ANTHROPIC_API_KEY = "[runtime env]"
py scripts\run_claudecode_acceptance.py `
  --execute `
  --platform windows `
  --suite full `
  --permission-mode bypassPermissions `
  --models mimo-v2.5 north-mini-code nemotron-3-ultra `
  --base-url https://new.relai.asia `
  --timeout 300
```

Execute the WSL clawgod matrix from WSL:

```bash
cd /home/lenovo/zen-proxy-rs
ANTHROPIC_API_KEY="[runtime env]" python3 scripts/run_claudecode_acceptance.py \
  --execute \
  --platform wsl \
  --suite full \
  --permission-mode bypassPermissions \
  --models mimo-v2.5 north-mini-code nemotron-3-ultra \
  --base-url https://new.relai.asia \
  --timeout 300
```

The report stores command shape, exit/status, timing, byte counts, hashes,
marker checks, output format, inferred tool-call count, workspace check
results, and ClaudeCode turn metadata only. It does not store API keys, raw
prompts, raw completions, or tool outputs.

Important counting note: ClaudeCode WebFetch/WebSearch here are local client
tools, not Anthropic server-side web tools. `usage.server_tool_use.*` can remain
zero even when the tool was invoked. Treat `num_turns >= 2`, final markers, and
workspace checks as the runner's primary tool-execution evidence.

Safety defaults:

- API key is read from `ANTHROPIC_API_KEY` or `--api-key-env`.
- The runner refuses execute mode against `sub2api.closeapi.top` unless
  `--allow-production-base-url` is explicitly passed.
- WSL clawgod runs with a temporary `$HOME/.clawgod/provider.json`, then deletes
  it. The global `/home/lenovo/.clawgod/provider.json` is not modified.
- When started from WSL, Windows platform cases are skipped by default and
  return non-zero. When started from Windows, WSL platform cases are skipped by
  default and return non-zero. Run each platform matrix on its native host.
- Do not use ClaudeCode `--bare` for WebFetch/WebSearch acceptance because it
  can hide those tools. The runner isolates mutable user state with temporary
  Windows `USERPROFILE` or WSL `HOME` instead.

## Runner Validation 2026-06-14

Fresh validation for `scripts/run_claudecode_acceptance.py`:

```text
python3 -m py_compile scripts/run_claudecode_acceptance.py: passed
dry-run --platform both for all 3 models, full suite, and all 3 output formats:
  selected cases 81, planned matrix items 486
execute --platform windows:
  mimo-v2.5         Bash pass, WebFetch pass, WebSearch pass
  north-mini-code   Bash pass, WebFetch pass, WebSearch pass
  nemotron-3-ultra  Bash pass, WebFetch pass, WebSearch pass
execute --platform wsl:
  mimo-v2.5         Bash pass, WebFetch pass, WebSearch pass
  north-mini-code   Bash pass, WebFetch pass, WebSearch pass
  nemotron-3-ultra  Bash pass, WebFetch pass, WebSearch pass
execute --platform windows from WSL:
  skipped by default and returns non-zero
execute --platform wsl from Windows:
  skipped by default and returns non-zero
production base guard:
  execute against sub2api.closeapi.top refused before running cases
```

The generated evidence paths are under ignored `test-records/runs/`:

```text
20260614-windows-native-full-v4/
20260614-wsl-native-full-v4/
dryrun-production-block-3/
```

This runner validation supersedes the earlier manual full matrix for the dev
route. Before production, rerun both native matrices freshly and then perform a
real proxy-pool canary.

## Dev Test Deployment 2026-06-15

Only the isolated dev/test service was changed:

```text
service: zen-proxy-rs-v4108-test.service
path:    /opt/zen-proxy-rs-v4108-test/zen-proxy-rs
binary:  4fe3bb9af196cd34e60bf81069d48bba501ce6a9
backup:  /opt/zen-proxy-rs-v4108-test/zen-proxy-rs.bak.20260615000841
health:  http://172.17.0.1:4010/health -> status=ok
```

Production was not changed:

```text
production binary: /opt/zen-proxy-rs/zen-proxy-rs
production SHA1:   a9244338d9a24cc40ebab0d5274b2916421b40ff
```

Code-level validation before deployment:

```text
free-model-client-rs:
  cargo test forced_tool_choice
  cargo test anthropic_system_content_blocks_are_normalized_before_upstream
  cargo test upstream_429_is_returned_as_rate_limit_error
  cargo test anthropic_claude_code_stream_rate_limit_fast_fails_as_error_event
zen-proxy-rs:
  cargo build --release
  cargo test v4::model -- --test-threads=1
  cargo test --test e2e_integration test_dynamic_public_allowlist_filters_test_channel_models -- --test-threads=1
```

Implemented in the deployed dev binary:

```text
- ClaudeCode forced tool_choice is downgraded to upstream auto for:
  mimo-v2.5, north-mini-code, nemotron-3-ultra
- Anthropic system content blocks are normalized before OpenAI-upstream calls.
- ClaudeCode stream fetch 429/provider_rate_limited returns a stream
  rate_limit_error immediately from the proxy guard instead of doing three
  internal guard retries. ClaudeCode may still retry the whole request itself.
```

Post-deploy live evidence on the dev/new domain:

```text
WSL ClaudeCode, nemotron-3-ultra, Bash/json:
  run 20260614-postdeploy-wsl-bash-json-nemotron -> pass
Windows ClaudeCode, nemotron-3-ultra, Bash/json:
  run 20260615-postdeploy-windows-bash-json-nemotron -> pass
WSL ClaudeCode, nemotron-3-ultra, Read/json:
  read_text pass, read_offset pass, read_csv pass
WSL ClaudeCode, nemotron-3-ultra, Glob/json:
  glob_py pass, glob_markdown pass, glob_txt timeout_no_output
WSL ClaudeCode, nemotron-3-ultra, Grep/json:
  grep_plain once reached tool execution but missed marker, later timed out;
  grep_regex and grep_include timed out.
WSL ClaudeCode, mimo-v2.5 and north-mini-code, Bash/json:
  blocked by dev upstream provider rate limiting, visible in 4010 logs as
  "upstream provider rate limited the request".
```

Current conclusion: `nemotron-3-ultra` has verified Windows + WSL ClaudeCode
Bash tool closure and WSL Read closure. `mimo-v2.5` and `north-mini-code` cannot
be honestly re-certified while the dev upstream is returning rate limits.
`Glob/Grep` need further prompt/model-behavior stabilization before claiming
full local-tool coverage. Do not promote to production from this checkpoint.

## Current Live Acceptance Evidence

Before this production-prep document, the following real ClaudeCode matrix was
manually verified on the dev/test route:

```text
Windows official ClaudeCode C:\Users\Lenovo\.local\bin\claude.orig.exe:
  mimo-v2.5         Bash pass, WebFetch pass, WebSearch pass
  north-mini-code   Bash pass, WebFetch pass, WebSearch pass
  nemotron-3-ultra  Bash pass, WebFetch pass, WebSearch pass

WSL clawgod launcher /home/lenovo/.local/bin/claude with temporary HOME/provider:
  mimo-v2.5         Bash pass, WebFetch pass, WebSearch pass
  north-mini-code   Bash pass, WebFetch pass, WebSearch pass
  nemotron-3-ultra  Bash pass, WebFetch pass, WebSearch pass
```

Operational note: Windows `mimo-v2.5` WebFetch can be slow, around 193 seconds
in the observed run. Treat `>180s` WebFetch as a slow pass if the marker is
present within the configured timeout; do not classify it as a protocol failure
without corroborating logs.

Recommended log correlation after a run:

```bash
ssh panda 'journalctl -u zen-proxy-rs-v4108-test.service --since "30 minutes ago" --no-pager |
  grep -E "model=(mimo-v2.5|north-mini-code|nemotron-3-ultra)|tool_name_classes|completion summary"'
```

Expected tool classes:

```text
Bash      -> tool_name_classes=["shell"]
WebFetch  -> tool_name_classes=["web_fetch"]
WebSearch -> tool_name_classes=["web_search"]
```

## Production Promotion Package

When the user explicitly approves production, prepare one small change package:

```text
1. Verify production channel and route read-only before writing anything.
2. Deploy the already tested ZenProxy binary/config to the intended production
   service only after hash verification.
3. Keep ALLOW_DIRECT_FALLBACK=false in production.
4. Keep NODES_FILE on the real production nodes file.
5. Add only the approved dynamic public models:
     mimo-v2.5
     north-mini-code
     nemotron-3-ultra
6. Add or otherwise allow the ClaudeCode helper alias:
     claude-haiku-4-5 -> deepseek-v4-flash-free
7. Keep deepseek-v4-flash-lite restricted to Hermes/OpenClaw behavior only.
8. Do not expose raw upstream ids ending in -free as public model names.
```

Recommended gray order:

```text
phase 1: mimo-v2.5
phase 2: north-mini-code
phase 3: nemotron-3-ultra
```

Each phase must re-run the ClaudeCode matrix on `new.relai.asia` first, then
run a bounded production canary only after approval.

## Production Gates

Do not promote to `sub2api.closeapi.top` until all are true:

```text
dev/test Windows + WSL runner matrix passes freshly
dev/test logs show expected tool_name_classes for Bash/WebFetch/WebSearch
production service path has real proxy nodes and ALLOW_DIRECT_FALLBACK=false
bounded proxy-pool smoke passes without direct fallback
deepseek-v4-flash and deepseek-v4-flash-lite regressions are zero
production channel/current route is verified read-only immediately before write
rollback commands are prepared and tested on the test service
```

The 2026-06-15 dev source-mode audit found that the previous test instance
shape was not enough for production confidence. The root problem is not a
"wait until stable" issue:

```text
old dev/test source mode:
  NODES_FILE=/dev/null
  ALLOW_DIRECT_FALLBACK=true
  selected_node_id=direct

observed result:
  deepseek-v4-flash 429 on a minimal Anthropic /v1/messages request
  mimo-v2.5         429
  north-mini-code   429
  nemotron-3-ultra  200
```

The direct source is a single upstream/free-usage path and can be rate-limited
at the provider. That explains why `mimo-v2.5` and `north-mini-code` collapsed
under ClaudeCode retries, and why treating the failure as "wait for stability"
was wrong.

The follow-up test with the real node file exposed a second source problem:

```text
strict node-pool mode:
  NODES_FILE=/opt/zen-proxy-rs/nodes.json
  ALLOW_DIRECT_FALLBACK=false

observed result:
  90-node external SOCKS pool produced transport_error / 502 for the minimal
  Anthropic requests; health moved many nodes to dead.
```

Current dev/test compromise after the audit:

```text
NODES_FILE=/opt/zen-proxy-rs/nodes.json
ALLOW_DIRECT_FALLBACK=true
DYNAMIC_MODEL_PUBLIC_MODE=candidate_canary_or_active
DYNAMIC_MODEL_PUBLIC_ALLOWLIST=mimo-v2.5,nemotron-3-ultra,north-mini-code
```

This keeps the test service from failing only with 502, but it still proves the
source pool is not production-ready. A new guard now quarantines unproven
candidate models after 3 public traffic failures. Live result on
the dev/new domain:

```text
mimo-v2.5-free       quarantined after 3 upstream_429 failures
north-mini-code-free quarantined after 3 upstream_429 failures
nemotron-3-ultra     remains candidate after 1 minimal 200
/v1/models now:      deepseek-v4-flash, deepseek-v4-flash-lite, nemotron-3-ultra
```

Do not run full ClaudeCode matrices for `mimo-v2.5` or `north-mini-code` until a
non-direct, healthy source path is available and bounded probes pass.

## Rollback

If production promotion is later approved and then fails:

```text
1. Remove dynamic model names from the production NewAPI route/channel:
     mimo-v2.5,north-mini-code,nemotron-3-ultra,claude-haiku-4-5
2. Leave only stable aliases:
     deepseek-v4-flash,deepseek-v4-flash-lite
3. Clear production DYNAMIC_MODEL_CLAUDECODE_COMPAT_ALLOWLIST.
4. Set DYNAMIC_MODEL_PUBLIC_MODE=static_only or clear dynamic public exposure.
5. Keep ALLOW_DIRECT_FALLBACK=false.
6. Restart only the intended production service.
7. Verify /v1/models and one stable ClaudeCode smoke.
8. If binary rollback is needed, restore the previous verified binary hash and
   restart again.
```

If the failure is helper-only, first remove or remap `claude-haiku-4-5` and
retest WebFetch/WebSearch. Do not demote the three dynamic main models solely
because a helper route was misconfigured.
