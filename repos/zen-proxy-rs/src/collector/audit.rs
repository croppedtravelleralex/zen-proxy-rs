use crate::collector::{RequestFilter, RequestQueryResult, RequestTelemetry};
use serde_json::{json, Value};
use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct AuditStore {
    dir: PathBuf,
    writer: Mutex<Option<AuditWriter>>,
}

struct AuditWriter {
    path: PathBuf,
    writer: BufWriter<File>,
}

#[derive(Default)]
struct AuditStats {
    requests: u64,
    success: u64,
    count_4xx: u64,
    count_5xx: u64,
    count_429: u64,
    stream: u64,
    non_stream: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    bytes_sent: u64,
    bytes_received: u64,
    empty_output: u64,
    low_completion: u64,
    large_context: u64,
    huge_context: u64,
    compacted: u64,
    slow_ttft: u64,
    slow_total: u64,
    latencies: Vec<u64>,
    ttfts: Vec<u64>,
}

impl AuditStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let _ = fs::create_dir_all(&dir);
        Self {
            dir,
            writer: Mutex::new(None),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn append(&self, tele: &RequestTelemetry) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let path = self.request_path(tele.ts);
        let mut guard = self.writer.lock().unwrap();
        let needs_open = guard.as_ref().is_none_or(|current| current.path != path);
        if needs_open {
            let file = OpenOptions::new().create(true).append(true).open(&path)?;
            *guard = Some(AuditWriter {
                path: path.clone(),
                writer: BufWriter::new(file),
            });
        }
        if let Some(current) = guard.as_mut() {
            let line = serde_json::to_string(tele).unwrap_or_default();
            writeln!(current.writer, "{line}")?;
            current.writer.flush()?;
        }
        Ok(())
    }

    pub fn flush(&self) {
        if let Some(current) = self.writer.lock().unwrap().as_mut() {
            let _ = current.writer.flush();
        }
    }

    pub fn query_requests(&self, filter: &RequestFilter) -> RequestQueryResult {
        let mut items = self.filtered(filter);
        items.sort_by_key(|item| Reverse(item.ts));
        let limit = filter.limit.max(1);
        if items.len() > limit {
            items.truncate(limit);
        }
        RequestQueryResult {
            items,
            next_cursor: None,
        }
    }

    pub fn export(&self, filter: &RequestFilter) -> String {
        let mut items = self.filtered(filter);
        items.sort_by_key(|item| item.ts);
        let mut body = String::new();
        for item in items.into_iter().take(filter.limit.max(1)) {
            if let Ok(line) = serde_json::to_string(&item) {
                body.push_str(&line);
                body.push('\n');
            }
        }
        body
    }

    pub fn summary(&self, filter: &RequestFilter) -> Value {
        let mut stats = AuditStats::default();
        for item in self.filtered(filter) {
            stats.record(&item);
        }
        stats.into_json()
    }

    pub fn grouped(&self, filter: &RequestFilter, group: AuditGroup) -> Value {
        let mut groups: BTreeMap<String, AuditStats> = BTreeMap::new();
        for item in self.filtered(filter) {
            let key = match group {
                AuditGroup::Model => item.model.clone(),
                AuditGroup::Node => {
                    if item.selected_node_id.is_empty() {
                        "unknown".to_string()
                    } else {
                        item.selected_node_id.clone()
                    }
                }
            };
            groups.entry(key).or_default().record(&item);
        }
        let values = groups
            .into_iter()
            .map(|(key, stats)| json!({"key": key, "stats": stats.into_json()}))
            .collect::<Vec<_>>();
        json!(values)
    }

    pub fn anomalies(&self, filter: &RequestFilter) -> Value {
        let mut items = self
            .filtered(filter)
            .into_iter()
            .filter(is_anomalous)
            .collect::<Vec<_>>();
        items.sort_by_key(|item| Reverse(item.ts));
        let limit = filter.limit.max(1);
        if items.len() > limit {
            items.truncate(limit);
        }
        json!(items
            .into_iter()
            .map(|item| {
                json!({
                    "rid": item.rid,
                    "external_request_id": item.external_request_id,
                    "ts": item.ts,
                    "model": item.model,
                    "status": item.status,
                    "node_id": item.selected_node_id,
                    "completion_tokens": item.completion_tokens,
                    "prompt_tokens": item.prompt_tokens,
                    "latency_total_ms": item.latency_total_ms,
                    "ttft_ms": item.ttft_ms,
                    "failure_kind": item.failure_kind,
                    "flags": anomaly_flags(&item),
                })
            })
            .collect::<Vec<_>>())
    }

    pub fn timeseries(&self, filter: &RequestFilter, bucket_ms: i64) -> Value {
        let bucket_ms = bucket_ms.max(1);
        let mut buckets: BTreeMap<i64, AuditStats> = BTreeMap::new();
        for item in self.filtered(filter) {
            let bucket = item.ts.div_euclid(bucket_ms) * bucket_ms;
            buckets.entry(bucket).or_default().record(&item);
        }
        json!(buckets
            .into_iter()
            .map(|(bucket_ts, stats)| json!({"bucket_ts": bucket_ts, "stats": stats.into_json()}))
            .collect::<Vec<_>>())
    }

    pub fn top_requests(&self, filter: &RequestFilter, by: &str) -> Value {
        let mut items = self.filtered(filter);
        items.sort_by_key(|item| Reverse(top_request_value(item, by)));
        let limit = filter.limit.max(1);
        json!(items
            .into_iter()
            .take(limit)
            .map(|item| {
                json!({
                    "rid": item.rid,
                    "external_request_id": item.external_request_id,
                    "ts": item.ts,
                    "model": item.model,
                    "node_id": item.selected_node_id,
                    "status": item.status,
                    "latency_total_ms": item.latency_total_ms,
                    "ttft_ms": item.ttft_ms,
                    "prompt_tokens": item.prompt_tokens,
                    "completion_tokens": item.completion_tokens,
                    "total_tokens": item.total_tokens,
                    "bytes_sent": item.bytes_sent,
                    "bytes_received": item.bytes_received,
                    "failure_kind": item.failure_kind,
                    "sort_value": top_request_value(&item, by),
                })
            })
            .collect::<Vec<_>>())
    }

    pub fn top_nodes(&self, filter: &RequestFilter, by: &str) -> Value {
        let mut groups: HashMap<String, AuditStats> = HashMap::new();
        for item in self.filtered(filter) {
            let key = if item.selected_node_id.is_empty() {
                "unknown".to_string()
            } else {
                item.selected_node_id.clone()
            };
            groups.entry(key).or_default().record(&item);
        }
        let mut rows = groups
            .into_iter()
            .map(|(node_id, stats)| {
                let value = top_stats_value(&stats, by);
                json!({"node_id": node_id, "sort_value": value, "stats": stats.into_json()})
            })
            .collect::<Vec<_>>();
        rows.sort_by(|a, b| {
            b.get("sort_value")
                .and_then(Value::as_f64)
                .partial_cmp(&a.get("sort_value").and_then(Value::as_f64))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        rows.truncate(filter.limit.max(1));
        json!(rows)
    }

    pub fn failures(&self, filter: &RequestFilter) -> Value {
        let mut groups: BTreeMap<String, AuditStats> = BTreeMap::new();
        for item in self.filtered(filter).into_iter().filter(is_failure) {
            let key = failure_key(&item);
            groups.entry(key).or_default().record(&item);
        }
        json!(groups
            .into_iter()
            .map(|(key, stats)| json!({"key": key, "stats": stats.into_json()}))
            .collect::<Vec<_>>())
    }

    pub fn node_detail(&self, filter: &RequestFilter, node_id: &str) -> Value {
        let mut node_filter = clone_filter(filter);
        node_filter.node_url = Some(node_id.to_string());
        let mut items = self.filtered(&node_filter);
        items.sort_by_key(|item| Reverse(item.ts));
        let mut stats = AuditStats::default();
        for item in &items {
            stats.record(item);
        }
        let recent = items
            .into_iter()
            .take(filter.limit.max(1))
            .map(|item| {
                json!({
                    "rid": item.rid,
                    "external_request_id": item.external_request_id,
                    "ts": item.ts,
                    "status": item.status,
                    "model": item.model,
                    "latency_total_ms": item.latency_total_ms,
                    "ttft_ms": item.ttft_ms,
                    "prompt_tokens": item.prompt_tokens,
                    "completion_tokens": item.completion_tokens,
                    "bytes_sent": item.bytes_sent,
                    "bytes_received": item.bytes_received,
                    "failure_kind": item.failure_kind,
                    "flags": anomaly_flags(&item),
                })
            })
            .collect::<Vec<_>>();
        json!({
            "node_id": node_id,
            "stats": stats.into_json(),
            "recent": recent,
        })
    }

    pub fn by_external_id(&self, external_id: &str, limit: usize) -> Value {
        let mut items = self
            .filtered(&RequestFilter {
                limit,
                ..Default::default()
            })
            .into_iter()
            .filter(|item| item.external_request_id == external_id)
            .collect::<Vec<_>>();
        items.sort_by_key(|item| Reverse(item.ts));
        items.truncate(limit.max(1));
        json!(items)
    }

    pub fn reconcile(&self, filter: &RequestFilter) -> Value {
        let items = self.filtered(filter);
        let total = items.len() as u64;
        let with_external = items
            .iter()
            .filter(|item| !item.external_request_id.is_empty())
            .count() as u64;
        let with_gateway = items.iter().filter(|item| !item.gateway.is_empty()).count() as u64;
        let duplicate_external_ids = duplicate_external_ids(&items);
        json!({
            "requests": total,
            "with_external_request_id": with_external,
            "missing_external_request_id": total.saturating_sub(with_external),
            "with_gateway": with_gateway,
            "missing_gateway": total.saturating_sub(with_gateway),
            "duplicate_external_ids": duplicate_external_ids,
            "reconcile_ready": with_external > 0 && duplicate_external_ids.is_empty(),
        })
    }

    pub fn budget_history(&self, filter: &RequestFilter, bucket_ms: i64) -> Value {
        let bucket_ms = bucket_ms.max(1);
        let mut buckets: BTreeMap<i64, HashMap<String, AuditStats>> = BTreeMap::new();
        for item in self.filtered(filter) {
            let bucket = item.ts.div_euclid(bucket_ms) * bucket_ms;
            let node_id = if item.selected_node_id.is_empty() {
                "unknown".to_string()
            } else {
                item.selected_node_id.clone()
            };
            buckets
                .entry(bucket)
                .or_default()
                .entry(node_id)
                .or_default()
                .record(&item);
        }
        json!(buckets
            .into_iter()
            .map(|(bucket_ts, nodes)| {
                let mut node_rows = nodes
                    .into_iter()
                    .map(|(node_id, stats)| json!({"node_id": node_id, "stats": stats.into_json()}))
                    .collect::<Vec<_>>();
                node_rows.sort_by(|a, b| {
                    b["stats"]["requests"]
                        .as_u64()
                        .cmp(&a["stats"]["requests"].as_u64())
                });
                json!({"bucket_ts": bucket_ts, "nodes": node_rows})
            })
            .collect::<Vec<_>>())
    }

    fn filtered(&self, filter: &RequestFilter) -> Vec<RequestTelemetry> {
        self.flush();
        let mut out = Vec::new();
        for path in self.request_files() {
            let Ok(file) = File::open(path) else {
                continue;
            };
            let reader = BufReader::new(file);
            for line in reader.lines().map_while(Result::ok) {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(item) = serde_json::from_str::<RequestTelemetry>(&line) else {
                    continue;
                };
                if !matches_filter(&item, filter) {
                    continue;
                }
                out.push(item);
            }
        }
        out
    }

    fn request_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return files;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if name.starts_with("requests-") && name.ends_with(".jsonl") {
                files.push(path);
            }
        }
        files.sort();
        files
    }

    fn request_path(&self, ts: i64) -> PathBuf {
        let date = chrono::DateTime::from_timestamp_millis(ts)
            .map(|dt| {
                dt.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d")
                    .to_string()
            })
            .unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%d").to_string());
        self.dir.join(format!("requests-{date}.jsonl"))
    }
}

pub enum AuditGroup {
    Model,
    Node,
}

impl AuditStats {
    fn record(&mut self, item: &RequestTelemetry) {
        self.requests += 1;
        if (200..=299).contains(&item.status) {
            self.success += 1;
        }
        if item.rate_limited || item.status == 429 {
            self.count_429 += 1;
        } else if item.status >= 500 {
            self.count_5xx += 1;
        } else if item.status >= 400 {
            self.count_4xx += 1;
        }
        if item.is_streaming {
            self.stream += 1;
        } else {
            self.non_stream += 1;
        }
        self.prompt_tokens += item.prompt_tokens as u64;
        self.completion_tokens += item.completion_tokens as u64;
        self.total_tokens += item.total_tokens as u64;
        self.bytes_sent += item.bytes_sent;
        self.bytes_received += item.bytes_received;
        if item.completion_tokens == 0 {
            self.empty_output += 1;
        }
        if item.completion_tokens <= 3 {
            self.low_completion += 1;
        }
        if item.prompt_tokens >= 100_000 {
            self.large_context += 1;
        }
        if item.prompt_tokens >= 200_000 {
            self.huge_context += 1;
        }
        if item.context.as_ref().is_some_and(|context| context.trimmed) {
            self.compacted += 1;
        }
        if item.ttft_ms >= 10_000 {
            self.slow_ttft += 1;
        }
        if item.latency_total_ms >= 30_000 {
            self.slow_total += 1;
        }
        self.latencies.push(item.latency_total_ms);
        if item.ttft_ms > 0 {
            self.ttfts.push(item.ttft_ms);
        }
    }

    fn into_json(mut self) -> Value {
        self.latencies.sort_unstable();
        self.ttfts.sort_unstable();
        json!({
            "requests": self.requests,
            "success": self.success,
            "count_429": self.count_429,
            "count_4xx": self.count_4xx,
            "count_5xx": self.count_5xx,
            "stream": self.stream,
            "non_stream": self.non_stream,
            "prompt_tokens": self.prompt_tokens,
            "completion_tokens": self.completion_tokens,
            "total_tokens": self.total_tokens,
            "bytes_sent": self.bytes_sent,
            "bytes_received": self.bytes_received,
            "avg_latency_ms": avg(&self.latencies),
            "p50_latency_ms": percentile(&self.latencies, 0.50),
            "p90_latency_ms": percentile(&self.latencies, 0.90),
            "p95_latency_ms": percentile(&self.latencies, 0.95),
            "p99_latency_ms": percentile(&self.latencies, 0.99),
            "avg_ttft_ms": avg(&self.ttfts),
            "p90_ttft_ms": percentile(&self.ttfts, 0.90),
            "anomalies": {
                "empty_output": self.empty_output,
                "low_completion": self.low_completion,
                "large_context": self.large_context,
                "huge_context": self.huge_context,
                "compacted": self.compacted,
                "slow_ttft": self.slow_ttft,
                "slow_total": self.slow_total,
            }
        })
    }
}

fn matches_filter(item: &RequestTelemetry, filter: &RequestFilter) -> bool {
    if filter.rid.as_ref().is_some_and(|rid| item.rid != *rid) {
        return false;
    }
    if filter
        .model
        .as_ref()
        .is_some_and(|model| item.model != *model)
    {
        return false;
    }
    if filter
        .node_url
        .as_ref()
        .is_some_and(|node| item.selected_node_id != *node && item.node_url != *node)
    {
        return false;
    }
    if filter.status.is_some_and(|status| item.status != status) {
        return false;
    }
    if filter.since.is_some_and(|since| item.ts < since) {
        return false;
    }
    if filter.until.is_some_and(|until| item.ts > until) {
        return false;
    }
    true
}

fn is_anomalous(item: &RequestTelemetry) -> bool {
    item.completion_tokens <= 3
        || item.prompt_tokens >= 100_000
        || item.latency_total_ms >= 30_000
        || item.ttft_ms >= 10_000
        || !item.failure_kind.is_empty()
        || item.context.as_ref().is_some_and(|context| context.trimmed)
}

fn is_failure(item: &RequestTelemetry) -> bool {
    item.status >= 400 || item.rate_limited || !item.failure_kind.is_empty()
}

fn failure_key(item: &RequestTelemetry) -> String {
    if !item.failure_kind.is_empty() {
        return item.failure_kind.clone();
    }
    if item.status == 429 || item.rate_limited {
        return "rate_limited".to_string();
    }
    if item.status >= 500 {
        return format!("status_{}", item.status);
    }
    if item.status >= 400 {
        return format!("client_status_{}", item.status);
    }
    "unknown_failure".to_string()
}

fn top_request_value(item: &RequestTelemetry, by: &str) -> u64 {
    match by {
        "tokens" | "total_tokens" => item.total_tokens as u64,
        "prompt_tokens" => item.prompt_tokens as u64,
        "completion_tokens" => item.completion_tokens as u64,
        "bytes" | "traffic" => item.bytes_sent.saturating_add(item.bytes_received),
        "bytes_sent" => item.bytes_sent,
        "bytes_received" => item.bytes_received,
        "ttft" | "ttft_ms" => item.ttft_ms,
        "latency" | "latency_ms" | "total_ms" => item.latency_total_ms,
        "errors" | "failures" => u64::from(is_failure(item)),
        _ => item.latency_total_ms,
    }
}

fn top_stats_value(stats: &AuditStats, by: &str) -> f64 {
    match by {
        "tokens" | "total_tokens" => stats.total_tokens as f64,
        "prompt_tokens" => stats.prompt_tokens as f64,
        "completion_tokens" => stats.completion_tokens as f64,
        "bytes" | "traffic" => stats.bytes_sent.saturating_add(stats.bytes_received) as f64,
        "bytes_sent" => stats.bytes_sent as f64,
        "bytes_received" => stats.bytes_received as f64,
        "errors" | "failures" => (stats.count_4xx + stats.count_5xx + stats.count_429) as f64,
        "latency" | "latency_ms" | "avg_latency" => avg(&stats.latencies),
        "requests" | "calls" => stats.requests as f64,
        _ => stats.requests as f64,
    }
}

fn duplicate_external_ids(items: &[RequestTelemetry]) -> Vec<Value> {
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for item in items {
        if item.external_request_id.is_empty() {
            continue;
        }
        *counts.entry(item.external_request_id.clone()).or_default() += 1;
    }
    counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(external_request_id, count)| json!({"external_request_id": external_request_id, "count": count}))
        .collect()
}

fn clone_filter(filter: &RequestFilter) -> RequestFilter {
    RequestFilter {
        rid: filter.rid.clone(),
        model: filter.model.clone(),
        node_url: filter.node_url.clone(),
        status: filter.status,
        since: filter.since,
        until: filter.until,
        limit: filter.limit,
        cursor: filter.cursor,
    }
}

fn anomaly_flags(item: &RequestTelemetry) -> Vec<&'static str> {
    let mut flags = Vec::new();
    if item.completion_tokens == 0 {
        flags.push("empty_output");
    } else if item.completion_tokens <= 3 {
        flags.push("low_completion");
    }
    if item.prompt_tokens >= 200_000 {
        flags.push("huge_context");
    } else if item.prompt_tokens >= 100_000 {
        flags.push("large_context");
    }
    if item.ttft_ms >= 10_000 {
        flags.push("slow_ttft");
    }
    if item.latency_total_ms >= 30_000 {
        flags.push("slow_total");
    }
    if item.context.as_ref().is_some_and(|context| context.trimmed) {
        flags.push("compacted");
    }
    if !item.failure_kind.is_empty() {
        flags.push("failure");
    }
    flags
}

fn avg(items: &[u64]) -> f64 {
    if items.is_empty() {
        return 0.0;
    }
    items.iter().sum::<u64>() as f64 / items.len() as f64
}

fn percentile(items: &[u64], percentile: f64) -> u64 {
    if items.is_empty() {
        return 0;
    }
    let idx = ((items.len() - 1) as f64 * percentile).floor() as usize;
    items[idx]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector::telemetry::new_telemetry;

    #[test]
    fn audit_store_persists_and_queries_requests() {
        let dir = std::env::temp_dir().join(format!("zen-audit-test-{}", uuid::Uuid::new_v4()));
        let store = AuditStore::new(&dir);
        let mut tele = new_telemetry();
        tele.rid = "rid-1".to_string();
        tele.model = "deepseek-v4-flash".to_string();
        tele.status = 200;
        tele.completion_tokens = 2;
        tele.total_tokens = 102;
        tele.prompt_tokens = 100;
        tele.latency_total_ms = 31_000;
        store.append(&tele).unwrap();
        store.flush();

        let result = store.query_requests(&RequestFilter {
            rid: Some("rid-1".to_string()),
            ..Default::default()
        });
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].rid, "rid-1");

        let summary = store.summary(&RequestFilter::default());
        assert_eq!(summary["requests"], 1);
        assert_eq!(summary["anomalies"]["low_completion"], 1);
        assert_eq!(summary["anomalies"]["slow_total"], 1);
        assert_eq!(
            store.top_requests(&RequestFilter::default(), "latency")[0]["rid"],
            "rid-1"
        );
        assert_eq!(
            store
                .failures(&RequestFilter::default())
                .as_array()
                .unwrap()
                .len(),
            0
        );

        let _ = std::fs::remove_dir_all(dir);
    }
}
