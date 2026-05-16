use std::sync::LazyLock;
use std::collections::HashMap;
use crate::config::Config;

pub static MODEL_MAPPING: LazyLock<HashMap<&str, &str>> = LazyLock::new(|| {
    HashMap::from([
        ("deepseek-v4-flash", "big-pickle"),
        ("deepseek-v4-flash-lite", "big-pickle-nothinking"),
        ("deepseek-v4-pro", "deepseek-v4-flash-free"),
        ("deepseek-v4-pro-lite", "deepseek-v4-flash-nothinking"),
    ])
});

pub fn apply_model_override(body: &[u8], config: &Config) -> Vec<u8> {
    let mut root: serde_json::Value =
        serde_json::from_slice(body).unwrap_or(serde_json::Value::Null);
    if root.is_null() {
        return body.to_vec();
    }
    let mapped = if let Some(override_model) = &config.model_override {
        Some(override_model.as_str())
    } else if let Some(current_model) = root.get("model").and_then(|m| m.as_str()) {
        config
            .model_mapping
            .get(current_model)
            .map(|s| s.as_str())
            .or_else(|| MODEL_MAPPING.get(current_model).copied())
    } else {
        None
    };
    if let Some(resolved) = mapped {
        root["model"] = serde_json::Value::String(resolved.to_string());
        if resolved.ends_with("-nothinking") {
            let mut thinking = serde_json::Map::new();
            thinking.insert("type".to_string(), serde_json::Value::String("disabled".to_string()));
            root["thinking"] = serde_json::Value::Object(thinking);
        }
    }
    if let Some(messages) = root.get("messages").and_then(|m| m.as_array()) {
        let has_assistant_without_reasoning = messages.iter().any(|msg| {
            msg.get("role").and_then(|r| r.as_str()) == Some("assistant")
                && msg.get("reasoning_content").is_none()
        });
        if has_assistant_without_reasoning && root.get("thinking").is_none() {
            let mut thinking = serde_json::Map::new();
            thinking.insert("type".to_string(), serde_json::Value::String("disabled".to_string()));
            root["thinking"] = serde_json::Value::Object(thinking);
        }
    }
    serde_json::to_vec(&root).unwrap_or_else(|_| body.to_vec())
}

pub fn patch_sse_line(line: &[u8]) -> Vec<u8> {
    let (payload, trailing) = if line.ends_with(b"\n") {
        (&line[..line.len() - 1], Some(b'\n'))
    } else {
        (line, None)
    };
    let prefix = b"data: ";
    if payload.len() < prefix.len() || &payload[..prefix.len()] != prefix {
        return line.to_vec();
    }
    let json_str = &payload[prefix.len()..];
    if json_str == b"[DONE]" {
        return line.to_vec();
    }
    let mut val: serde_json::Value = match serde_json::from_slice(json_str) {
        Ok(v) => v,
        Err(_) => return line.to_vec(),
    };
    let obj = match val.as_object_mut() {
        Some(o) => o,
        None => return line.to_vec(),
    };
    let reasoning = match obj.get("reasoning_content").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return line.to_vec(),
    };
    let cv = obj.get("content").and_then(|v| v.as_str()).unwrap_or("");
    if cv.is_empty() {
        obj.insert("content".to_string(), serde_json::Value::String(reasoning));
    }
    let mut out = Vec::with_capacity(prefix.len() + 128);
    out.extend_from_slice(prefix);
    let _ = serde_json::to_writer(&mut out, &val);
    if let Some(c) = trailing {
        out.push(c);
    }
    out
}

// patch_response_content: Move reasoning_content into content when content is empty.
// Uses indexing-based access to avoid Rust borrow-checker conflicts with serde_json::Value.
pub fn patch_response_content(content: &[u8]) -> Vec<u8> {
    let mut root: serde_json::Value = match serde_json::from_slice(content) {
        Ok(v) => v,
        Err(_) => return content.to_vec(),
    };
    match root.get_mut("choices").and_then(|c| c.as_array_mut()) {
        None => return content.to_vec(),
        Some(choices) => {
            let mut changed = false;
            for choice in choices.iter_mut() {
                // Extract needed data as owned values first, then mutate separately.
                let delta_key = if choice.get("delta").is_some() { "delta" } else { "message" };
                let reasoning = choice
                    .get(delta_key)
                    .and_then(|d| d.get("reasoning_content"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let content_empty = choice
                    .get(delta_key)
                    .and_then(|d| d.get("content"))
                    .and_then(|v| v.as_str())
                    .map_or(true, |c| c.is_empty());

                if let Some(r) = reasoning {
                    if content_empty {
                        choice[delta_key]["content"] = serde_json::Value::String(r);
                        changed = true;
                    }
                }
            }
            if changed {
                serde_json::to_vec(&root).unwrap_or_else(|_| content.to_vec())
            } else {
                content.to_vec()
            }
        }
    }
}

pub fn build_upstream_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    let mut url = format!("{}/{}", base, path);
    if url.ends_with('/') && url.len() > 1 {
        url.truncate(url.len() - 1);
    }
    url
}

pub fn smart_backoff(attempt: u32, status: Option<u16>) -> f64 {
    use rand::Rng;
    let base = match status {
        Some(429) => 0.5,
        Some(s) if (500..600).contains(&s) => 1.0,
        _ => 0.2,
    };
    let delay = base * (2u64.pow(attempt) as f64);
    match status {
        Some(429) => {
            let jitter: f64 = rand::thread_rng().gen_range(0.0..0.25);
            delay + jitter
        }
        Some(s) if (500..600).contains(&s) => delay.min(8.0),
        _ => delay,
    }
}

pub fn should_retry(status: u16, attempt: u32, max_retries: u32) -> bool {
    if attempt >= max_retries {
        return false;
    }
    matches!(status, 429 | 500 | 502 | 503 | 504)
}
