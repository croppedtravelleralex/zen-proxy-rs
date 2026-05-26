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

## AI Maintenance Rule

Future AI work must start from [V4.0 AI Entry](./v4.0/00_AI_ENTRY.md), then check
the latest operations report before changing code. Runtime facts from ZenProxy
admin APIs, NewAPI logs, Redis budgets, WAL files, tests, and release builds are
higher priority than older chat history.
