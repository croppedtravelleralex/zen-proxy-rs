use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

fn secret_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            Regex::new(r"(?i)\b(sk-[A-Za-z0-9][A-Za-z0-9_\-]{4,})\b").unwrap(),
            Regex::new(
                r"(?i)\b(api[_-]?key|newapi[_-]?key|password|token|secret)\s*=\s*([^\s\r\n]+)",
            )
            .unwrap(),
            Regex::new(r"\b([A-Za-z0-9_.-]+:\d{2,5}:)([^:\s]+):([^:\s]+)\b").unwrap(),
            Regex::new(
                r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
            )
            .unwrap(),
        ]
    })
}

pub fn redact_text(input: &str) -> String {
    let mut output = input.to_string();
    output = secret_patterns()[0]
        .replace_all(&output, "[REDACTED_SECRET_KEY]")
        .into_owned();
    output = secret_patterns()[1]
        .replace_all(&output, "${1}=[REDACTED]")
        .into_owned();
    output = secret_patterns()[2]
        .replace_all(&output, "${1}[REDACTED]:[REDACTED]")
        .into_owned();
    output = secret_patterns()[3]
        .replace_all(&output, "[REDACTED_PRIVATE_KEY]")
        .into_owned();
    output
}

pub fn redact_value(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(redact_text(text)),
        Value::Array(items) => Value::Array(items.iter().map(redact_value).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), redact_value(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redacts_common_secret_shapes() {
        let text = "API_KEY=abc123\nNEWAPI_KEY=sk-fake-do-not-leak\nproxy.example:8080:user:pass";
        let redacted = redact_text(text);
        assert!(!redacted.contains("abc123"));
        assert!(!redacted.contains("sk-fake-do-not-leak"));
        assert!(!redacted.contains("user:pass"));
        assert!(redacted.contains("API_KEY=[REDACTED]"));
    }

    #[test]
    fn redacts_nested_json_strings() {
        let value = json!({"content":["PASSWORD=hunter2", {"key":"sk-secret-value"}]});
        let redacted = redact_value(&value);
        let serialized = redacted.to_string();
        assert!(!serialized.contains("hunter2"));
        assert!(!serialized.contains("sk-secret-value"));
    }
}
