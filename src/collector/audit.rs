use crate::collector::{RequestFilter, RequestQueryResult, RequestTelemetry};
use serde_json::{json, Value};
use std::collections::BTreeMap;
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
        items.sort_by(|a, b| b.ts.cmp(&a.ts));
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
        items.sort_by(|a, b| a.ts.cmp(&b.ts));
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
        stats.to_json()
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
            .map(|(key, stats)| json!({"key": key, "stats": stats.to_json()}))
            .collect::<Vec<_>>();
        json!(values)
    }

    pub fn anomalies(&self, filter: &RequestFilter) -> Value {
        let mut items = self
            .filtered(filter)
            .into_iter()
            .filter(|item| is_anomalous(item))
            .collect::<Vec<_>>();
        items.sort_by(|a, b| b.ts.cmp(&a.ts));
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

    fn to_json(mut self) -> Value {
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

        let _ = std::fs::remove_dir_all(dir);
    }
}
