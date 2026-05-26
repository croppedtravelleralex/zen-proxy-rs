use super::types::*;
use serde_json::Value;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ToolHistoryRepair {
    pub synthetic_tool_ids: usize,
    pub paired_tool_results: usize,
    pub downgraded_tool_results: usize,
    pub downgraded_assistant_calls: usize,
}

#[derive(Debug)]
struct PendingToolCallState {
    id: String,
    message_index: usize,
    used: bool,
}

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
            tool_call_id: None,
        });
    }
    for msg in &req.messages {
        msgs.extend(anthropic_message_to_openai_messages(msg));
    }
    msgs
}

pub fn canonicalize_openai_tool_history(messages: &mut [Message]) -> ToolHistoryRepair {
    let mut repair = ToolHistoryRepair::default();
    let mut pending = Vec::<PendingToolCallState>::new();

    for message_index in 0..messages.len() {
        let role = messages[message_index].role.clone();
        if role != "tool" && !pending.iter().all(|item| item.used) {
            downgrade_unresolved_pending(messages, &mut pending, &mut repair);
        }

        match role.as_str() {
            "assistant" => {
                let message = &mut messages[message_index];
                let Some(calls) = message.tool_calls.as_mut() else {
                    continue;
                };
                for (tool_index, call) in calls.iter_mut().enumerate() {
                    if call.id.as_deref().map(str::trim).is_none_or(str::is_empty) {
                        call.id = Some(synthetic_tool_id(message_index, tool_index, call));
                        repair.synthetic_tool_ids += 1;
                    }
                    if let Some(id) = call.id.as_ref().filter(|id| !id.trim().is_empty()) {
                        pending.push(PendingToolCallState {
                            id: id.clone(),
                            message_index,
                            used: false,
                        });
                    }
                }
            }
            "tool" => {
                let message = &mut messages[message_index];
                let original_id = message
                    .tool_call_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .map(ToOwned::to_owned);
                let matched = message
                    .tool_call_id
                    .as_deref()
                    .filter(|id| !id.trim().is_empty())
                    .and_then(|id| mark_pending_used(&mut pending, id))
                    .or_else(|| consume_next_pending(&mut pending));

                if let Some(id) = matched {
                    if original_id.as_deref() != Some(id.as_str()) {
                        message.tool_call_id = Some(id);
                        repair.paired_tool_results += 1;
                    }
                } else {
                    downgrade_tool_message(message);
                    repair.downgraded_tool_results += 1;
                }
            }
            _ => {}
        }
    }

    downgrade_unresolved_pending(messages, &mut pending, &mut repair);
    repair
}

fn downgrade_unresolved_pending(
    messages: &mut [Message],
    pending: &mut Vec<PendingToolCallState>,
    repair: &mut ToolHistoryRepair,
) {
    let unresolved = pending
        .iter()
        .filter(|item| !item.used)
        .map(|item| (item.message_index, item.id.clone()))
        .collect::<Vec<_>>();
    for (message_index, id) in unresolved {
        let Some(message) = messages.get_mut(message_index) else {
            continue;
        };
        let Some(calls) = message.tool_calls.as_mut() else {
            continue;
        };
        let before = calls.len();
        calls.retain(|call| call.id.as_deref() != Some(id.as_str()));
        let removed = before.saturating_sub(calls.len());
        repair.downgraded_assistant_calls += removed;
        if calls.is_empty() {
            message.tool_calls = None;
            if message.content.is_null()
                || message
                    .content
                    .as_str()
                    .is_some_and(|content| content.trim().is_empty())
            {
                message.content = Value::String(
                    "[Tool call recovered as plain context: matching tool result missing]"
                        .to_string(),
                );
            }
        }
    }
    pending.clear();
}

fn mark_pending_used(pending: &mut [PendingToolCallState], id: &str) -> Option<String> {
    let call = pending
        .iter_mut()
        .find(|item| item.id == id && !item.used)?;
    call.used = true;
    Some(call.id.clone())
}

fn consume_next_pending(pending: &mut [PendingToolCallState]) -> Option<String> {
    let call = pending.iter_mut().find(|item| !item.used)?;
    call.used = true;
    Some(call.id.clone())
}

fn downgrade_tool_message(message: &mut Message) {
    let preview = message
        .content
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| message.content.to_string());
    message.role = "user".to_string();
    message.tool_call_id = None;
    message.tool_calls = None;
    message.content = Value::String(format!(
        "[Tool result recovered as plain context: original tool_call_id missing or invalid]\n{preview}"
    ));
}

fn synthetic_tool_id(message_index: usize, tool_index: usize, call: &ToolCall) -> String {
    let hash = stable_hash64(&format!(
        "{}:{}:{}:{}",
        message_index, tool_index, call.function.name, call.function.arguments
    ));
    format!("call_fmc_{message_index}_{tool_index}_{hash:016x}")
}

fn stable_hash64(input: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn anthropic_message_to_openai_messages(msg: &AnthropicMessage) -> Vec<Message> {
    match msg.role.as_str() {
        "assistant" => vec![anthropic_assistant_to_openai_message(&msg.content)],
        "user" => anthropic_user_to_openai_messages(&msg.content),
        _ => vec![Message {
            role: msg.role.clone(),
            content: Value::String(anthropic_content_to_text(&msg.content)),
            tool_calls: None,
            tool_call_id: None,
        }],
    }
}

fn anthropic_assistant_to_openai_message(content: &Value) -> Message {
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();

    if let Value::Array(blocks) = content {
        for block in blocks {
            match block.get("type").and_then(|v| v.as_str()) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            text_parts.push(text.to_string());
                        }
                    }
                }
                Some("tool_use") => {
                    if let Some(name) = block.get("name").and_then(|v| v.as_str()) {
                        let input = block.get("input").cloned().unwrap_or(Value::Null);
                        tool_calls.push(ToolCall {
                            id: block
                                .get("id")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            call_type: "function".to_string(),
                            function: ToolFunction {
                                name: name.to_string(),
                                arguments: serde_json::to_string(&input).unwrap_or_default(),
                            },
                            index: Some(tool_calls.len() as i64),
                        });
                    }
                }
                _ => {}
            }
        }
    } else {
        text_parts.push(anthropic_content_to_text(content));
    }

    Message {
        role: "assistant".to_string(),
        content: if text_parts.is_empty() {
            Value::Null
        } else {
            Value::String(text_parts.join("\n"))
        },
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        tool_call_id: None,
    }
}

fn anthropic_user_to_openai_messages(content: &Value) -> Vec<Message> {
    let Value::Array(blocks) = content else {
        return vec![Message {
            role: "user".to_string(),
            content: Value::String(anthropic_content_to_text(content)),
            tool_calls: None,
            tool_call_id: None,
        }];
    };

    let mut tool_messages = Vec::new();
    let mut user_text = Vec::new();
    for block in blocks {
        match block.get("type").and_then(|v| v.as_str()) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        user_text.push(text.to_string());
                    }
                }
            }
            Some("tool_result") => {
                tool_messages.push(Message {
                    role: "tool".to_string(),
                    content: crate::redact::redact_value(&Value::String(
                        anthropic_content_to_text(block.get("content").unwrap_or(&Value::Null)),
                    )),
                    tool_calls: None,
                    tool_call_id: block
                        .get("tool_use_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                });
            }
            _ => {}
        }
    }
    let mut messages = tool_messages;
    if !user_text.is_empty() {
        messages.push(Message {
            role: "user".to_string(),
            content: Value::String(user_text.join("\n")),
            tool_calls: None,
            tool_call_id: None,
        });
    }
    messages
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
                Some("tool_use") => String::new(),
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

pub fn anthropic_tool_choice_to_openai(choice: &Value) -> Value {
    match choice.get("type").and_then(Value::as_str) {
        Some("auto") => Value::String("auto".to_string()),
        Some("any") => Value::String("required".to_string()),
        Some("tool") => choice
            .get("name")
            .and_then(Value::as_str)
            .map(|name| {
                serde_json::json!({
                    "type": "function",
                    "function": { "name": name }
                })
            })
            .unwrap_or_else(|| Value::String("required".to_string())),
        _ => choice.clone(),
    }
}

pub fn disable_thinking_for_assistant_history(body: &mut Value, messages: &[Message]) {
    let has_assistant_history = messages.iter().any(|msg| msg.role == "assistant");
    if !has_assistant_history || body.get("thinking").is_some() {
        return;
    }
    body["thinking"] = serde_json::json!({"type":"disabled"});
}

pub fn disable_thinking_by_default(body: &mut Value) {
    if body.get("thinking").is_some() {
        return;
    }
    body["thinking"] = serde_json::json!({"type":"disabled"});
}

pub fn disable_thinking_for_tool_use(body: &mut Value) {
    if body.get("thinking").is_some() {
        return;
    }
    let has_tools = body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty());
    let has_tool_choice = body
        .get("tool_choice")
        .is_some_and(|choice| !choice.is_null());
    if has_tools || has_tool_choice {
        body["thinking"] = serde_json::json!({"type":"disabled"});
    }
}

pub fn stabilize_short_user_prompt(body: &mut Value) {
    if body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty())
    {
        return;
    }

    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    let Some(last_user) = messages
        .iter_mut()
        .rev()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
    else {
        return;
    };
    let Some(content) = last_user.get_mut("content") else {
        return;
    };
    let Some(text) = content.as_str() else {
        return;
    };
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 2 {
        return;
    }

    *content = Value::String("只回复 ok".to_string());
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

pub fn is_short_no_tool_health_request(body: &ChatRequest) -> bool {
    if has_tools(body) || body.tool_choice.is_some() {
        return false;
    }

    let user_messages = body
        .messages
        .iter()
        .filter(|msg| msg.role == "user")
        .collect::<Vec<_>>();
    if user_messages.len() != 1 || body.messages.iter().any(|msg| msg.role == "assistant") {
        return false;
    }

    let Some(text) = user_messages[0].content.as_str() else {
        return false;
    };
    let trimmed = text.trim();
    !trimmed.is_empty() && trimmed.chars().count() <= 2
}

pub fn is_reasoning_only_error(msg: &str) -> bool {
    msg.contains("reasoning_content without final content")
}
