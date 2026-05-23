//! OpenAI-compatible SSE (Server-Sent Events) formatting utilities.
//!
//! Format is EXACTLY: `data: {json}\n\n` (double newline).
//! The final event is: `data: [DONE]\n\n`.

use serde::Serialize;
use serde_json;

/// Role sentinel included in the very first delta chunk (cargo-cult from opencode).
#[derive(Debug, Clone, Serialize)]
pub struct DeltaRole {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// Return the standard `data: [DONE]\n\n` terminal SSE frame.
pub fn openai_sse_done() -> String {
    "data: [DONE]\n\n".to_string()
}

/// Build a single SSE chunk in OpenAI chunk format.
///
/// Parameters:
/// - `id` — chatcmpl_xxx
/// - `model` — model name
/// - `delta_content` — text content delta (if any)
/// - `delta_tool_calls` — tool call array delta (if any)
/// - `finish_reason` — "stop", "length", "tool_calls", or null
/// - `usage` — optional usage object for the final chunk
pub fn openai_sse_chunk(
    id: &str,
    model: &str,
    delta_content: Option<&str>,
    delta_tool_calls: Option<&[crate::protocol::types::ToolCall]>,
    finish_reason: Option<&str>,
    usage: Option<&crate::protocol::types::Usage>,
) -> String {
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut delta_map = serde_json::Map::new();
    if let Some(content) = delta_content {
        delta_map.insert("content".to_string(), serde_json::Value::String(content.to_string()));
    }
    if let Some(tool_calls) = delta_tool_calls {
        if let Ok(val) = serde_json::to_value(tool_calls) {
            delta_map.insert("tool_calls".to_string(), val);
        }
    }

    let mut chunk = serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta_map,
            "finish_reason": finish_reason,
        }]
    });

    if let Some(u) = usage {
        if let Ok(val) = serde_json::to_value(u) {
            chunk["usage"] = val;
        }
    }

    format!("data: {}\n\n", serde_json::to_string(&chunk).unwrap_or_default())
}

/// Build the initial role-sentinel chunk that opencode clients expect.
pub fn openai_sse_role_chunk(id: &str, model: &str) -> String {
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let chunk = serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": { "role": "assistant", "content": "" },
            "finish_reason": null,
        }]
    });

    format!("data: {}\n\n", serde_json::to_string(&chunk).unwrap_or_default())
}
