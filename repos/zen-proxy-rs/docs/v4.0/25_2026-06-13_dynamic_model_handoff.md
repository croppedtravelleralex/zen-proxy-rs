# 2026-06-13 Dynamic Model Handoff

## Stop State

The user stopped the V4.109 execution and asked for documentation handoff only.
Do not continue runtime changes, NewAPI edits, model tests, deployment, or
GitHub artifact transfer from this point unless explicitly approved again.

Production channel 69 remains the stable path and must stay untouched:

```text
channel 69:
  name:     Zenproxyrs4.3
  base_url: http://172.17.0.1:4000
  models:   deepseek-v4-flash,deepseek-v4-flash-lite
```

## Last Confirmed Facts

Git state:

```text
repo:   /home/lenovo/zen-proxy-rs
branch: codex/v4109-dynamic-model-promotion-plan
state:  ahead of origin by 1 before this documentation handoff
remote: https://github.com/croppedtravelleralex/zen-proxy-rs.git
```

The intermittent push failure is explained by the HTTPS remote needing GitHub
credentials in a non-interactive environment:

```text
fatal: could not read Username ... terminal prompts disabled
```

Recommended fix before future pushes:

```text
either configure a working credential helper/token for HTTPS
or switch the remote to an SSH URL that works from this environment
```

`free-model-client-rs` had a separate untracked item:

```text
repo:  /home/lenovo/free-model-client-rs
file:  north-mini-code
state: untracked
```

Do not delete or stage that item without inspecting it in the next session.

## NewAPI Test Channel Situation

Last confirmed channel intent before the stop:

```text
channel 82:
  name:     Zenproxyrs4.108-test
  status:   enabled
  base_url: http://172.17.0.1:4010
  models:   deepseek-v4-flash,deepseek-v4-flash-lite,mimo-v2.5,nemotron-3-ultra,north-mini-code
```

During debugging, channel 82 was changed to include `ccmax` for the user's
Windows ClaudeCode test route. This was test-channel work only and must not be
copied to channel 69.

The current blocker was NewAPI group configuration, not ZenProxy routing:

```text
Windows ClaudeCode -> cc-switch
-> https://sub2api.closeapi.top/v1/messages?beta=true
-> panda NewAPI
-> HTTP 403: group ccmax deprecated
```

Evidence:

```text
cc-switch log:
  [Claude] >>> https://sub2api.closeapi.top/v1/messages?beta=true
  model=deepseek-v4-flash

NewAPI log:
  POST /v1/messages?beta=true
  user 1
  error: 分组 ccmax 已被弃用
```

The last successful database read showed NewAPI still had the restrictive
group options:

```text
AutoGroups       = ["hhhl"]
GroupRatio       = {"hhhl":0.1}
UserUsableGroups = {"hhhl":"hhhl"}
```

An attempted SQL update to restore `ccmax`, `ds`, `vip`, and `v4108-test` was
aborted by user stop. There is no success evidence for that update. Do not
assume NewAPI group configuration was fixed.

## ClaudeCode Configuration Note

Windows ClaudeCode/cc-switch was moved away from `north-mini-code` during the
debug session because real ClaudeCode requests were failing. Backups were made:

```text
C:\Users\Lenovo\.claude\settings.json.codexbak-20260612-204751
C:\Users\Lenovo\.cc-switch\cc-switch.db.codexbak-20260612-204751
```

The intended Windows daily model after that change was:

```text
deepseek-v4-flash
```

Do not make more client-side changes unless the user explicitly asks. Future
fixes should remain source-side in ZenProxy/free-model-client unless the task
is specifically to restore local client configuration.

## North Mini Code Status

`north-mini-code` passed shallow direct and NewAPI smokes after the
`tool_choice:null` forwarding fix, but it did not pass real ClaudeCode usage.

The important distinction:

```text
shallow curl/simple chat: can pass
real ClaudeCode request: failed
```

Observed real ClaudeCode failure:

```text
model seen by ZenProxy: north-mini-code
mapped upstream model:  north-mini-code-free
protocol:              Anthropic /v1/messages
stream:                true
max_tokens:            32000
tools:                 ClaudeCode tool schema present in full runs
failure:               upstream provider error status=422
surface result:        NewAPI/cc-switch 500 or 502
```

Even a minimal no-tool ClaudeCode-shaped request to `north-mini-code` failed
with upstream 422. Therefore:

```text
north-mini-code is not ClaudeCode-ready.
Do not use it as a daily ClaudeCode default.
Do not mark it dynamic_claudecode_compatible.
Do not promote it to active based on shallow tests.
```

If it remains visible on channel 82, label it as a test-only model until a
real ClaudeCode daily-development matrix passes.

## Why "Tests Passed" But Real Use Failed

The previous checks were too shallow:

```text
passed:
  /v1/models visibility
  minimal OpenAI non-stream
  minimal OpenAI stream
  minimal Anthropic non-stream

not proven:
  real ClaudeCode Anthropic request shape
  ClaudeCode tool schema
  long max_tokens=32000 behavior
  daily development task loop
  formatting, file edit, shell, and tool-call stability
```

Future acceptance must separate:

```text
model visibility
minimal transport health
OpenAI-compatible chat health
Anthropic-compatible chat health
ClaudeCode real workload health
production promotion readiness
```

## Safe Restart Checklist

Before continuing V4.109:

1. Re-read this file and `24_v4.109_dynamic_model_promotion_goal.md`.
2. Verify current NewAPI options read-only before writing anything:

```text
select key,value
from options
where key in ('AutoGroups','GroupRatio','UserUsableGroups','DefaultUseAutoGroup')
order by key;
```

3. Verify channel 69 and 82 read-only:

```text
select id,name,status,"group",models,base_url
from channels
where id in (69,82)
order by id;
```

4. Verify the token used by Windows ClaudeCode and its group.
5. Do not patch NewAPI database blindly if an admin UI/API or sync job will
   overwrite the value. First identify whether ratio sync, UI settings, or
   another process is resetting group options.
6. Do not resume `north-mini-code` ClaudeCode testing until `ccmax` access is
   fixed on the intended NewAPI instance.
7. Do not call `north-mini-code` successful until a real `claude -p` request
   succeeds through:

```text
Windows ClaudeCode
-> cc-switch
-> panda NewAPI / closeapi route
-> channel 82
-> ZenProxy 4010
-> free-model-client-rs
-> upstream
```

## Minimum Future Acceptance

For every dynamic model that is visible on channel 82, require all rows below:

```text
/v1/models visibility                 pass
OpenAI non-stream simple chat          pass
OpenAI stream simple chat              pass
Anthropic non-stream simple chat       pass
Anthropic stream simple chat           pass
Windows ClaudeCode "only OK" smoke     pass
WSL ClaudeCode "only OK" smoke         pass
ClaudeCode tool schema request         pass
small file read/write task             pass
shell command task                     pass
markdown table/code/list formatting    pass
no upstream 422/500/502                pass
no accidental production channel 69    pass
```

Only after that can a model earn `dynamic_claudecode_compatible`.

## Recommended Next Decision

When work resumes, choose one of these paths:

```text
conservative:
  remove north-mini-code from channel 82 public list
  keep channel 82 to deepseek-v4-flash, deepseek-v4-flash-lite, mimo-v2.5,
  nemotron-3-ultra
  fix NewAPI group config separately

diagnostic:
  keep north-mini-code visible only on channel 82
  fix NewAPI group config
  run the full real-ClaudeCode matrix
  quarantine north-mini-code immediately on any repeated 422

rollback:
  disable or ignore channel 82
  keep only channel 69 production stable aliases
```

The conservative path is recommended unless the user explicitly wants more
dynamic-model testing.
