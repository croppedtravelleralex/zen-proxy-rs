# ZenProxyRS V4.0 Documentation

This documentation set is the only active design baseline for `zen-proxy-rs`.
Older design documents were removed to avoid stale architecture guidance.

## Active Entry

Start here:

- [V4.0 AI Entry](./v4.0/00_AI_ENTRY.md)

## Document Map

| File | Purpose |
|---|---|
| [00_AI_ENTRY.md](./v4.0/00_AI_ENTRY.md) | AI handoff, current objective, reading order |
| [01_positioning_and_scope.md](./v4.0/01_positioning_and_scope.md) | V4.0 product boundary and non-goals |
| [02_architecture.md](./v4.0/02_architecture.md) | Layered "mortise and tenon" architecture |
| [03_contracts_and_interfaces.md](./v4.0/03_contracts_and_interfaces.md) | Required traits and data contracts |
| [04_request_flow.md](./v4.0/04_request_flow.md) | Target request path and failure handling |
| [05_models_and_routing.md](./v4.0/05_models_and_routing.md) | Public models, upstream mapping, routing policy |
| [06_pool_and_probe_policy.md](./v4.0/06_pool_and_probe_policy.md) | Pool state machine, dead probing, 429 handling |
| [07_observability_and_admin.md](./v4.0/07_observability_and_admin.md) | Ledger, metrics, admin APIs, exit-IP proof |
| [08_implementation_plan.md](./v4.0/08_implementation_plan.md) | Ordered implementation tasks |
| [09_acceptance_and_risks.md](./v4.0/09_acceptance_and_risks.md) | Acceptance gates, known risks, rollback |
| [10_2026-05-25_operations_report.md](./v4.0/10_2026-05-25_operations_report.md) | V4.1-A maintenance notes and 2026-05-25 NewAPI call analysis |
| [11_v4.3_scalable_data_plane.md](./v4.0/11_v4.3_scalable_data_plane.md) | V4.3 scalable data-plane target and lane isolation |
| [12_v4.4_pool_fault_isolation.md](./v4.0/12_v4.4_pool_fault_isolation.md) | V4.4 pool fault isolation, node anti-mis-injury, and 2026-05-26 deployment evidence |
| [13_v4.5_cache_affinity_ttft.md](./v4.0/13_v4.5_cache_affinity_ttft.md) | V4.5 cache-affinity routing and effective TTFT telemetry |
| [14_v4.6_protocol_guard.md](./v4.0/14_v4.6_protocol_guard.md) | V4.6 protocol graph guard and pair-aware compactor |
| [15_test_records_and_client_acceptance.md](./v4.0/15_test_records_and_client_acceptance.md) | V4.7 test evidence packages, OpenClaw/Hermes acceptance, and 100K-200K TTFT experiment plan |
| [16_v4.5_p8_95_plus_acceptance.md](./v4.0/16_v4.5_p8_95_plus_acceptance.md) | V4.5/P8 95+ acceptance plan, test matrix, Windows/WSL/panda flows, and NewAPI/ZenProxy reconciliation |
| [17_v4.6_99plus_runtime_policy.md](./v4.0/17_v4.6_99plus_runtime_policy.md) | V4.6 99+ source-side quality, non-stream guard, lane isolation, and timing metrics |
| [18_v4.8_four_client_500_round_acceptance.md](./v4.0/18_v4.8_four_client_500_round_acceptance.md) | V4.8 99+ four-client 500-round acceptance plan for Windows ClaudeCode, WSL ClaudeCode, OpenClaw, and Hermes |
| [19_2026-05-30_handoff_and_unfinished_work.md](./v4.0/19_2026-05-30_handoff_and_unfinished_work.md) | 2026-05-30 handoff: verified facts, unfinished Hermes/OpenClaw panda-NewAPI work, unimplemented V4.8 harness, and next-session guardrails |
| [20_2026-06-01_final_anchor_deploy.md](./v4.0/20_2026-06-01_final_anchor_deploy.md) | 2026-06-01 panda deployment: ClaudeCode huge-context final-anchor, buffered retry, and panda-local huge stream smoke evidence |

## AI Maintenance Rule

Future AI work must start from [V4.0 AI Entry](./v4.0/00_AI_ENTRY.md), then check
the latest operations report before changing code. Runtime facts from ZenProxy
admin APIs, NewAPI logs, Redis budgets, WAL files, tests, and release builds are
higher priority than older chat history.
