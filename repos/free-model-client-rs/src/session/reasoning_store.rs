use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

static STORE: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();
static REDIS_CLIENT: OnceLock<Option<redis::Client>> = OnceLock::new();

const REASONING_TTL_SECS: usize = 86_400;

fn store() -> &'static RwLock<HashMap<String, String>> {
    STORE.get_or_init(|| RwLock::new(HashMap::new()))
}

pub fn configure(redis_url: Option<String>) {
    let _ = REDIS_CLIENT.set(redis_url.and_then(|url| redis::Client::open(url).ok()));
}

pub fn assistant_reasoning_key(session_scope: &str, message_index: usize) -> String {
    format!("{session_scope}:assistant:{message_index}")
}

pub fn put_reasoning(key: &str, reasoning: String) {
    if reasoning.trim().is_empty() {
        return;
    }
    if let Ok(mut guard) = store().write() {
        guard.insert(key.to_string(), reasoning.clone());
    }
    let _ = redis_put(key, &reasoning);
}

pub fn get_reasoning(key: &str) -> Option<String> {
    if let Some(reasoning) = store().read().ok()?.get(key).cloned() {
        return Some(reasoning);
    }
    let reasoning = redis_get(key)?;
    if let Ok(mut guard) = store().write() {
        guard.insert(key.to_string(), reasoning.clone());
    }
    Some(reasoning)
}

pub fn session_scope_from_model(model: &str) -> String {
    format!("model:{model}")
}

fn redis_key(key: &str) -> String {
    format!("fmc:reasoning:{key}")
}

fn redis_put(key: &str, reasoning: &str) -> bool {
    let Some(client) = REDIS_CLIENT.get().and_then(|client| client.as_ref()) else {
        return false;
    };
    let Ok(mut conn) = client.get_connection() else {
        return false;
    };
    redis::cmd("SETEX")
        .arg(redis_key(key))
        .arg(REASONING_TTL_SECS)
        .arg(reasoning)
        .query::<()>(&mut conn)
        .is_ok()
}

fn redis_get(key: &str) -> Option<String> {
    let client = REDIS_CLIENT.get()?.as_ref()?;
    let mut conn = client.get_connection().ok()?;
    redis::cmd("GET")
        .arg(redis_key(key))
        .query::<Option<String>>(&mut conn)
        .ok()?
        .filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_round_trip() {
        let key = assistant_reasoning_key("test-session", 3);
        put_reasoning(&key, "chain of thought".to_string());
        assert_eq!(get_reasoning(&key).as_deref(), Some("chain of thought"));
    }
}
