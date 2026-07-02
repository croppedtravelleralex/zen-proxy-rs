use std::collections::HashMap;
use std::sync::RwLock;

use crate::collector::{RequestAttemptTelemetry, RequestTelemetry};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEvent {
    pub ts: i64,
    pub rid: String,
    pub event_type: String,
    pub node_id: String,
    pub node_url_redacted: String,
    pub model: String,
    pub stream: bool,
    pub status: u16,
    pub retry_after: Option<i64>,
    pub error_type: Option<String>,
    pub error_body_summary: Option<String>,
    pub latency_ms: u64,
    pub exit_ip: Option<String>,
    pub upstream_api_key_hash: String,
    pub user_agent_hash: Option<String>,
    pub client_hash: Option<String>,
    pub project_hash: Option<String>,
    pub session_hash: Option<String>,
    pub request_hash: Option<String>,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
    pub pool_from: Option<String>,
    pub pool_to: Option<String>,
    pub attempt: u32,
}

impl LedgerEvent {
    pub fn redact_node_url(url: &str) -> String {
        if url == "direct" {
            return "direct".to_string();
        }
        if let Some(at_pos) = url.find('@') {
            let protocol_end = url.find("://").map(|p| p + 3).unwrap_or(0);
            format!("{}***@{}", &url[..protocol_end], &url[at_pos + 1..])
        } else {
            url.to_string()
        }
    }

    pub fn short_hash(input: &str) -> String {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(input.as_bytes());
        hex::encode(&hash[..8])
    }
}

pub fn sanitize_request_telemetry(input: &RequestTelemetry) -> RequestTelemetry {
    let mut out = input.clone();
    out.client_id = hash_or_empty(&out.client_id);
    out.node_url = LedgerEvent::redact_node_url(&out.node_url);
    out.selected_node_url_redacted =
        LedgerEvent::redact_node_url(non_empty_or(&out.selected_node_url_redacted, &out.node_url));
    out.failure_message = sanitize_text(&out.failure_message);
    out.path = sanitize_text(&out.path);
    out.gateway = sanitize_text(&out.gateway);
    out.retry_chain = out.retry_chain.iter().map(sanitize_attempt).collect();
    if let Some(context) = out.context.as_mut() {
        context.trace = context
            .trace
            .iter()
            .map(|item| sanitize_text(item))
            .collect();
    }
    out
}

fn sanitize_attempt(input: &RequestAttemptTelemetry) -> RequestAttemptTelemetry {
    let mut out = input.clone();
    out.node_url_redacted = LedgerEvent::redact_node_url(&out.node_url_redacted);
    out.error_type = sanitize_text(&out.error_type);
    out.outcome = sanitize_text(&out.outcome);
    out
}

pub fn sanitize_json_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(s) => serde_json::Value::String(sanitize_text(&s)),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(sanitize_json_value).collect())
        }
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, sanitize_json_value(value)))
                .collect(),
        ),
        other => other,
    }
}

pub fn sanitize_text(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }
    let mut out = redact_bearer_like(input);
    out = redact_proxy_credentials(&out);
    out = redact_paths(&out);
    out
}

fn hash_or_empty(input: &str) -> String {
    if input.is_empty() {
        String::new()
    } else {
        format!("hash:{}", LedgerEvent::short_hash(input))
    }
}

fn non_empty_or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() {
        fallback
    } else {
        value
    }
}

fn redact_bearer_like(input: &str) -> String {
    input
        .split_whitespace()
        .map(|token| {
            let lower = token.to_ascii_lowercase();
            if token.starts_with("sk-")
                || token.starts_with("sk_")
                || lower.starts_with("bearer:")
                || lower.starts_with("apikey:")
                || lower.starts_with("api_key:")
                || lower.starts_with("authorization:")
            {
                redact_token_preserving_punct(token)
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn redact_token_preserving_punct(token: &str) -> String {
    let end_punct = token
        .chars()
        .rev()
        .take_while(|c| matches!(c, ',' | ';' | ')' | ']' | '}' | '"' | '\''))
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("[redacted-secret]{end_punct}")
}

fn redact_proxy_credentials(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(scheme_pos) = rest.find("://") {
        let (prefix, after_prefix) = rest.split_at(scheme_pos + 3);
        out.push_str(prefix);
        let after_scheme = after_prefix;
        let url_end = after_scheme
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ')' || c == ']')
            .unwrap_or(after_scheme.len());
        let (url_part, tail) = after_scheme.split_at(url_end);
        if let Some(at_pos) = url_part.find('@') {
            out.push_str("***@");
            out.push_str(&url_part[at_pos + 1..]);
        } else {
            out.push_str(url_part);
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

fn redact_paths(input: &str) -> String {
    input
        .split_whitespace()
        .map(|token| {
            let normalized = token.replace('\\', "/");
            let lower = normalized.to_ascii_lowercase();
            if lower.starts_with("/home/")
                || lower.starts_with("/root/")
                || lower.starts_with("/mnt/")
                || lower.starts_with("//wsl.localhost/")
                || (lower.len() > 3 && lower.as_bytes()[1] == b':' && lower.as_bytes()[2] == b'/')
            {
                redact_token_preserving_punct(token)
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Default, Clone)]
pub(crate) struct PerDimensionCounters {
    pub(crate) requests: u64,
    pub(crate) success: u64,
    pub(crate) count_429: u64,
    pub(crate) count_5xx: u64,
    pub(crate) count_network_error: u64,
}

#[derive(Default)]
pub struct LedgerCounters {
    by_node: RwLock<HashMap<String, PerDimensionCounters>>,
    by_model: RwLock<HashMap<String, PerDimensionCounters>>,
    by_key: RwLock<HashMap<String, PerDimensionCounters>>,
    by_stream: RwLock<HashMap<bool, PerDimensionCounters>>,
    by_error_type: RwLock<HashMap<String, PerDimensionCounters>>,
    by_status: RwLock<HashMap<String, PerDimensionCounters>>,
    total_requests: std::sync::atomic::AtomicU64,
    total_success: std::sync::atomic::AtomicU64,
    total_429: std::sync::atomic::AtomicU64,
    total_5xx: std::sync::atomic::AtomicU64,
    total_network_error: std::sync::atomic::AtomicU64,
    events_path: RwLock<Option<String>>,
}

impl LedgerCounters {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_events_path(&self, path: Option<String>) {
        *self.events_path.write().unwrap() = path;
    }

    pub fn record(&self, event: &LedgerEvent) {
        use std::sync::atomic::Ordering;

        self.total_requests.fetch_add(1, Ordering::Relaxed);
        if event.status >= 200 && event.status < 300 {
            self.total_success.fetch_add(1, Ordering::Relaxed);
        }
        if event.status == 429 {
            self.total_429.fetch_add(1, Ordering::Relaxed);
        } else if event.status >= 500 {
            self.total_5xx.fetch_add(1, Ordering::Relaxed);
        }
        if event.error_type.as_deref() == Some("network") {
            self.total_network_error.fetch_add(1, Ordering::Relaxed);
        }

        self.inc_dimension(&self.by_node, &event.node_id, event.status);
        self.inc_dimension(&self.by_model, &event.model, event.status);
        self.inc_dimension(&self.by_key, &event.upstream_api_key_hash, event.status);
        self.inc_dimension_bool(&self.by_stream, event.stream, event.status);
        self.inc_dimension(&self.by_status, &event.status.to_string(), event.status);
        self.inc_dimension(
            &self.by_error_type,
            event.error_type.as_deref().unwrap_or("none"),
            event.status,
        );

        let is_429 = event.status == 429 || event.error_type.as_deref() == Some("rate_limited");
        let is_5xx = event.status >= 500 && event.status != 429;
        let is_network = event.error_type.as_deref() == Some("network")
            || event.error_type.as_deref() == Some("timeout");

        if is_429 || is_5xx || is_network || event.pool_from.is_some() || event.pool_to.is_some() {
            if let Some(path) = self.events_path.read().unwrap().as_ref() {
                if let Ok(json) = serde_json::to_string(event) {
                    use std::io::Write;
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)
                    {
                        let _ = writeln!(f, "{}", json);
                    }
                }
            }
        }
    }

    fn inc_dimension(
        &self,
        map: &RwLock<HashMap<String, PerDimensionCounters>>,
        key: &str,
        status: u16,
    ) {
        let mut m = map.write().unwrap();
        let entry = m.entry(key.to_string()).or_default();
        entry.requests += 1;
        if (200..300).contains(&status) {
            entry.success += 1;
        }
        if status == 429 {
            entry.count_429 += 1;
        } else if status >= 500 {
            entry.count_5xx += 1;
        }
    }

    fn inc_dimension_bool(
        &self,
        map: &RwLock<HashMap<bool, PerDimensionCounters>>,
        key: bool,
        status: u16,
    ) {
        let mut m = map.write().unwrap();
        let entry = m.entry(key).or_default();
        entry.requests += 1;
        if (200..300).contains(&status) {
            entry.success += 1;
        }
        if status == 429 {
            entry.count_429 += 1;
        } else if status >= 500 {
            entry.count_5xx += 1;
        }
    }

    pub fn summary(&self) -> serde_json::Value {
        use serde_json::json;
        use std::sync::atomic::Ordering;

        let by_node: serde_json::Value = {
            let m = self.by_node.read().unwrap();
            let mut map = serde_json::Map::new();
            for (k, v) in m.iter() {
                map.insert(
                    k.clone(),
                    json!({
                        "requests": v.requests,
                        "success": v.success,
                        "429": v.count_429,
                        "5xx": v.count_5xx,
                    }),
                );
            }
            serde_json::Value::Object(map)
        };
        let by_error_type = dimension_json(&self.by_error_type);
        let by_status = dimension_json(&self.by_status);

        json!({
            "total_requests": self.total_requests.load(Ordering::Relaxed),
            "success": self.total_success.load(Ordering::Relaxed),
            "429": self.total_429.load(Ordering::Relaxed),
            "5xx": self.total_5xx.load(Ordering::Relaxed),
            "network_errors": self.total_network_error.load(Ordering::Relaxed),
            "by_node": by_node,
            "by_error_type": by_error_type,
            "by_status": by_status,
        })
    }

    pub fn by_model_summary(&self) -> HashMap<String, PerDimensionCounters> {
        self.by_model.read().unwrap().clone()
    }

    pub fn by_key_summary(&self) -> HashMap<String, PerDimensionCounters> {
        self.by_key.read().unwrap().clone()
    }

    pub fn by_stream_summary(&self) -> HashMap<bool, PerDimensionCounters> {
        self.by_stream.read().unwrap().clone()
    }
}

fn dimension_json(map: &RwLock<HashMap<String, PerDimensionCounters>>) -> serde_json::Value {
    use serde_json::json;
    let m = map.read().unwrap();
    let mut out = serde_json::Map::new();
    for (k, v) in m.iter() {
        out.insert(
            k.clone(),
            json!({
                "requests": v.requests,
                "success": v.success,
                "429": v.count_429,
                "5xx": v.count_5xx,
                "network": v.count_network_error,
            }),
        );
    }
    serde_json::Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_node_url_masks_password() {
        let url = "socks5h://user:pass123@p.webshare.io:80";
        let redacted = LedgerEvent::redact_node_url(url);
        assert!(!redacted.contains("pass123"));
        assert!(redacted.contains("p.webshare.io"));
        assert!(redacted.contains("***"));
    }

    #[test]
    fn redact_direct_is_unmodified() {
        assert_eq!(LedgerEvent::redact_node_url("direct"), "direct");
    }

    #[test]
    fn short_hash_is_16_hex_chars() {
        let hash = LedgerEvent::short_hash("some-value");
        assert_eq!(hash.len(), 16);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn ledger_event_serializes_to_jsonl() {
        let ev = LedgerEvent {
            ts: chrono::Utc::now().timestamp_millis(),
            rid: "test-rid".into(),
            event_type: "rate_limited".into(),
            node_id: "node-1".into(),
            error_body_summary: None,
            exit_ip: None,
            node_url_redacted: LedgerEvent::redact_node_url("socks5h://u:p@host:1080"),
            model: "big-pickle".into(),
            stream: true,
            status: 429,
            retry_after: Some(81791),
            error_type: Some("FreeUsageLimitError".into()),
            latency_ms: 1200,
            upstream_api_key_hash: LedgerEvent::short_hash("public"),
            user_agent_hash: None,
            client_hash: None,
            project_hash: None,
            session_hash: None,
            request_hash: None,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            pool_from: Some("dispatch".into()),
            pool_to: Some("ratelimited".into()),
            attempt: 0,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("FreeUsageLimitError"));
        assert!(!json.contains("u:p@"));
    }

    #[test]
    fn sanitize_text_redacts_key_proxy_and_paths() {
        let input = "key sk-dev proxy socks5h://user:pass@host:1080 path /home/lenovo/app C:\\Users\\Lenovo\\secret";
        let out = sanitize_text(input);
        assert!(!out.contains("sk-dev"));
        assert!(!out.contains("user:pass"));
        assert!(!out.contains("/home/lenovo"));
        assert!(!out.contains("C:\\Users\\Lenovo"));
        assert!(out.contains("[redacted-secret]"));
        assert!(out.contains("***@host:1080"));
    }

    #[test]
    fn sanitize_request_telemetry_hashes_client_and_redacts_node() {
        let mut tele = crate::collector::telemetry::new_telemetry();
        tele.client_id = "sk-dev".into();
        tele.node_url = "socks5h://user:pass@host:1080".into();
        tele.failure_message = "failed at /home/lenovo/app with sk-secret".into();

        let out = sanitize_request_telemetry(&tele);

        assert_ne!(out.client_id, "sk-dev");
        assert!(out.client_id.starts_with("hash:"));
        assert_eq!(out.node_url, "socks5h://***@host:1080");
        assert!(!out.failure_message.contains("/home/lenovo"));
        assert!(!out.failure_message.contains("sk-secret"));
    }

    #[test]
    fn summary_includes_dimensions() {
        let ledger = LedgerCounters::new();
        let ev = LedgerEvent {
            ts: 0,
            rid: "r".into(),
            event_type: "rate_limited".into(),
            node_id: "n1".into(),
            node_url_redacted: "n1".into(),
            model: "big-pickle".into(),
            stream: true,
            status: 429,
            retry_after: None,
            error_type: None,
            error_body_summary: None,
            exit_ip: None,
            latency_ms: 100,
            upstream_api_key_hash: "k1".into(),
            user_agent_hash: None,
            client_hash: None,
            project_hash: None,
            session_hash: None,
            request_hash: None,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            pool_from: None,
            pool_to: None,
            attempt: 0,
        };
        ledger.record(&ev);
        let s = ledger.summary();
        assert_eq!(s["total_requests"], 1);
        assert_eq!(s["429"], 1);
        assert!(s["by_node"]["n1"]["429"] == 1);
    }
}
