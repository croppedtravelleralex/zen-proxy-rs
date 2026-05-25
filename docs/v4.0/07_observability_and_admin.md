# V4.0 Observability and Admin

## Canonical Request Record

V4.0 needs one canonical request record. Metrics, admin query, and WAL should be
derived from that record or clearly marked as derived caches.

Required fields:

- request id
- timestamp
- public model
- upstream model
- protocol
- stream flag
- selected node id
- redacted node URL
- observed exit IP when available
- status
- outcome
- retry count
- retry-after when available
- total latency
- upstream latency
- TTFT for streaming
- token usage when available

## Exit-IP Proof

V4.0 must make the selected transport verifiable.

Preferred test-mode record:

```text
selected_node_id
selected_proxy_url_redacted
observed_exit_ip
upstream_status
```

If Zen cannot expose the observed IP, use a controlled auxiliary endpoint in a
test mode to prove the selected transport's egress IP. Production records may
only retain selected node id and redacted URL.

## Metrics

Minimum Prometheus dimensions:

- requests total
- success total
- 429 total
- 5xx total
- transport errors total
- dead pool size
- rate-limited pool size
- dispatch pool size
- active count
- request latency
- upstream latency
- TTFT

Avoid high-cardinality labels for raw client ids or full proxy URLs.

## Admin API

Required endpoints:

```text
GET  /admin/health
GET  /admin/pools
GET  /admin/nodes
POST /admin/nodes/{id}/probe
POST /admin/nodes/{id}/recover
GET  /admin/requests
GET  /admin/requests/{request_id}
GET  /admin/events
GET  /admin/config
GET  /admin/runtime
POST /admin/config/reload
POST /admin/fuse
```

Admin endpoints require authentication. Missing admin credentials should fail
closed for admin routes.

`/admin/runtime` is the fast live-data endpoint for V4.3. It includes:

- lane limits and in-flight counts;
- context governance limits;
- global budget mode and lease window;
- pool sizes and current leased count;
- data-plane internals: node-registry size, transport client-cache count, and
  direct-client initialization state.

## Durable Audit API

V4.1+ adds a durable audit layer in addition to the current-process ring buffer.
The ring buffer is still useful for live debugging, but it is not the full-day
truth source after restarts.

Default audit behavior:

```text
AUDIT_LOG_ENABLED=true
AUDIT_LOG_DIR=/tmp/zen-proxy-audit
file pattern: requests-YYYY-MM-DD.jsonl
record format: one RequestTelemetry JSON object per line
```

For production/VPS deployment, set `AUDIT_LOG_DIR` to a persistent location
such as `/var/log/zen-proxy-rs/audit` and configure log rotation or archival.

Durable audit endpoints:

```text
GET /admin/audit/summary?from=&to=&model=&status=&node=
GET /admin/audit/requests?from=&to=&model=&status=&node=&limit=
GET /admin/audit/requests/{rid}
GET /admin/audit/models?from=&to=
GET /admin/audit/nodes?from=&to=
GET /admin/audit/anomalies?from=&to=&limit=
GET /admin/audit/export?from=&to=&format=jsonl
```

`from` and `to` accept Unix seconds or Unix milliseconds. Query results come
from audit files, not from the in-memory ring buffer.

Audit records include V4.1 fields:

- `external_request_id`
- `gateway`
- `gateway_channel_id`
- `failure_kind`
- `failure_message`
- `retry_chain`
- context governance telemetry
- timing breakdown
- token and byte counters

Current anomaly classes:

- `empty_output`: completion tokens are 0
- `low_completion`: completion tokens are 1-3
- `large_context`: prompt tokens >= 100k
- `huge_context`: prompt tokens >= 200k
- `slow_ttft`: TTFT >= 10s
- `slow_total`: total latency >= 30s
- `compacted`: context governance trimmed the request
- `failure`: non-empty `failure_kind`

The current durable store is JSONL. SQLite/DuckDB aggregation remains the next
step if historical queries become too slow at higher volume.

## Event Types

Required event records:

- request_success
- request_rate_limited
- request_upstream_error
- request_transport_error
- pool_transition
- dead_probe_scheduled
- dead_probe_success
- dead_probe_failed
- config_reloaded
- fuse_opened
- fuse_closed
