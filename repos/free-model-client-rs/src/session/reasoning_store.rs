use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

static STORE: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();

fn store() -> &'static RwLock<HashMap<String, String>> {
    STORE.get_or_init(|| RwLock::new(HashMap::new()))
}

pub fn assistant_reasoning_key(session_scope: &str, message_index: usize) -> String {
    format!("{session_scope}:assistant:{message_index}")
}

pub fn put_reasoning(key: &str, reasoning: String) {
    if reasoning.trim().is_empty() {
        return;
    }
    if let Ok(mut guard) = store().write() {
        guard.insert(key.to_string(), reasoning);
    }
}

pub fn get_reasoning(key: &str) -> Option<String> {
    store().read().ok()?.get(key).cloned()
}

pub fn session_scope_from_model(model: &str) -> String {
    format!("model:{model}")
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
