use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use super::NodeId;

static PINS: OnceLock<Mutex<HashMap<String, NodeId>>> = OnceLock::new();
static REDIS_URL: OnceLock<Option<String>> = OnceLock::new();
static REDIS_CLIENT: OnceLock<Option<redis::Client>> = OnceLock::new();

const PIN_TTL_SECS: u64 = 86_400;

fn memory_store() -> &'static Mutex<HashMap<String, NodeId>> {
    PINS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn configure(redis_url: Option<String>) {
    let _ = REDIS_URL.set(redis_url.clone());
    if let Some(url) = redis_url {
        let _ = REDIS_CLIENT.set(redis::Client::open(url).ok());
    }
}

pub fn pin_key(upstream_model: &str, session_id: &str) -> String {
    format!("zprs:pin:{upstream_model}:{session_id}")
}

pub fn lookup(upstream_model: &str, session_id: &str) -> Option<NodeId> {
    if session_id.trim().is_empty() || upstream_model.trim().is_empty() {
        return None;
    }
    let key = pin_key(upstream_model, session_id);
    if let Some(node_id) = redis_lookup(&key) {
        return Some(node_id);
    }
    memory_store().lock().ok()?.get(&key).cloned()
}

pub fn record(upstream_model: &str, session_id: &str, node_id: &NodeId) {
    if session_id.trim().is_empty() || upstream_model.trim().is_empty() || node_id.trim().is_empty()
    {
        return;
    }
    let key = pin_key(upstream_model, session_id);
    if redis_record(&key, node_id) {
        return;
    }
    if let Ok(mut guard) = memory_store().lock() {
        guard.insert(key, node_id.clone());
    }
}

pub fn clear(upstream_model: &str, session_id: &str) -> bool {
    if session_id.trim().is_empty() || upstream_model.trim().is_empty() {
        return false;
    }
    let key = pin_key(upstream_model, session_id);
    let mut cleared = redis_clear(&key);
    if let Ok(mut guard) = memory_store().lock() {
        cleared |= guard.remove(&key).is_some();
    }
    cleared
}

pub fn is_mimo_family(model: &str) -> bool {
    let normalized = model.trim().to_ascii_lowercase();
    normalized.contains("mimo")
}

fn redis_lookup(key: &str) -> Option<NodeId> {
    let client = REDIS_CLIENT.get()?.as_ref()?;
    let mut conn = client.get_connection().ok()?;
    let value: Option<String> = redis::cmd("GET").arg(key).query(&mut conn).ok()?;
    value.filter(|node| !node.trim().is_empty())
}

fn redis_record(key: &str, node_id: &NodeId) -> bool {
    let Some(client) = REDIS_CLIENT.get().and_then(|client| client.as_ref()) else {
        return false;
    };
    let Ok(mut conn) = client.get_connection() else {
        return false;
    };
    redis::cmd("SETEX")
        .arg(key)
        .arg(PIN_TTL_SECS)
        .arg(node_id.as_str())
        .query::<()>(&mut conn)
        .is_ok()
}

fn redis_clear(key: &str) -> bool {
    let Some(client) = REDIS_CLIENT.get().and_then(|client| client.as_ref()) else {
        return false;
    };
    let Ok(mut conn) = client.get_connection() else {
        return false;
    };
    redis::cmd("DEL")
        .arg(key)
        .query::<usize>(&mut conn)
        .map(|deleted| deleted > 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_round_trip() {
        record("mimo-v2.5-free", "sess-1", &"node-a".to_string());
        assert_eq!(
            lookup("mimo-v2.5-free", "sess-1").as_deref(),
            Some("node-a")
        );
    }

    #[test]
    fn clear_removes_memory_pin() {
        record(
            "deepseek-v4-flash-free",
            "sess-clear",
            &"node-b".to_string(),
        );
        assert_eq!(
            lookup("deepseek-v4-flash-free", "sess-clear").as_deref(),
            Some("node-b")
        );

        assert!(clear("deepseek-v4-flash-free", "sess-clear"));
        assert_eq!(lookup("deepseek-v4-flash-free", "sess-clear"), None);
    }

    #[test]
    fn mimo_detection() {
        assert!(is_mimo_family("mimo-v2.5"));
        assert!(!is_mimo_family("deepseek-v4-flash"));
    }
}
