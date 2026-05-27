# V4.5 P8 Acceptance Matrix

This file is a test-run checklist for the V4.5/P8 release evidence layer. It is
kept under `tests/` because each row should correspond to either an executable
script case or a manual client run whose evidence is collected under
`test-records/runs/<run_id>/`.

## Required Script Checks

```bash
python3 -m py_compile scripts/collect_test_record.py scripts/run_chain_smoke.py scripts/run_openclaw_hermes_acceptance.py scripts/run_ttft_experiment.py
scripts/run_v45_p8_acceptance.sh plan
```

## Safe Smoke

```bash
NEWAPI_API_KEY=sk-dev scripts/run_v45_p8_acceptance.sh smoke
```

Expected evidence:

- `summary.md`
- `client-smoke.md`
- `derived/metrics.jsonl`
- `derived/request-map.jsonl`
- `derived/tool-repair-summary.json`

Required cases:

- `P0-models`
- `P0-openai-nonstream`
- `P0-openai-stream`
- `P0-openai-missing-tool-call-id`
- `P0-anthropic-messages-nonstream`
- `P0-anthropic-messages-stream`
- `P0-anthropic-missing-tool-use-id`
- `P0-anthropic-mixed-text-tool-result`

## Client Acceptance

```bash
NEWAPI_API_KEY=sk-dev scripts/run_v45_p8_acceptance.sh clients
```

Required cases:

- HTTP `/v1/models`
- HTTP short chat
- Hermes help
- Hermes short chat when config/runtime permits
- OpenClaw help
- OpenClaw model status
- OpenClaw capability list
- OpenClaw short chat when config/runtime permits

## TTFT Probe

```bash
NEWAPI_API_KEY=sk-dev scripts/run_v45_p8_acceptance.sh ttft --tokens 1000
```

Long-context TTFT tests require explicit token/request budgets and should not be
run as part of default smoke.

## Manual Windows Evidence

After a Windows client call through NewAPI, collect:

```bash
python3 scripts/collect_test_record.py \
  --scenario windows-client-smoke \
  --zen-base-url http://127.0.0.1:4000 \
  --zen-admin-base-url http://127.0.0.1:4001 \
  --zen-admin-base-url http://127.0.0.1:4002 \
  --zen-admin-base-url http://127.0.0.1:4004 \
  --newapi-base-url http://127.0.0.1:8081 \
  --admin-key test-key
```

## Pass Bar

- P0 API smoke pass rate is 100%.
- Tool protocol cases do not return upstream schema errors.
- `tool-repair-summary.json` has no `post_invalid` rows.
- NewAPI/ZenProxy join rate is at least 95% when NewAPI request ids are
  available.
- No raw prompts, completions, tool outputs, API keys, proxy credentials, or
  Authorization headers are stored in generated evidence.
