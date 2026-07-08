use super::types::*;
use serde_json::Value;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ToolHistoryRepair {
    pub synthetic_tool_ids: usize,
    pub paired_tool_results: usize,
    pub downgraded_tool_results: usize,
    pub downgraded_assistant_calls: usize,
    pub stabilized_tool_call_ids: usize,
}

#[derive(Debug)]
struct PendingToolCallState {
    id: String,
    message_index: usize,
    used: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolHistoryPolicy {
    Strict,
    Compat,
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
        let system_text = anthropic_system_to_openai_text(sys);
        if !system_text.trim().is_empty() {
            msgs.push(Message {
                role: "system".into(),
                content: Value::String(system_text),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            });
        }
    }
    for msg in &req.messages {
        msgs.extend(anthropic_message_to_openai_messages(msg));
    }
    msgs
}

pub fn canonicalize_openai_tool_history(messages: &mut [Message]) -> ToolHistoryRepair {
    canonicalize_openai_tool_history_with_policy(messages, ToolHistoryPolicy::Compat)
}

pub fn canonicalize_openai_tool_history_with_policy(
    messages: &mut [Message],
    policy: ToolHistoryPolicy,
) -> ToolHistoryRepair {
    let mut repair = ToolHistoryRepair::default();
    let mut pending = Vec::<PendingToolCallState>::new();

    for message_index in 0..messages.len() {
        let role = messages[message_index].role.clone();
        if role != "tool" && !pending.iter().all(|item| item.used) {
            downgrade_unresolved_pending(messages, &mut pending, &mut repair, policy);
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
                    downgrade_tool_message(message, policy);
                    repair.downgraded_tool_results += 1;
                }
            }
            _ => {}
        }
    }

    downgrade_unresolved_pending(messages, &mut pending, &mut repair, policy);
    repair.stabilized_tool_call_ids += stabilize_tool_call_ids(messages);
    repair
}

fn downgrade_unresolved_pending(
    messages: &mut [Message],
    pending: &mut Vec<PendingToolCallState>,
    repair: &mut ToolHistoryRepair,
    policy: ToolHistoryPolicy,
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
                message.content = match policy {
                    ToolHistoryPolicy::Compat => Value::String(
                        "[Tool call recovered as plain context: matching tool result missing]"
                            .to_string(),
                    ),
                    ToolHistoryPolicy::Strict => Value::String(String::new()),
                };
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

fn downgrade_tool_message(message: &mut Message, policy: ToolHistoryPolicy) {
    let preview = message
        .content
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| message.content.to_string());
    message.role = "user".to_string();
    message.tool_call_id = None;
    message.tool_calls = None;
    message.content = match policy {
        ToolHistoryPolicy::Compat => Value::String(format!(
            "[Tool result recovered as plain context: original tool_call_id missing or invalid]\n{preview}"
        )),
        ToolHistoryPolicy::Strict => Value::String(preview),
    };
}

fn synthetic_tool_id(message_index: usize, tool_index: usize, call: &ToolCall) -> String {
    let hash = stable_hash64(&format!(
        "{}:{}:{}:{}",
        message_index, tool_index, call.function.name, call.function.arguments
    ));
    format!("call_fmc_{message_index}_{tool_index}_{hash:016x}")
}

fn stabilize_tool_call_ids(messages: &mut [Message]) -> usize {
    use std::collections::HashMap;

    let mut changed = 0usize;
    let mut id_map = HashMap::<String, String>::new();
    for (message_index, message) in messages.iter_mut().enumerate() {
        if let Some(calls) = message.tool_calls.as_mut() {
            for (tool_index, call) in calls.iter_mut().enumerate() {
                let stable_id = synthetic_tool_id(message_index, tool_index, call);
                if let Some(old_id) = call.id.clone().filter(|id| !id.trim().is_empty()) {
                    id_map.insert(old_id, stable_id.clone());
                }
                if call.id.as_deref() != Some(stable_id.as_str()) {
                    call.id = Some(stable_id);
                    changed += 1;
                }
            }
        }
        if message.role == "tool" {
            let Some(old_id) = message.tool_call_id.clone() else {
                continue;
            };
            let Some(stable_id) = id_map.get(&old_id) else {
                continue;
            };
            if old_id != *stable_id {
                message.tool_call_id = Some(stable_id.clone());
                changed += 1;
            }
        }
    }
    changed
}

fn stable_hash64(input: &str) -> u64 {
    stable_hash64_update(0xcbf29ce484222325u64, input.as_bytes())
}

fn stable_hash64_update(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
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
            reasoning_content: None,
        }],
    }
}

fn anthropic_assistant_to_openai_message(content: &Value) -> Message {
    let mut text_parts = Vec::new();
    let mut reasoning_parts = Vec::new();
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
                Some("thinking") => {
                    if let Some(text) = block
                        .get("thinking")
                        .and_then(|v| v.as_str())
                        .or_else(|| block.get("text").and_then(|v| v.as_str()))
                    {
                        if !text.is_empty() {
                            reasoning_parts.push(text.to_string());
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
        reasoning_content: if reasoning_parts.is_empty() {
            None
        } else {
            Some(reasoning_parts.join("\n"))
        },
    }
}

fn anthropic_user_to_openai_messages(content: &Value) -> Vec<Message> {
    let Value::Array(blocks) = content else {
        return vec![Message {
            role: "user".to_string(),
            content: Value::String(anthropic_content_to_text(content)),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
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
                    reasoning_content: None,
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
            reasoning_content: None,
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

fn anthropic_system_to_openai_text(content: &Value) -> String {
    strip_anthropic_billing_header_lines(&anthropic_content_to_text(content))
}

fn strip_anthropic_billing_header_lines(text: &str) -> String {
    text.lines()
        .filter(|line| !line.trim_start().starts_with("x-anthropic-billing-header:"))
        .collect::<Vec<_>>()
        .join("\n")
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

pub fn disable_thinking_for_assistant_history(_body: &mut Value, _messages: &[Message]) {
    // V4.6 keeps model reasoning available for normal multi-turn context.
}

pub fn disable_thinking_by_default(_body: &mut Value) {
    // V4.6 no longer disables thinking for ordinary requests by default.
}

pub fn set_thinking_disabled_if_absent(body: &mut Value) -> bool {
    if body.get("thinking").is_some() {
        return false;
    }
    body["thinking"] = serde_json::json!({"type":"disabled"});
    true
}

pub fn disable_thinking_for_tool_use(body: &mut Value) -> bool {
    if body.get("thinking").is_some() {
        return false;
    }
    let has_tools = body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty());
    let has_tool_choice = body
        .get("tool_choice")
        .is_some_and(|choice| !choice.is_null());
    if has_tools || has_tool_choice {
        return set_thinking_disabled_if_absent(body);
    }
    false
}

pub fn stabilize_short_user_prompt(_body: &mut Value) {
    // Preserve terse user intent such as "1", "继续", and "执行".
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NonStreamOutputPolicy {
    pub prompt_tokens: u64,
    pub requested_max_tokens: Option<u64>,
    pub effective_max_tokens: Option<u64>,
    pub capped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamOutputPolicy {
    pub prompt_tokens: u64,
    pub requested_max_tokens: Option<u64>,
    pub effective_max_tokens: Option<u64>,
    pub capped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestShape {
    pub system_tokens: u64,
    pub messages_tokens: u64,
    pub tools_tokens: u64,
    pub tool_count: usize,
    pub tool_name_classes: Vec<&'static str>,
    pub message_count: usize,
    pub largest_message_tokens: u64,
    pub last_user_tokens: u64,
    pub estimated_total_tokens: u64,
    pub stream: bool,
    pub max_tokens: Option<u64>,
    pub tool_choice_present: bool,
    pub prompt_hash: u64,
    pub prefix_4k_hash: u64,
    pub prefix_32k_hash: u64,
    pub prefix_128k_hash: u64,
    pub prefix_256k_hash: u64,
    pub cache_material_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortNonStreamRequestKind {
    NotShortNonStream,
    HealthProbe,
    ChannelTest,
    InternalClaudeCodeProbe,
    UserShortRequest,
    UnknownShortNonStream,
}

impl ShortNonStreamRequestKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotShortNonStream => "not_short_nonstream",
            Self::HealthProbe => "health_probe",
            Self::ChannelTest => "channel_test",
            Self::InternalClaudeCodeProbe => "internal_claude_code_probe",
            Self::UserShortRequest => "user_short_request",
            Self::UnknownShortNonStream => "unknown_short_nonstream",
        }
    }
}

const CLAUDE_CODE_LOW_BUDGET_TOOL_PROBE_MAX_REQUEST_TOKENS: u64 = 32;
const CLAUDE_CODE_LOW_BUDGET_TOOL_PROBE_MAX_TOTAL_TOKENS: u64 = 2_048;
pub const CLAUDE_CODE_LOW_BUDGET_TOOL_PROBE_MIN_OUTPUT_TOKENS: u64 = 64;

pub fn request_shape(body: &ChatRequest) -> RequestShape {
    let mut system_tokens = 0u64;
    let mut messages_tokens = 0u64;
    let mut largest_message_tokens = 0u64;
    let mut last_user_tokens = 0u64;

    for message in &body.messages {
        let tokens = value_shape_tokens(&message.content);
        largest_message_tokens = largest_message_tokens.max(tokens);
        if message.role == "system" {
            system_tokens = system_tokens.saturating_add(tokens);
        } else {
            messages_tokens = messages_tokens.saturating_add(tokens);
        }
        if message.role == "user" {
            last_user_tokens = tokens;
        }
    }

    let (tool_count, tools_tokens, tool_name_classes) = body
        .tools
        .as_ref()
        .map(|tools| {
            let rendered = serde_json::to_string(tools).unwrap_or_default();
            let mut classes = tools
                .iter()
                .map(|tool| tool_name_class(&tool.function.name))
                .collect::<Vec<_>>();
            classes.sort_unstable();
            classes.dedup();
            (tools.len(), estimate_tokens(&rendered), classes)
        })
        .unwrap_or((0, 0, Vec::new()));
    let estimated_total_tokens = system_tokens
        .saturating_add(messages_tokens)
        .saturating_add(tools_tokens);
    let prompt_hash = request_prompt_hash(body, tool_count);
    let cache_material = request_cache_material(body);
    let cache_material_bytes = cache_material.len();
    let prefix_4k_hash = request_cache_prefix_hash(&cache_material, 4 * 1024);
    let prefix_32k_hash = request_cache_prefix_hash(&cache_material, 32 * 1024);
    let prefix_128k_hash = request_cache_prefix_hash(&cache_material, 128 * 1024);
    let prefix_256k_hash = request_cache_prefix_hash(&cache_material, 256 * 1024);

    RequestShape {
        system_tokens,
        messages_tokens,
        tools_tokens,
        tool_count,
        tool_name_classes,
        message_count: body.messages.len(),
        largest_message_tokens,
        last_user_tokens,
        estimated_total_tokens,
        stream: body.stream.unwrap_or(false),
        max_tokens: body.max_tokens,
        tool_choice_present: body.tool_choice.is_some(),
        prompt_hash,
        prefix_4k_hash,
        prefix_32k_hash,
        prefix_128k_hash,
        prefix_256k_hash,
        cache_material_bytes,
    }
}

pub fn tool_name_class(name: &str) -> &'static str {
    let normalized = name.trim().to_ascii_lowercase().replace(['-', '.'], "_");
    match normalized.as_str() {
        "web_search" | "websearch" | "web" | "search" => "web_search",
        "web_fetch" | "webfetch" | "fetch" | "fetch_url" => "web_fetch",
        "task" | "subagent" | "sub_agent" => "task",
        "bash" | "shell" | "exec" | "execute" | "run_command" => "shell",
        "read" | "write" | "edit" | "multiedit" | "read_file" | "write_file" | "edit_file" => {
            "file"
        }
        "todowrite" | "todo_write" | "todo" => "todo",
        "memorysearch" | "memory_search" | "memoryread" | "memory_read" => "memory",
        "mcp__cherryhub__list"
        | "mcp__cherryhub__inspect"
        | "mcp__cherryhub__invoke"
        | "mcp__cherryhub__exec" => "mcp",
        _ if normalized.starts_with("mcp__") => "mcp",
        _ if normalized.contains("web_search") => "web_search",
        _ if normalized.contains("web_fetch") => "web_fetch",
        _ => "other",
    }
}

pub fn classify_short_non_stream_request(
    body: &ChatRequest,
    is_claude_code: bool,
) -> ShortNonStreamRequestKind {
    if body.stream.unwrap_or(false) {
        return ShortNonStreamRequestKind::NotShortNonStream;
    }
    let shape = request_shape(body);
    if is_claude_code_low_budget_tool_probe_shape(body, &shape, is_claude_code) {
        return ShortNonStreamRequestKind::InternalClaudeCodeProbe;
    }
    if shape.tool_count > 0 || shape.tool_choice_present {
        return ShortNonStreamRequestKind::NotShortNonStream;
    }
    if is_short_no_tool_health_request(body) {
        return ShortNonStreamRequestKind::HealthProbe;
    }
    if is_short_no_tool_channel_test_probe(body) {
        return ShortNonStreamRequestKind::ChannelTest;
    }
    if shape.message_count > 4 || shape.estimated_total_tokens > 256 {
        return ShortNonStreamRequestKind::NotShortNonStream;
    }
    if is_claude_code && shape.message_count <= 2 && shape.last_user_tokens <= 64 {
        return ShortNonStreamRequestKind::InternalClaudeCodeProbe;
    }

    let user_messages = body
        .messages
        .iter()
        .filter(|message| message.role == "user")
        .count();
    let has_assistant = body
        .messages
        .iter()
        .any(|message| message.role == "assistant");
    if user_messages == 1 && !has_assistant {
        return ShortNonStreamRequestKind::UserShortRequest;
    }
    ShortNonStreamRequestKind::UnknownShortNonStream
}

pub fn is_claude_code_low_budget_tool_probe(body: &ChatRequest, is_claude_code: bool) -> bool {
    let shape = request_shape(body);
    is_claude_code_low_budget_tool_probe_shape(body, &shape, is_claude_code)
}

fn is_claude_code_low_budget_tool_probe_shape(
    body: &ChatRequest,
    shape: &RequestShape,
    is_claude_code: bool,
) -> bool {
    if !is_claude_code || body.stream.unwrap_or(false) {
        return false;
    }
    let Some(max_tokens) = body.max_tokens else {
        return false;
    };
    max_tokens <= CLAUDE_CODE_LOW_BUDGET_TOOL_PROBE_MAX_REQUEST_TOKENS
        && (1..=2).contains(&shape.tool_count)
        && !shape.tool_choice_present
        && shape.message_count <= 2
        && shape.last_user_tokens <= 64
        && shape.estimated_total_tokens <= CLAUDE_CODE_LOW_BUDGET_TOOL_PROBE_MAX_TOTAL_TOKENS
}

pub fn claude_code_low_budget_tool_probe_max_tokens(
    body: &ChatRequest,
    is_claude_code: bool,
) -> Option<u64> {
    if is_claude_code_low_budget_tool_probe(body, is_claude_code) {
        return body
            .max_tokens
            .map(|max_tokens| max_tokens.max(CLAUDE_CODE_LOW_BUDGET_TOOL_PROBE_MIN_OUTPUT_TOKENS));
    }
    body.max_tokens
}

pub fn claude_code_low_budget_probe_max_tokens(
    body: &ChatRequest,
    is_claude_code: bool,
) -> Option<u64> {
    let shape = request_shape(body);
    if is_claude_code_low_budget_tool_probe_shape(body, &shape, is_claude_code)
        || is_claude_code_low_budget_no_tool_probe_shape(body, &shape, is_claude_code)
    {
        return body
            .max_tokens
            .map(|max_tokens| max_tokens.max(CLAUDE_CODE_LOW_BUDGET_TOOL_PROBE_MIN_OUTPUT_TOKENS));
    }
    body.max_tokens
}

fn is_claude_code_low_budget_no_tool_probe_shape(
    body: &ChatRequest,
    shape: &RequestShape,
    is_claude_code: bool,
) -> bool {
    if !is_claude_code || body.stream.unwrap_or(false) {
        return false;
    }
    let Some(max_tokens) = body.max_tokens else {
        return false;
    };
    max_tokens <= CLAUDE_CODE_LOW_BUDGET_TOOL_PROBE_MAX_REQUEST_TOKENS
        && shape.tool_count == 0
        && !shape.tool_choice_present
        && shape.message_count <= 2
        && shape.last_user_tokens <= 64
        && shape.estimated_total_tokens <= CLAUDE_CODE_LOW_BUDGET_TOOL_PROBE_MAX_TOTAL_TOKENS
}

fn value_shape_tokens(value: &Value) -> u64 {
    match value {
        Value::String(text) => estimate_tokens(text),
        Value::Null => 0,
        other => estimate_tokens(&serde_json::to_string(other).unwrap_or_default()),
    }
}

fn request_prompt_hash(body: &ChatRequest, tool_count: usize) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    hash = stable_hash64_update(hash, body.model.as_bytes());
    hash = stable_hash64_update(hash, b"\x1f");
    let stream_label: &[u8] = if body.stream.unwrap_or(false) {
        b"stream"
    } else {
        b"nonstream"
    };
    hash = stable_hash64_update(hash, stream_label);
    hash = stable_hash64_update(hash, b"\x1f");
    hash = stable_hash64_update(hash, tool_count.to_string().as_bytes());

    for message in &body.messages {
        hash = stable_hash64_update(hash, b"\x1e");
        hash = stable_hash64_update(hash, message.role.as_bytes());
        hash = stable_hash64_update(hash, b"\x1f");
        match &message.content {
            Value::String(text) => {
                hash = stable_hash64_update(hash, text.as_bytes());
            }
            other => {
                let rendered = serde_json::to_string(other).unwrap_or_default();
                hash = stable_hash64_update(hash, rendered.as_bytes());
            }
        }
    }

    hash
}

fn request_cache_material(body: &ChatRequest) -> String {
    let mut material = String::new();
    material.push_str("model=");
    material.push_str(&body.model);
    material.push('\n');
    // Tool result payloads are expected to change between otherwise identical
    // ClaudeCode runs. They still affect the full prompt hash, but should not
    // split the cache identity before the stable prefix can be reused.
    let cache_messages = body
        .messages
        .iter()
        .enumerate()
        .map(|(message_index, message)| cache_identity_message(message_index, message))
        .collect::<Vec<_>>();
    material.push_str("messages=");
    material.push_str(&serde_json::to_string(&cache_messages).unwrap_or_default());
    material.push('\n');
    material.push_str("tools=");
    material.push_str(&serde_json::to_string(&body.tools).unwrap_or_default());
    material.push('\n');
    material.push_str("tool_choice=");
    material.push_str(&serde_json::to_string(&body.tool_choice).unwrap_or_default());
    material
}

fn cache_identity_message(message_index: usize, message: &Message) -> Message {
    if message.role == "tool" {
        return Message {
            role: message.role.clone(),
            content: Value::String("[tool_result]".to_string()),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        };
    }
    let mut cached = message.clone();
    cached.reasoning_content = None;
    if let Some(calls) = cached.tool_calls.as_mut() {
        for (tool_index, call) in calls.iter_mut().enumerate() {
            call.id = Some(synthetic_tool_id(message_index, tool_index, call));
        }
    }
    cached
}

fn request_cache_prefix_hash(material: &str, prefix_bytes: usize) -> u64 {
    let bytes = material.as_bytes();
    let len = bytes.len().min(prefix_bytes);
    stable_hash64_update(0xcbf29ce484222325u64, &bytes[..len])
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StreamContextRepair {
    pub before_tokens: u64,
    pub after_tokens: u64,
    pub compacted_messages: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamContextPolicy {
    pub compact_at_tokens: u64,
    pub target_tokens: u64,
    pub min_text_tokens: u64,
    pub head_chars: usize,
    pub compact_system_messages: bool,
    pub anchor_latest_user_instruction: bool,
    pub latest_user_anchor_chars: usize,
    pub sanitize_claude_code_resume_pressure: bool,
}

impl StreamContextPolicy {
    pub const fn default() -> Self {
        Self {
            compact_at_tokens: 80_000,
            target_tokens: 60_000,
            min_text_tokens: 8_000,
            head_chars: 8 * 1024,
            compact_system_messages: false,
            anchor_latest_user_instruction: false,
            latest_user_anchor_chars: 0,
            sanitize_claude_code_resume_pressure: false,
        }
    }

    pub const fn claude_code_huge_context() -> Self {
        Self {
            compact_at_tokens: 80_000,
            target_tokens: 12_000,
            min_text_tokens: 2_000,
            head_chars: 2 * 1024,
            compact_system_messages: true,
            anchor_latest_user_instruction: true,
            latest_user_anchor_chars: 2 * 1024,
            sanitize_claude_code_resume_pressure: true,
        }
    }
}

pub fn non_stream_output_policy(
    messages: &[Message],
    requested_max_tokens: Option<u64>,
) -> NonStreamOutputPolicy {
    let prompt_tokens = estimate_tokens(&build_prompt_text(messages));
    non_stream_output_policy_for_prompt_tokens(prompt_tokens, requested_max_tokens)
}

pub fn non_stream_output_policy_for_prompt_tokens(
    prompt_tokens: u64,
    requested_max_tokens: Option<u64>,
) -> NonStreamOutputPolicy {
    NonStreamOutputPolicy {
        prompt_tokens,
        requested_max_tokens,
        effective_max_tokens: requested_max_tokens,
        capped: false,
    }
}

pub fn stream_output_max_tokens(requested_max_tokens: Option<u64>) -> Option<u64> {
    requested_max_tokens
}

pub fn stream_output_policy(
    messages: &[Message],
    requested_max_tokens: Option<u64>,
) -> StreamOutputPolicy {
    let prompt_tokens = estimate_tokens(&build_prompt_text(messages));
    stream_output_policy_for_prompt_tokens(prompt_tokens, requested_max_tokens)
}

pub fn stream_output_policy_for_prompt_tokens(
    prompt_tokens: u64,
    requested_max_tokens: Option<u64>,
) -> StreamOutputPolicy {
    StreamOutputPolicy {
        prompt_tokens,
        requested_max_tokens,
        effective_max_tokens: requested_max_tokens,
        capped: false,
    }
}

pub fn observe_context(messages: &[Message]) -> StreamContextRepair {
    let tokens = estimate_tokens(&build_prompt_text(messages));
    StreamContextRepair {
        before_tokens: tokens,
        after_tokens: tokens,
        compacted_messages: 0,
    }
}

pub fn model_disables_input_compaction(model: &str) -> bool {
    matches!(
        normalize_model(model).as_str(),
        "deepseek-v4-flash"
            | "deepseek-v4-flash-free"
            | "big-pickle"
            | "mimo-v2.5"
            | "mimo-v2.5-free"
            | "hy3"
            | "hy3-free"
            | "north-mini-code"
            | "north-mini-code-free"
            | "nemotron-3-ultra"
            | "nemotron-3-ultra-free"
    )
}

pub fn compact_stream_context(messages: &mut [Message]) -> StreamContextRepair {
    compact_stream_context_with_policy(messages, StreamContextPolicy::default())
}

pub fn compact_claude_code_huge_session_context(
    messages: &mut Vec<Message>,
) -> StreamContextRepair {
    const MIN_MESSAGES_TO_FOLD: usize = 160;
    const RECENT_MESSAGES_TO_KEEP: usize = 48;

    let mut policy = StreamContextPolicy::claude_code_huge_context();
    let before_tokens = estimate_tokens(&build_prompt_text(messages));
    let mid_sized_tool_history_pressure =
        should_compact_mid_sized_claude_code_tool_history(messages, before_tokens);
    if mid_sized_tool_history_pressure {
        policy.compact_at_tokens = 24_000;
    }
    let base_repair = compact_stream_context_with_policy(messages, policy);
    let should_fold_short_history = messages.len() >= MIN_MESSAGES_TO_FOLD
        || base_repair.after_tokens > policy.target_tokens.saturating_mul(3)
        || (mid_sized_tool_history_pressure && messages.len() > RECENT_MESSAGES_TO_KEEP);
    if !should_fold_short_history {
        return base_repair;
    }

    let original_len = messages.len();
    let recent_start = original_len.saturating_sub(RECENT_MESSAGES_TO_KEEP);
    let mut system_messages = Vec::new();
    let mut folded_messages = Vec::new();
    let mut recent_messages = Vec::new();

    for (idx, message) in std::mem::take(messages).into_iter().enumerate() {
        if message.role == "system" {
            system_messages.push(message);
        } else if idx >= recent_start {
            recent_messages.push(message);
        } else {
            folded_messages.push(message);
        }
    }

    if folded_messages.is_empty() {
        *messages = system_messages;
        messages.extend(recent_messages);
        return base_repair;
    }

    let summary = build_claude_code_folded_history_summary(&folded_messages);
    let mut compacted = system_messages;
    compacted.push(Message {
        role: "user".to_string(),
        content: Value::String(summary),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    });
    compacted.extend(recent_messages);
    *messages = compacted;

    StreamContextRepair {
        before_tokens: base_repair.before_tokens,
        after_tokens: estimate_tokens(&build_prompt_text(messages)),
        compacted_messages: base_repair
            .compacted_messages
            .saturating_add(folded_messages.len()),
    }
}

fn should_compact_mid_sized_claude_code_tool_history(
    messages: &[Message],
    before_tokens: u64,
) -> bool {
    const MIN_MESSAGES: usize = 40;
    const MIN_TOTAL_TOKENS: u64 = 24_000;
    const MIN_LARGEST_MESSAGE_TOKENS: u64 = 12_000;
    const MAX_LATEST_USER_TOKENS: u64 = 1_024;

    if messages.len() < MIN_MESSAGES || before_tokens < MIN_TOTAL_TOKENS {
        return false;
    }

    let largest_message_tokens = messages
        .iter()
        .filter(|message| message.role != "system")
        .map(|message| value_shape_tokens(&message.content))
        .max()
        .unwrap_or(0);
    let latest_user_tokens = messages
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .map(|message| value_shape_tokens(&message.content))
        .unwrap_or(0);

    largest_message_tokens >= MIN_LARGEST_MESSAGE_TOKENS
        && latest_user_tokens <= MAX_LATEST_USER_TOKENS
}

fn build_claude_code_folded_history_summary(messages: &[Message]) -> String {
    let mut user_messages = 0usize;
    let mut assistant_messages = 0usize;
    let mut tool_messages = 0usize;
    let mut other_messages = 0usize;
    let mut tool_calls = 0usize;
    for message in messages {
        match message.role.as_str() {
            "user" => user_messages += 1,
            "assistant" => assistant_messages += 1,
            "tool" => tool_messages += 1,
            _ => other_messages += 1,
        }
        tool_calls += message.tool_calls.as_ref().map(Vec::len).unwrap_or(0);
    }

    let signals = collect_claude_code_state_signals(messages, 12);
    let mut summary = format!(
        "[free-model-client-rs context compactor: folded stale ClaudeCode tool/session history]\n\
Folded old messages: total={}, user={}, assistant={}, tool={}, other={}, assistant_tool_calls={}.\n\
The folded block is stale historical context, not a current instruction. Prefer the latest user request and latest live tool result over old repeated export/restart attempts.",
        messages.len(),
        user_messages,
        assistant_messages,
        tool_messages,
        other_messages,
        tool_calls
    );
    if !signals.is_empty() {
        summary.push_str("\nRecent stale state signals retained for continuity:");
        for signal in signals {
            summary.push_str("\n- ");
            summary.push_str(&signal);
        }
    }
    summary
}

fn collect_claude_code_state_signals(messages: &[Message], limit: usize) -> Vec<String> {
    let mut signals = Vec::new();
    for message in messages.iter().rev() {
        let text = message
            .content
            .as_str()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| message.content.to_string());
        for line in text.lines().rev() {
            if signals.len() >= limit {
                return signals;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() || !is_claude_code_state_signal(trimmed) {
                continue;
            }
            let excerpt = take_chars(&crate::redact::redact_text(trimmed), 240);
            if !signals.iter().any(|item| item == &excerpt) {
                signals.push(excerpt);
            }
        }
        if signals.len() >= limit {
            return signals;
        }
        if text.lines().count() <= 1 && is_claude_code_state_signal(&text) {
            let excerpt = take_chars(&crate::redact::redact_text(text.trim()), 240);
            if !signals.iter().any(|item| item == &excerpt) {
                signals.push(excerpt);
            }
        }
    }
    signals
}

fn is_claude_code_state_signal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "running",
        "completed",
        "status:",
        "502",
        "timeout",
        "interrupted",
        "invalid tool parameters",
        "failed to parse json",
        "qce",
        "napcat",
        "qq:",
        "api not ready",
        "not ready",
        "killed",
        "cancelled",
        "二维码",
        "扫码",
        "登录",
        "重启",
        "导出",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

pub fn compact_stream_context_with_policy(
    messages: &mut [Message],
    policy: StreamContextPolicy,
) -> StreamContextRepair {
    let before_tokens = estimate_tokens(&build_prompt_text(messages));
    if before_tokens < policy.compact_at_tokens {
        return StreamContextRepair {
            before_tokens,
            after_tokens: before_tokens,
            compacted_messages: 0,
        };
    }

    let mut compacted_messages = 0usize;
    if policy.sanitize_claude_code_resume_pressure {
        for msg in messages.iter_mut() {
            let Some(text) = msg.content.as_str() else {
                continue;
            };
            let sanitized = sanitize_claude_code_resume_pressure(text);
            if sanitized != text {
                msg.content = Value::String(sanitized);
                compacted_messages += 1;
            }
        }
    }

    let mut over_tokens =
        estimate_tokens(&build_prompt_text(messages)).saturating_sub(policy.target_tokens.max(1));
    let latest_user_idx = messages
        .iter()
        .enumerate()
        .rev()
        .find(|(_, msg)| msg.role == "user")
        .map(|(idx, _)| idx);
    let mut candidates = messages
        .iter()
        .enumerate()
        .filter_map(|(idx, msg)| {
            if msg.role == "system" && !policy.compact_system_messages {
                return None;
            }
            let text = msg.content.as_str()?;
            let tokens = estimate_tokens(text);
            if tokens < policy.min_text_tokens {
                return None;
            }
            let should_anchor_user = policy.anchor_latest_user_instruction
                && msg.role == "user"
                && (Some(idx) == latest_user_idx || latest_anchor_marker_start(text).is_some())
                && !is_claude_code_resume_pressure(text);
            let priority = if should_anchor_user {
                3usize
            } else if msg.role == "system" {
                0usize
            } else if Some(idx) == latest_user_idx {
                2usize
            } else {
                1usize
            };
            Some((priority, tokens, idx))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)));

    for (_priority, tokens, idx) in candidates {
        if over_tokens == 0 {
            break;
        }
        let Some(text) = messages[idx].content.as_str() else {
            continue;
        };
        let keep_tokens = tokens
            .saturating_sub(over_tokens)
            .max(policy.min_text_tokens);
        if keep_tokens >= tokens {
            continue;
        }
        let keep_chars = (keep_tokens as usize).saturating_mul(4);
        let mut compacted = if messages[idx].role == "system" && policy.compact_system_messages {
            compact_text_head(text, keep_chars)
        } else if policy.anchor_latest_user_instruction
            && messages[idx].role == "user"
            && (Some(idx) == latest_user_idx || latest_anchor_marker_start(text).is_some())
            && !is_claude_code_resume_pressure(text)
        {
            compact_text_middle_with_latest_user_anchor(
                text,
                keep_chars,
                policy.head_chars,
                policy.latest_user_anchor_chars,
            )
        } else {
            compact_text_middle(text, keep_chars, policy.head_chars)
        };
        if policy.sanitize_claude_code_resume_pressure {
            compacted = sanitize_claude_code_resume_pressure(&compacted);
        }
        if compacted.len() >= text.len() {
            continue;
        }
        let saved_tokens = tokens.saturating_sub(estimate_tokens(&compacted));
        messages[idx].content = Value::String(compacted);
        compacted_messages += 1;
        over_tokens = over_tokens.saturating_sub(saved_tokens);
    }

    StreamContextRepair {
        before_tokens,
        after_tokens: estimate_tokens(&build_prompt_text(messages)),
        compacted_messages,
    }
}

pub fn append_latest_user_anchor_message(messages: &mut Vec<Message>, max_chars: usize) -> bool {
    let Some(anchor) = select_active_user_anchor(messages, max_chars) else {
        return false;
    };
    if anchor.trim().is_empty() {
        return false;
    }
    let content = if has_exact_reply_instruction(&anchor) {
        format!(
            "[free-model-client-rs context compactor: active latest user request after stale ClaudeCode transcript/session context was omitted]\n[free-model-client-rs context compactor: exact-output guard; answer this active request directly, without git, transcript, or workspace-state inspection]\n{anchor}"
        )
    } else {
        format!(
            "[free-model-client-rs context compactor: active latest user request after stale ClaudeCode transcript/session context was omitted]\n{anchor}"
        )
    };
    if messages
        .last()
        .is_some_and(|message| message.role == "user" && message.content.as_str() == Some(&content))
    {
        return false;
    }
    messages.push(Message {
        role: "user".to_string(),
        content: Value::String(content),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    });
    true
}

pub fn reduce_to_exact_output_anchor_message(
    messages: &mut Vec<Message>,
    max_chars: usize,
) -> bool {
    let Some(anchor) = select_active_user_anchor(messages, max_chars) else {
        return false;
    };
    if !has_exact_reply_instruction(&anchor) {
        return false;
    }

    messages.clear();
    messages.push(Message {
        role: "user".to_string(),
        content: Value::String(format!(
            "[free-model-client-rs context compactor: isolated ClaudeCode huge exact-output request]\nReturn only the requested literal answer.\n{anchor}"
        )),
        tool_calls: None,
        tool_call_id: None,
                reasoning_content: None,
    });
    true
}

pub fn exact_output_literal_from_messages(messages: &[Message]) -> Option<String> {
    for text in messages
        .iter()
        .rev()
        .filter(|message| message.role == "user")
        .filter_map(|message| message.content.as_str())
    {
        let anchor = extract_latest_user_anchor(text, 2 * 1024);
        if let Some(literal) = exact_output_literal_from_text(&anchor) {
            return Some(literal);
        }
        if let Some(literal) = exact_output_literal_from_text(text) {
            return Some(literal);
        }
    }
    None
}

pub fn claude_code_recovery_literal_from_messages(messages: &[Message]) -> Option<String> {
    let prompt = build_prompt_text(messages);
    if !is_claude_code_resume_pressure(&prompt) {
        return None;
    }
    safe_marker_literal_from_text(&prompt)
}

pub fn is_claude_code_recovery_pressure_messages(messages: &[Message]) -> bool {
    is_claude_code_resume_pressure(&build_prompt_text(messages))
}

fn safe_marker_literal_from_text(text: &str) -> Option<String> {
    text.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .rfind(|token| {
            !token.is_empty()
                && token.chars().count() <= 80
                && token.ends_with("_OK")
                && token
                    .chars()
                    .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
        })
        .map(ToOwned::to_owned)
}

fn exact_output_literal_from_text(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    if let Some(literal) = extract_multiline_literal(text) {
        return Some(literal);
    }
    if let Some(literal) = extract_after_ascii_marker(text, &lower, "reply exactly") {
        return Some(literal);
    }
    if let Some(literal) = extract_after_ascii_marker(text, &lower, "return exactly") {
        return Some(literal);
    }
    if let Some(literal) = extract_output_only_literal(text, &lower) {
        return Some(literal);
    }
    if let Some(literal) = extract_after_unicode_marker(text, "只输出") {
        return Some(literal);
    }
    if let Some(literal) = extract_after_unicode_marker(text, "只回复") {
        return Some(literal);
    }
    None
}

fn extract_multiline_literal(text: &str) -> Option<String> {
    const MAX_LITERAL_CHARS: usize = 8 * 1024;
    let markers = [
        "只输出以下",
        "只输出下面",
        "只回复以下",
        "只回复下面",
        "原样输出以下",
        "原样输出下面",
        "output the following",
        "return the following",
        "reply with the following",
    ];
    let lower = text.to_ascii_lowercase();
    let idx = markers
        .iter()
        .filter_map(|marker| {
            if marker.is_ascii() {
                lower.rfind(marker)
            } else {
                text.rfind(marker)
            }
        })
        .max()?;
    let tail = text.get(idx..)?;
    let newline = tail.find('\n')?;
    let literal = tail.get(newline + 1..)?.trim();
    if literal.is_empty()
        || literal.chars().count() > MAX_LITERAL_CHARS
        || literal.chars().any(|ch| ch == '\0')
    {
        return None;
    }
    Some(literal.to_string())
}

fn extract_after_ascii_marker(text: &str, lower: &str, marker: &str) -> Option<String> {
    let idx = lower.rfind(marker)?;
    let raw = text.get(idx + marker.len()..)?;
    normalize_exact_output_literal(raw)
}

fn extract_output_only_literal(text: &str, lower: &str) -> Option<String> {
    let idx = lower.rfind("output ")?;
    let raw = text.get(idx + "output ".len()..)?;
    let raw_lower = lower.get(idx + "output ".len()..)?;
    let end = raw_lower.find(" only")?;
    normalize_exact_output_literal(raw.get(..end)?)
}

fn extract_after_unicode_marker(text: &str, marker: &str) -> Option<String> {
    let idx = text.rfind(marker)?;
    let raw = text.get(idx + marker.len()..)?;
    normalize_exact_output_literal(raw)
}

fn normalize_exact_output_literal(raw: &str) -> Option<String> {
    let first_line = raw.lines().next()?.trim();
    let literal = first_line
        .trim_matches(|ch: char| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    '"' | '\'' | ':' | '：' | '.' | '。' | '!' | '！' | ',' | '，' | ';' | '；'
                )
        })
        .trim();
    if literal.is_empty()
        || literal.chars().count() > 80
        || literal.split_whitespace().count() > 1
        || literal.chars().any(char::is_control)
    {
        return None;
    }
    Some(literal.to_string())
}

fn select_active_user_anchor(messages: &[Message], max_chars: usize) -> Option<String> {
    let mut fallback = None;
    for text in messages
        .iter()
        .rev()
        .filter(|message| message.role == "user")
        .filter_map(|message| message.content.as_str())
    {
        let anchor = extract_latest_user_anchor(text, max_chars);
        if anchor.trim().is_empty() {
            continue;
        }
        if is_claude_code_resume_pressure(&anchor) && !has_explicit_user_anchor(text) {
            continue;
        }
        if has_explicit_user_anchor(text) || has_exact_reply_instruction(&anchor) {
            return Some(anchor);
        }
        if fallback.is_none() && !is_claude_code_resume_pressure(&anchor) {
            fallback = Some(anchor);
        }
    }
    fallback
}

fn compact_text_middle_with_latest_user_anchor(
    text: &str,
    keep_chars: usize,
    head_chars: usize,
    anchor_chars: usize,
) -> String {
    let anchor = extract_latest_user_anchor(text, anchor_chars);
    if anchor.trim().is_empty() {
        return compact_text_middle(text, keep_chars, head_chars);
    }

    let anchor_budget = anchor.chars().count().saturating_add(256);
    let body_keep_chars = keep_chars.saturating_sub(anchor_budget).max(1);
    let compacted = compact_text_middle(text, body_keep_chars, head_chars);
    format!(
        "[free-model-client-rs context compactor: latest user excerpt preserved]\n{anchor}\n[free-model-client-rs context compactor: oversized context follows]\n{compacted}"
    )
}

fn extract_latest_user_anchor(text: &str, max_chars: usize) -> String {
    let max_chars = max_chars.max(1);
    let window = take_tail_chars(text, max_chars.saturating_mul(8));
    let marker_start = latest_anchor_marker_start(&window);
    let anchor = marker_start
        .and_then(|idx| window.get(idx..))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| take_chars(value, max_chars))
        .unwrap_or_else(|| last_non_empty_tail_lines(&window, max_chars));
    anchor.trim().to_string()
}

fn latest_anchor_marker_start(text: &str) -> Option<usize> {
    let ascii_lower = text.to_ascii_lowercase();
    let ascii_markers = [
        "final question:",
        "final request:",
        "final instruction:",
        "latest user request:",
        "my request for codex:",
        "my request:",
        "current request:",
        "current task:",
        "now:",
    ];
    let mut best = ascii_markers
        .iter()
        .filter_map(|marker| ascii_lower.rfind(marker))
        .max();

    let unicode_markers = [
        "最终问题",
        "最后问题",
        "最终要求",
        "最后要求",
        "当前要求",
        "当前任务",
        "现在要求",
        "现在的要求",
        "只输出",
    ];
    for marker in unicode_markers {
        if let Some(idx) = text.rfind(marker) {
            best = Some(best.map_or(idx, |current| current.max(idx)));
        }
    }

    best
}

fn has_explicit_user_anchor(text: &str) -> bool {
    latest_anchor_marker_start(text).is_some() || has_exact_reply_instruction(text)
}

fn has_exact_reply_instruction(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("reply exactly")
        || lower.contains("output ") && (lower.contains(" only") || lower.contains("exactly"))
        || lower.contains("return exactly")
        || text.contains("只输出")
        || text.contains("只回复")
}

fn sanitize_claude_code_resume_pressure(text: &str) -> String {
    if !is_claude_code_resume_pressure(text) {
        return text.to_string();
    }

    let mut kept = Vec::new();
    let mut removed = 0usize;
    for line in text.lines() {
        if has_explicit_user_anchor(line) || !is_claude_code_resume_pressure(line) {
            kept.push(line);
        } else {
            removed += 1;
        }
    }

    if removed == 0 {
        return text.to_string();
    }
    let mut sanitized = kept.join("\n");
    if !sanitized.trim().is_empty() {
        sanitized.push('\n');
    }
    sanitized.push_str(&format!(
        "[free-model-client-rs context compactor: omitted stale ClaudeCode transcript/session recovery lines; removed_lines={removed}]"
    ));
    sanitized
}

fn is_claude_code_resume_pressure(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        ".claude/projects",
        ".jsonl",
        "pick up where we left off",
        "where we left off",
        "read the transcript",
        "read transcript",
        "latest transcript",
        "conversation transcript",
        "summary file",
        "git status",
        "git diff",
        "git log",
        "git log --oneline",
        "recent git",
        "current workspace state",
        "workspace-state",
        "workspace state",
        "understand current state",
        "understand the current state",
        "continue previous conversation",
        "continue the previous conversation",
        "compacted conversation",
        "ready for the next instruction",
        "ready for next instruction",
        "next instruction",
        "project files",
        "session history",
        "full context",
        "reviewed the full context",
        "session is complete",
        "the session is complete",
        "working tree has",
        "summary of what's in the working tree",
        "uncommitted changes",
        "tests pass",
        "tests with no warnings",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn last_non_empty_tail_lines(text: &str, max_chars: usize) -> String {
    let mut kept = Vec::new();
    let mut chars = 0usize;
    for line in text.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let line_chars = trimmed.chars().count();
        if !kept.is_empty() && chars.saturating_add(line_chars) > max_chars {
            break;
        }
        kept.push(trimmed.to_string());
        chars = chars.saturating_add(line_chars).saturating_add(1);
        if chars >= max_chars {
            break;
        }
    }
    kept.reverse();
    let joined = kept.join("\n");
    if joined.chars().count() <= max_chars {
        joined
    } else {
        take_tail_chars(&joined, max_chars)
    }
}

fn compact_text_middle(text: &str, keep_chars: usize, head_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= keep_chars {
        return text.to_string();
    }
    let head_chars = head_chars.min(keep_chars / 2).max(1);
    let tail_chars = keep_chars.saturating_sub(head_chars).max(1);
    let head = take_chars(text, head_chars);
    let tail = take_tail_chars(text, tail_chars);
    format!(
        "{head}\n[free-model-client-rs context compactor: omitted middle of oversized context; original_chars={char_count}; kept_head_chars={head_chars}; kept_tail_chars={tail_chars}]\n{tail}"
    )
}

fn compact_text_head(text: &str, keep_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= keep_chars {
        return text.to_string();
    }
    let keep_chars = keep_chars.max(1);
    let head = take_chars(text, keep_chars);
    format!(
        "{head}\n[free-model-client-rs context compactor: omitted tail of oversized system context; original_chars={char_count}; kept_head_chars={keep_chars}]"
    )
}

fn take_chars(text: &str, count: usize) -> String {
    text.chars().take(count).collect()
}

fn take_tail_chars(text: &str, count: usize) -> String {
    let mut chars = text.chars().rev().take(count).collect::<Vec<_>>();
    chars.reverse();
    chars.into_iter().collect()
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

    if body.messages.iter().all(|msg| {
        msg.content
            .as_str()
            .is_none_or(|text| text.trim().is_empty())
    }) {
        return true;
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
    matches!(
        trimmed.to_ascii_lowercase().as_str(),
        "ping"
            | "health"
            | "healthcheck"
            | "health_check"
            | "/health"
            | "__health__"
            | "__zen_health__"
            | "__fmc_health__"
            | "zen_health"
            | "fmc_health"
    ) || matches!(trimmed, "健康检查" | "健康测试")
}

pub fn is_short_no_tool_channel_test_probe(body: &ChatRequest) -> bool {
    short_no_tool_empty_fallback_text(body).is_some_and(|text| text == "ok")
}

pub fn short_no_tool_empty_fallback_text(body: &ChatRequest) -> Option<&'static str> {
    if has_tools(body)
        || body.tool_choice.is_some()
        || body.max_tokens.is_none_or(|max_tokens| max_tokens > 64)
    {
        return None;
    }

    let user_messages = body
        .messages
        .iter()
        .filter(|msg| msg.role == "user")
        .collect::<Vec<_>>();
    if user_messages.len() != 1
        || body.messages.iter().any(|msg| {
            msg.role == "assistant"
                || (msg.role != "user"
                    && msg
                        .content
                        .as_str()
                        .is_some_and(|text| !text.trim().is_empty()))
        })
    {
        return None;
    }

    let text = user_messages[0].content.as_str()?;
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "hi" | "hello" | "test" | "echo hi" | "echo hello" | "echo test"
    ) || matches!(trimmed, "测试" | "測試")
    {
        return Some("ok");
    }

    if lower.contains("reply pong only")
        || lower.contains("answer pong only")
        || lower.contains("pong only")
        || lower.contains("respond pong")
    {
        return Some("PONG");
    }

    if lower.contains("strict smoke")
        || lower.contains("chain smoke")
        || lower.contains("reply pass")
        || lower.contains("answer pass")
        || lower.contains("pass only")
        || lower.contains("respond pass")
    {
        return Some("PASS");
    }

    if lower.contains("reply ok")
        || lower.contains("answer ok")
        || lower.contains("exactly ok")
        || lower.contains("ok only")
        || lower.contains("respond ok")
    {
        return Some("ok");
    }

    None
}

pub fn is_reasoning_only_error(msg: &str) -> bool {
    msg.contains("reasoning_content without final content")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn message(role: &str, content: &str) -> Message {
        Message {
            role: role.to_string(),
            content: Value::String(content.to_string()),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    fn request(content: &str, stream: bool, max_tokens: Option<u64>) -> ChatRequest {
        ChatRequest {
            model: "deepseek-v4-flash".to_string(),
            messages: vec![message("user", content)],
            stream: Some(stream),
            max_tokens,
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
        }
    }

    fn tool(name: &str) -> OpenAITool {
        OpenAITool {
            tool_type: "function".to_string(),
            function: OpenAIToolFunction {
                name: name.to_string(),
                description: Some("SECRET_TOOL_DESCRIPTION".to_string()),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "SECRET_PATH_DESCRIPTION"}
                    }
                })),
            },
        }
    }

    #[test]
    fn cache_prefix_ignores_dynamic_tool_result_payloads() {
        let tools = (0..39)
            .map(|idx| OpenAITool {
                tool_type: "function".to_string(),
                function: OpenAIToolFunction {
                    name: format!("tool_{idx}"),
                    description: Some("stable schema ".repeat(120)),
                    parameters: Some(json!({
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "stable path schema ".repeat(80)
                            }
                        }
                    })),
                },
            })
            .collect::<Vec<_>>();

        let mut first = request("continue", true, Some(1024));
        first.tools = Some(tools.clone());
        first.messages.insert(
            0,
            Message {
                role: "tool".to_string(),
                content: Value::String(format!("tool output A {}", "x".repeat(12_000))),
                tool_calls: None,
                tool_call_id: Some("toolu_same".to_string()),
                reasoning_content: None,
            },
        );

        let mut second = first.clone();
        second.messages[0].content = Value::String(format!("tool output B {}", "y".repeat(12_000)));

        let first_shape = request_shape(&first);
        let second_shape = request_shape(&second);

        assert_ne!(first_shape.prompt_hash, second_shape.prompt_hash);
        assert_eq!(first_shape.prefix_4k_hash, second_shape.prefix_4k_hash);
        assert_eq!(first_shape.prefix_32k_hash, second_shape.prefix_32k_hash);
    }

    #[test]
    fn cache_prefix_ignores_dynamic_claude_code_tool_ids() {
        let mut first = request("continue", true, Some(1024));
        first.messages.insert(
            0,
            Message {
                role: "assistant".to_string(),
                content: Value::Null,
                tool_calls: Some(vec![ToolCall {
                    id: Some("toolu_dynamic_a".to_string()),
                    call_type: "function".to_string(),
                    function: ToolFunction {
                        name: "Read".to_string(),
                        arguments: json!({"file_path": "docs/cache-95plus-architecture.md"})
                            .to_string(),
                    },
                    index: Some(0),
                }]),
                tool_call_id: None,
                reasoning_content: Some("dynamic hidden reasoning A".to_string()),
            },
        );
        first.messages.insert(
            1,
            Message {
                role: "tool".to_string(),
                content: Value::String(format!("tool output A {}", "x".repeat(12_000))),
                tool_calls: None,
                tool_call_id: Some("toolu_dynamic_a".to_string()),
                reasoning_content: None,
            },
        );

        let mut second = first.clone();
        let calls = second.messages[0].tool_calls.as_mut().unwrap();
        calls[0].id = Some("toolu_dynamic_b".to_string());
        second.messages[0].reasoning_content = Some("dynamic hidden reasoning B".to_string());
        second.messages[1].tool_call_id = Some("toolu_dynamic_b".to_string());
        second.messages[1].content = Value::String(format!("tool output B {}", "y".repeat(12_000)));

        let first_shape = request_shape(&first);
        let second_shape = request_shape(&second);

        assert_ne!(first_shape.prompt_hash, second_shape.prompt_hash);
        assert_eq!(first_shape.prefix_4k_hash, second_shape.prefix_4k_hash);
        assert_eq!(first_shape.prefix_32k_hash, second_shape.prefix_32k_hash);
    }

    #[test]
    fn canonicalize_openai_tool_history_stabilizes_existing_tool_ids() {
        let mut messages = vec![
            Message {
                role: "assistant".to_string(),
                content: Value::Null,
                tool_calls: Some(vec![ToolCall {
                    id: Some("toolu_runtime_1".to_string()),
                    call_type: "function".to_string(),
                    function: ToolFunction {
                        name: "Read".to_string(),
                        arguments: json!({"file_path": "docs/cache-95plus-architecture.md"})
                            .to_string(),
                    },
                    index: Some(0),
                }]),
                tool_call_id: None,
                reasoning_content: None,
            },
            Message {
                role: "tool".to_string(),
                content: Value::String("tool output".to_string()),
                tool_calls: None,
                tool_call_id: Some("toolu_runtime_1".to_string()),
                reasoning_content: None,
            },
        ];

        let repair =
            canonicalize_openai_tool_history_with_policy(&mut messages, ToolHistoryPolicy::Compat);
        let stable_id = messages[0].tool_calls.as_ref().unwrap()[0]
            .id
            .as_ref()
            .unwrap()
            .clone();

        assert_eq!(repair.stabilized_tool_call_ids, 2);
        assert_ne!(stable_id, "toolu_runtime_1");
        assert_eq!(
            messages[1].tool_call_id.as_deref(),
            Some(stable_id.as_str())
        );
    }

    #[test]
    fn request_shape_counts_parts_without_exposing_raw_text() {
        let mut body = request("SECRET_USER_TEXT", true, Some(1024));
        body.messages
            .insert(0, message("system", "SECRET_SYSTEM_TEXT"));
        body.tools = Some(vec![tool("Read")]);

        let shape = request_shape(&body);
        let rendered = format!("{shape:?}");

        assert_eq!(shape.message_count, 2);
        assert_eq!(shape.tool_count, 1);
        assert_eq!(shape.tool_name_classes, vec!["file"]);
        assert!(shape.system_tokens > 0);
        assert!(shape.messages_tokens > 0);
        assert!(shape.tools_tokens > 0);
        assert!(shape.largest_message_tokens > 0);
        assert!(shape.last_user_tokens > 0);
        assert!(shape.estimated_total_tokens >= shape.system_tokens + shape.messages_tokens);
        assert_ne!(shape.prompt_hash, 0);
        assert!(!rendered.contains("SECRET_SYSTEM_TEXT"));
        assert!(!rendered.contains("SECRET_USER_TEXT"));
        assert!(!rendered.contains("SECRET_TOOL_DESCRIPTION"));
    }

    #[test]
    fn request_shape_classifies_web_tools_without_exposing_unknown_tool_names() {
        let mut body = request("use the web tool", true, Some(1024));
        body.tools = Some(vec![
            tool("web_fetch"),
            tool("web_search"),
            tool("Task"),
            tool("SECRET_CUSTOM_TOOL"),
        ]);

        let shape = request_shape(&body);
        let rendered = format!("{shape:?}");

        assert_eq!(
            shape.tool_name_classes,
            vec!["other", "task", "web_fetch", "web_search"]
        );
        assert!(!rendered.contains("SECRET_CUSTOM_TOOL"));
    }

    #[test]
    fn echo_hi_stays_channel_test_probe() {
        let body = request("echo hi", false, Some(64));

        assert!(is_short_no_tool_channel_test_probe(&body));
        assert_eq!(short_no_tool_empty_fallback_text(&body), Some("ok"));
        assert_eq!(
            classify_short_non_stream_request(&body, false),
            ShortNonStreamRequestKind::ChannelTest
        );
    }

    #[test]
    fn explicit_smoke_pass_gets_safe_empty_fallback() {
        let body = request("strict smoke: reply PASS only", false, Some(16));

        assert!(!is_short_no_tool_channel_test_probe(&body));
        assert_eq!(short_no_tool_empty_fallback_text(&body), Some("PASS"));
        assert_eq!(
            classify_short_non_stream_request(&body, true),
            ShortNonStreamRequestKind::InternalClaudeCodeProbe
        );
    }

    #[test]
    fn explicit_smoke_pong_gets_safe_empty_fallback() {
        let body = request("reply PONG only", false, Some(16));

        assert!(!is_short_no_tool_channel_test_probe(&body));
        assert_eq!(short_no_tool_empty_fallback_text(&body), Some("PONG"));
        assert_eq!(
            classify_short_non_stream_request(&body, true),
            ShortNonStreamRequestKind::InternalClaudeCodeProbe
        );
    }

    #[test]
    fn explicit_smoke_exact_ok_gets_safe_empty_fallback() {
        let body = request("Reply with exactly OK.", false, Some(16));

        assert!(is_short_no_tool_channel_test_probe(&body));
        assert_eq!(short_no_tool_empty_fallback_text(&body), Some("ok"));
        assert_eq!(
            classify_short_non_stream_request(&body, false),
            ShortNonStreamRequestKind::ChannelTest
        );
    }

    #[test]
    fn explicit_smoke_pong_does_not_fallback_when_too_large() {
        let body = request("reply PONG only", false, Some(256));

        assert!(!is_short_no_tool_channel_test_probe(&body));
        assert_eq!(short_no_tool_empty_fallback_text(&body), None);
        assert_eq!(
            classify_short_non_stream_request(&body, true),
            ShortNonStreamRequestKind::InternalClaudeCodeProbe
        );
    }

    #[test]
    fn ordinary_short_user_request_is_not_channel_test() {
        let body = request("write a title", false, Some(256));

        assert!(!is_short_no_tool_channel_test_probe(&body));
        assert_eq!(short_no_tool_empty_fallback_text(&body), None);
        assert_eq!(
            classify_short_non_stream_request(&body, false),
            ShortNonStreamRequestKind::UserShortRequest
        );
    }

    #[test]
    fn claude_code_quality_models_disable_input_compaction() {
        for model in [
            "deepseek-v4-flash",
            "mimo-v2.5",
            "mimo-v2.5-free",
            "hy3",
            "hy3-free",
            "north-mini-code",
            "nemotron-3-ultra-free",
        ] {
            assert!(model_disables_input_compaction(model), "{model}");
        }
        assert!(!model_disables_input_compaction("deepseek-v4-flash-lite"));
        assert!(model_disables_input_compaction("big-pickle"));
    }

    #[test]
    fn claude_code_tiny_nonstream_non_probe_is_observed_but_not_channel_test() {
        let body = request("session title", false, Some(64));

        assert!(!is_short_no_tool_channel_test_probe(&body));
        assert_eq!(short_no_tool_empty_fallback_text(&body), None);
        assert_eq!(
            classify_short_non_stream_request(&body, true),
            ShortNonStreamRequestKind::InternalClaudeCodeProbe
        );
    }

    #[test]
    fn short_request_with_tools_is_not_short_nonstream() {
        let mut body = request("echo hi", false, Some(64));
        body.tools = Some(vec![tool("Read")]);

        assert!(!is_short_no_tool_channel_test_probe(&body));
        assert_eq!(
            classify_short_non_stream_request(&body, true),
            ShortNonStreamRequestKind::NotShortNonStream
        );
    }
}
