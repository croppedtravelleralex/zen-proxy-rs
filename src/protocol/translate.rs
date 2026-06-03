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

pub fn disable_thinking_for_assistant_history(_body: &mut Value, _messages: &[Message]) {
    // V4.6 keeps model reasoning available for normal multi-turn context.
}

pub fn disable_thinking_by_default(_body: &mut Value) {
    // V4.6 no longer disables thinking for ordinary requests by default.
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

pub fn stabilize_short_user_prompt(_body: &mut Value) {
    // Preserve terse user intent such as "1", "继续", and "执行".
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NonStreamOutputPolicy {
    pub prompt_tokens: u64,
    pub requested_max_tokens: Option<u64>,
    pub effective_max_tokens: u64,
    pub capped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamOutputPolicy {
    pub prompt_tokens: u64,
    pub requested_max_tokens: Option<u64>,
    pub effective_max_tokens: u64,
    pub capped: bool,
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
    let requested = requested_max_tokens.unwrap_or(2_048).max(32);
    let cap = match prompt_tokens {
        100_000.. => 1_024,
        50_000.. => 2_048,
        _ => 4_096,
    };
    let effective_max_tokens = requested.min(cap).max(32);

    NonStreamOutputPolicy {
        prompt_tokens,
        requested_max_tokens,
        effective_max_tokens,
        capped: requested != effective_max_tokens,
    }
}

pub fn stream_output_max_tokens(requested_max_tokens: Option<u64>) -> u64 {
    requested_max_tokens.unwrap_or(1_024).max(32)
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
    let requested = requested_max_tokens.unwrap_or(1_024).max(32);
    let cap = match prompt_tokens {
        200_000.. => 512,
        100_000.. => 768,
        50_000.. => 1_024,
        _ => requested,
    };
    let effective_max_tokens = requested.min(cap).max(32);

    StreamOutputPolicy {
        prompt_tokens,
        requested_max_tokens,
        effective_max_tokens,
        capped: requested != effective_max_tokens,
    }
}

pub fn compact_stream_context(messages: &mut [Message]) -> StreamContextRepair {
    compact_stream_context_with_policy(messages, StreamContextPolicy::default())
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
    if body.stream != Some(true)
        || has_tools(body)
        || body.tool_choice.is_some()
        || body.max_tokens.is_none_or(|max_tokens| max_tokens > 64)
    {
        return false;
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
        return false;
    }

    let Some(text) = user_messages[0].content.as_str() else {
        return false;
    };
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "hi" | "hello" | "test" | "echo hi" | "echo hello" | "echo test"
    ) || matches!(trimmed, "测试" | "測試")
}

pub fn is_reasoning_only_error(msg: &str) -> bool {
    msg.contains("reasoning_content without final content")
}
