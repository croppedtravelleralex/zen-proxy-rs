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
POST /admin/config/reload
POST /admin/fuse
```

Admin endpoints require authentication. Missing admin credentials should fail
closed for admin routes.

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

