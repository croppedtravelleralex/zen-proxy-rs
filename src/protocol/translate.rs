use super::types::*;
use serde_json::Value;

pub fn normalize_model(model: &str) -> String {
    model
        .strip_prefix("opencode/")
        .unwrap_or(model)
        .to_lowercase()
}

pub fn map_upstream_model(model: &str, mappings: &[(String, String)]) -> String {
    mappings
        .iter()
        .find(|(public, _)| public == model)
        .map(|(_, upstream)| upstream.clone())
        .unwrap_or_else(|| model.to_string())
}

pub fn anthropic_to_openai_messages(req: &AnthropicRequest) -> Vec<Message> {
    let mut msgs = Vec::new();
    if let Some(ref sys) = req.system {
        msgs.push(Message {
            role: "system".into(),
            content: sys.clone(),
            tool_calls: None,
        });
    }
    for msg in &req.messages {
        let text = anthropic_content_to_text(&msg.content);
        msgs.push(Message {
            role: msg.role.clone(),
            content: Value::String(text),
            tool_calls: None,
        });
    }
    msgs
}

pub fn anthropic_content_to_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|b| match b.get("type").and_then(|v| v.as_str()) {
                Some("text") => b
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                Some("tool_use") => format!(
                    "Tool requested: {} {}",
                    b.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    b.get("input").map(|v| v.to_string()).unwrap_or_default()
                ),
                Some("tool_result") => format!(
                    "Tool result:\n{}",
                    anthropic_content_to_text(b.get("content").unwrap_or(&Value::Null))
                ),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => content.to_string(),
    }
}

pub fn anthropic_tools_to_openai(tools: &[ToolDef]) -> Vec<OpenAITool> {
    tools.iter().map(|t| OpenAITool {
        tool_type: "function".into(),
        function: OpenAIToolFunction {
            name: t.name.clone(), description: Some(t.description.clone()),
            parameters: Some(serde_json::json!({"type":t.input_schema.schema_type,"required":t.input_schema.required.clone().unwrap_or_default(),"properties":t.input_schema.properties.clone().unwrap_or(Value::Object(Default::default()))})),
        },
    }).collect()
}

pub fn estimate_tokens(text: &str) -> u64 {
    ((text.len() as f64) / 4.0).ceil() as u64
}
pub fn build_prompt_text(msgs: &[Message]) -> String {
    msgs.iter()
        .filter_map(|m| m.content.as_str().map(String::from))
        .collect::<Vec<_>>()
        .join("\n")
}
pub fn has_tools(body: &ChatRequest) -> bool {
    body.tools.as_ref().map(|t| !t.is_empty()).unwrap_or(false)
}
pub fn is_reasoning_only_error(msg: &str) -> bool {
    msg.contains("reasoning_content without final content")
}
