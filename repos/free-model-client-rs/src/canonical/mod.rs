use std::collections::{HashMap, HashSet};
use std::sync::{OnceLock, RwLock};

use serde_json::{json, Map, Value};

use crate::ccp::{apply_prompt_cache_key, CcpFlags, IcpIdentity, UskContext};
use crate::protocol::types::{ChatRequest, Message, OpenAITool};

static TOOLS_EPOCH: OnceLock<RwLock<HashMap<String, Value>>> = OnceLock::new();
const TOOL_REASONING_GLOBAL_SCOPE: &str = "__fmc_tool_call_reasoning_global_v1";

fn tools_epoch_store() -> &'static RwLock<HashMap<String, Value>> {
    TOOLS_EPOCH.get_or_init(|| RwLock::new(HashMap::new()))
}

pub fn tools_epoch_key(model: &str, session_scope: &str) -> String {
    format!("{model}:{session_scope}")
}

pub fn freeze_tools_epoch(model: &str, session_scope: &str, tools: &[OpenAITool]) -> Value {
    let key = tools_epoch_key(model, session_scope);
    let canonical = canonical_tools_value(tools);
    if let Ok(mut guard) = tools_epoch_store().write() {
        guard.entry(key).or_insert_with(|| canonical.clone());
        return guard
            .get(&tools_epoch_key(model, session_scope))
            .cloned()
            .unwrap_or(canonical);
    }
    canonical
}

pub fn apply_tools_epoch(model: &str, session_scope: &str, tools: &[OpenAITool]) -> Value {
    if tools.is_empty() {
        return Value::Null;
    }
    let key = tools_epoch_key(model, session_scope);
    if let Ok(guard) = tools_epoch_store().read() {
        if let Some(frozen) = guard.get(&key) {
            if tools_semantically_compatible(tools, frozen) || trf_transient_tools_only_drift(tools, frozen) {
                return frozen.clone();
            }
        }
    }
    let canonical = canonical_tools_value(tools);
    if let Ok(mut guard) = tools_epoch_store().write() {
        guard.insert(key, canonical.clone());
    }
    canonical
}

fn tools_semantically_compatible(current: &[OpenAITool], frozen: &Value) -> bool {
    canonical_tools_value(current) == *frozen
}

fn is_trf_transient_tool_name(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "toolsearch" | "websearch" | "webfetch"
    )
}

fn trf_transient_tools_only_drift(current: &[OpenAITool], frozen: &Value) -> bool {
    let frozen_tools = tools_from_canonical_value(frozen);
    if frozen_tools.is_empty() {
        return false;
    }
    let frozen_names: HashSet<String> = frozen_tools
        .iter()
        .map(|tool| tool.function.name.to_ascii_lowercase())
        .collect();
    let current_names: HashSet<String> = current
        .iter()
        .map(|tool| tool.function.name.to_ascii_lowercase())
        .collect();
    if frozen_names == current_names {
        return false;
    }
    if !frozen_names.is_subset(&current_names) {
        return false;
    }
    current_names
        .difference(&frozen_names)
        .all(|name| is_trf_transient_tool_name(name))
}

fn tools_from_canonical_value(value: &Value) -> Vec<OpenAITool> {
    serde_json::from_value(value.clone()).unwrap_or_default()
}

pub fn canonical_tools_value(tools: &[OpenAITool]) -> Value {
    let mut items = Vec::with_capacity(tools.len());
    for tool in tools {
        let mut function = Map::new();
        function.insert(
            "name".to_string(),
            Value::String(tool.function.name.clone()),
        );
        if let Some(description) = &tool.function.description {
            function.insert(
                "description".to_string(),
                Value::String(description.clone()),
            );
        }
        if let Some(parameters) = &tool.function.parameters {
            function.insert(
                "parameters".to_string(),
                sort_json_value(parameters.clone()),
            );
        }
        let mut item = Map::new();
        item.insert("type".to_string(), Value::String(tool.tool_type.clone()));
        item.insert("function".to_string(), Value::Object(function));
        items.push(Value::Object(item));
    }
    Value::Array(items)
}

pub fn build_upstream_messages_json(messages: &[Message]) -> Value {
    Value::Array(
        messages
            .iter()
            .map(message_to_upstream_json)
            .collect::<Vec<_>>(),
    )
}

pub fn message_to_upstream_json(message: &Message) -> Value {
    let mut object = Map::new();
    object.insert("role".to_string(), Value::String(message.role.clone()));
    object.insert("content".to_string(), message.content.clone());
    if let Some(tool_calls) = &message.tool_calls {
        if let Ok(value) = serde_json::to_value(tool_calls) {
            object.insert("tool_calls".to_string(), value);
        }
    }
    if let Some(tool_call_id) = &message.tool_call_id {
        object.insert(
            "tool_call_id".to_string(),
            Value::String(tool_call_id.clone()),
        );
    }
    if let Some(reasoning) = &message.reasoning_content {
        if !reasoning.is_empty() {
            object.insert(
                "reasoning_content".to_string(),
                Value::String(reasoning.clone()),
            );
        }
    }
    Value::Object(object)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningEnrichMode {
    /// Cache-Body path: never inject stored reasoning into upstream messages.
    CacheBody,
    /// Retry path: enrich only the last assistant message if missing reasoning.
    CurrentTurnOnly,
    /// Legacy: all historical assistant messages (harms cache; tests only).
    AllHistorical,
}

pub fn message_to_cache_upstream_json(message: &Message) -> Value {
    let mut object = Map::new();
    object.insert("role".to_string(), Value::String(message.role.clone()));
    object.insert("content".to_string(), message.content.clone());
    if let Some(tool_calls) = &message.tool_calls {
        if let Ok(value) = serde_json::to_value(tool_calls) {
            object.insert("tool_calls".to_string(), value);
        }
    }
    if let Some(tool_call_id) = &message.tool_call_id {
        object.insert(
            "tool_call_id".to_string(),
            Value::String(tool_call_id.clone()),
        );
    }
    Value::Object(object)
}

pub fn build_cache_upstream_messages_json(messages: &[Message]) -> Value {
    Value::Array(
        messages
            .iter()
            .map(message_to_cache_upstream_json)
            .collect::<Vec<_>>(),
    )
}

pub fn enrich_messages_with_reasoning(messages: &mut [Message], session_scope: &str) -> usize {
    enrich_messages_with_reasoning_mode(messages, session_scope, ReasoningEnrichMode::AllHistorical)
}

pub fn enrich_messages_with_reasoning_mode(
    messages: &mut [Message],
    session_scope: &str,
    mode: ReasoningEnrichMode,
) -> usize {
    match mode {
        ReasoningEnrichMode::CacheBody => 0,
        ReasoningEnrichMode::CurrentTurnOnly => {
            enrich_last_assistant_reasoning(messages, session_scope)
        }
        ReasoningEnrichMode::AllHistorical => {
            enrich_all_assistant_reasoning(messages, session_scope)
        }
    }
}

fn enrich_all_assistant_reasoning(messages: &mut [Message], session_scope: &str) -> usize {
    let mut enriched = 0usize;
    for (index, message) in messages.iter_mut().enumerate() {
        if message.role != "assistant" {
            continue;
        }
        if message
            .reasoning_content
            .as_ref()
            .is_some_and(|text| !text.trim().is_empty())
        {
            continue;
        }
        let key = crate::session::reasoning_store::assistant_reasoning_key(session_scope, index);
        if let Some(reasoning) = crate::session::reasoning_store::get_reasoning(&key) {
            message.reasoning_content = Some(reasoning);
            enriched += 1;
        }
    }
    enriched
}

fn enrich_last_assistant_reasoning(messages: &mut [Message], session_scope: &str) -> usize {
    let Some((index, message)) = messages
        .iter_mut()
        .enumerate()
        .rev()
        .find(|(_, msg)| msg.role == "assistant")
    else {
        return 0;
    };
    if message
        .reasoning_content
        .as_ref()
        .is_some_and(|text| !text.trim().is_empty())
    {
        return 0;
    }
    let key = crate::session::reasoning_store::assistant_reasoning_key(session_scope, index);
    if let Some(reasoning) = crate::session::reasoning_store::get_reasoning(&key) {
        message.reasoning_content = Some(reasoning);
        return 1;
    }
    0
}

pub fn record_collected_reasoning(
    session_scope: &str,
    assistant_message_index: usize,
    reasoning: &str,
) {
    let key = crate::session::reasoning_store::assistant_reasoning_key(
        session_scope,
        assistant_message_index,
    );
    crate::session::reasoning_store::put_reasoning(&key, reasoning.to_string());
}

pub fn record_tool_call_reasoning(
    session_scope: &str,
    tool_name: &str,
    tool_arguments: &str,
    reasoning: &str,
) {
    if reasoning.trim().is_empty() {
        return;
    }
    let stable_reasoning = stable_tool_call_reasoning_replay(tool_name);
    let Some(key) = tool_call_reasoning_key(session_scope, tool_name, tool_arguments) else {
        return;
    };
    crate::session::reasoning_store::put_reasoning(&key, stable_reasoning.clone());
    if let Some(global_key) =
        tool_call_reasoning_key(TOOL_REASONING_GLOBAL_SCOPE, tool_name, tool_arguments)
    {
        crate::session::reasoning_store::put_reasoning(&global_key, stable_reasoning);
    }
}

pub fn enrich_messages_with_tool_call_reasoning(
    messages: &mut [Message],
    session_scope: &str,
) -> usize {
    if session_scope.trim().is_empty() {
        return 0;
    }
    let mut enriched = 0usize;
    for message in messages {
        if message.role != "assistant"
            || message
                .reasoning_content
                .as_ref()
                .is_some_and(|text| !text.trim().is_empty())
        {
            continue;
        }
        let Some(tool_calls) = &message.tool_calls else {
            continue;
        };
        for call in tool_calls {
            let Some(key) = tool_call_reasoning_key(
                session_scope,
                &call.function.name,
                &call.function.arguments,
            ) else {
                continue;
            };
            let reasoning = crate::session::reasoning_store::get_reasoning(&key).or_else(|| {
                tool_call_reasoning_key(
                    TOOL_REASONING_GLOBAL_SCOPE,
                    &call.function.name,
                    &call.function.arguments,
                )
                .and_then(|global_key| crate::session::reasoning_store::get_reasoning(&global_key))
            });
            if let Some(reasoning) = reasoning {
                message.reasoning_content = Some(reasoning);
                enriched += 1;
                break;
            }
        }
    }
    enriched
}

fn tool_call_reasoning_key(
    session_scope: &str,
    tool_name: &str,
    tool_arguments: &str,
) -> Option<String> {
    let scope = session_scope.trim();
    let name = tool_name.trim().to_ascii_lowercase();
    if scope.is_empty() || name.is_empty() {
        return None;
    }
    Some(format!(
        "{scope}:tool_call_reasoning:{name}:{}",
        canonical_tool_arguments(tool_arguments)
    ))
}

fn canonical_tool_arguments(arguments: &str) -> String {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return "{}".to_string();
    }
    serde_json::from_str::<Value>(trimmed)
        .ok()
        .and_then(|value| serde_json::to_string(&sort_json_value(value)).ok())
        .unwrap_or_else(|| trimmed.to_string())
}

fn stable_tool_call_reasoning_replay(tool_name: &str) -> String {
    let name = tool_name.trim().to_ascii_lowercase();
    if name.is_empty() {
        "Tool call reasoning replayed.".to_string()
    } else {
        format!("Tool call reasoning replayed for {name}.")
    }
}

pub fn prefix_drift_bytes(previous_hash: u64, current_hash: u64) -> bool {
    previous_hash != 0 && current_hash != previous_hash
}

pub struct IcpUpstreamPackage {
    pub messages: Vec<Message>,
    pub body: Value,
    pub identity: IcpIdentity,
}

pub fn prepare_upstream_request(
    request: &ChatRequest,
    session_scope: &str,
    upstream_model: &str,
) -> (Vec<Message>, Value) {
    let package = prepare_icp_upstream_request(
        request,
        session_scope,
        upstream_model,
        &UskContext {
            api_key_id: session_scope,
            public_model: &request.model,
            upstream_model,
            source_client: "unknown",
        },
        &CcpFlags::from_env(),
    );
    (package.messages, package.body)
}

pub fn prepare_icp_upstream_request(
    request: &ChatRequest,
    session_scope: &str,
    upstream_model: &str,
    usk_ctx: &UskContext<'_>,
    flags: &CcpFlags,
) -> IcpUpstreamPackage {
    let identity = crate::ccp::compute_icp_identity(request, usk_ctx);
    let icp_scope = if flags.icp_enabled {
        identity.icp_scope.clone()
    } else {
        session_scope.to_string()
    };
    let mut messages = request.messages.clone();
    if flags.reasoning_sidecar {
        enrich_messages_with_reasoning_mode(
            &mut messages,
            session_scope,
            ReasoningEnrichMode::CacheBody,
        );
    } else {
        enrich_messages_with_reasoning(&mut messages, session_scope);
    }
    let tools_value = request
        .tools
        .as_ref()
        .map(|tools| apply_tools_epoch(&request.model, &icp_scope, tools))
        .unwrap_or(Value::Null);
    let messages_json = if flags.icp_enabled {
        build_cache_upstream_messages_json(&messages)
    } else {
        build_upstream_messages_json(&messages)
    };
    let mut body = json!({
        "model": upstream_model,
        "messages": messages_json,
        "stream": request.stream.unwrap_or(false),
        "temperature": request.temperature,
        "tools": if tools_value.is_null() { Value::Null } else { tools_value },
        "tool_choice": request.tool_choice,
    });
    if let Some(max_tokens) = request.max_tokens {
        body["max_tokens"] = json!(max_tokens);
    }
    apply_prompt_cache_key(&mut body, &identity, flags);
    apply_anthropic_cache_breakpoints(&mut body, request, flags);
    IcpUpstreamPackage {
        messages,
        body,
        identity,
    }
}

fn apply_anthropic_cache_breakpoints(body: &mut Value, request: &ChatRequest, flags: &CcpFlags) {
    if !flags.anthropic_breakpoints || !model_supports_anthropic_breakpoints(&request.model) {
        return;
    }

    let mut remaining = 4usize;
    if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
        if add_cache_control_to_last_object(tools) {
            remaining -= 1;
        }
    }
    if remaining == 0 {
        return;
    }

    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    if add_cache_control_to_last_role(messages, "system") {
        remaining -= 1;
    }
    if remaining == 0 {
        return;
    }

    let user_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| {
            message
                .get("role")
                .and_then(Value::as_str)
                .is_some_and(|role| role == "user")
        })
        .map(|(index, _)| index)
        .collect();
    let last_user_idx = user_indices.last().copied();

    if user_indices.len() > 1 && remaining > 0 {
        if let Some(last_user_idx) = last_user_idx {
            if let Some(anchor_idx) = find_last_conversation_anchor_before(messages, last_user_idx) {
                if let Some(object) = messages[anchor_idx].as_object_mut() {
                    if add_cache_control_to_message(object) {
                        remaining -= 1;
                    }
                }
            }
        }
    }

    if remaining > 0 {
        add_cache_control_to_last_role(messages, "user");
    }
}

fn model_supports_anthropic_breakpoints(model: &str) -> bool {
    let normalized: String = model
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect();
    matches!(normalized.as_str(), "bigpickle" | "mimov25" | "mimov25free")
}

/// Anthropic/opencode upstream allows up to four ephemeral cache_control markers.
const DEEPSEEK_MAX_CACHE_BREAKPOINTS: usize = 4;

pub fn apply_deepseek_stable_cache_breakpoints(body: &mut Value, request: &ChatRequest) -> usize {
    if !model_is_deepseek_flash(&request.model) {
        return 0;
    }
    let mut applied = 0usize;
    let mut remaining = DEEPSEEK_MAX_CACHE_BREAKPOINTS;
    if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
        if remaining > 0 && add_cache_control_to_last_object(tools) {
            applied += 1;
            remaining -= 1;
        }
    }
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return applied;
    };
    if remaining > 0 && add_cache_control_to_last_role(messages, "system") {
        applied += 1;
        remaining -= 1;
    }
    if remaining > 0 {
        applied += apply_deepseek_user_cache_breakpoints_on_messages(messages, remaining);
    }
    applied
}

/// Minimum user payload before splitting stable cache prefix from mutable tail.
const DEEPSEEK_USER_CACHE_CHUNK_MIN_BYTES: usize = 65_536;
/// Target stable prefix (~32k tokens at ~4 bytes/token).
const DEEPSEEK_USER_CACHE_STABLE_TARGET_BYTES: usize = 131_072;

fn split_user_text_stable_tail(text: &str) -> (String, Option<String>) {
    if let Some(split_at) = find_rust_codeblock_tail_split(text) {
        let stable = text[..split_at].to_string();
        let tail = text[split_at..].to_string();
        // Prefer structural split for any tier (incl. 10k) when bulk lives in ```rust fence.
        if !tail.is_empty() && stable.len() >= 1024 {
            return (stable, Some(tail));
        }
    }
    if text.len() < DEEPSEEK_USER_CACHE_CHUNK_MIN_BYTES {
        return (text.to_string(), None);
    }
    let split_idx = find_utf8_byte_split_index(text, DEEPSEEK_USER_CACHE_STABLE_TARGET_BYTES);
    if split_idx < text.len() {
        let stable = text[..split_idx].to_string();
        let tail = text[split_idx..].to_string();
        if !tail.is_empty() {
            return (stable, Some(tail));
        }
    }
    (text.to_string(), None)
}

fn find_rust_codeblock_tail_split(text: &str) -> Option<usize> {
    let start = text.find("```rust")?;
    let after_open = start + "```rust".len();
    let rest = text.get(after_open..)?;
    let close_rel = rest.find("\n```")?;
    let mut idx = after_open + close_rel + "\n```".len();
    while idx < text.len() {
        match text.as_bytes().get(idx) {
            Some(b'\n' | b'\r') => idx += 1,
            _ => break,
        }
    }
    if idx > start && idx < text.len() {
        Some(idx)
    } else {
        None
    }
}

fn find_utf8_byte_split_index(text: &str, target_bytes: usize) -> usize {
    if text.len() <= target_bytes {
        return text.len();
    }
    let mut idx = target_bytes.min(text.len());
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn add_cache_control_to_last_user_with_intelligent_chunking(messages: &mut [Value]) -> bool {
    apply_deepseek_user_cache_breakpoints_on_messages(messages, DEEPSEEK_MAX_CACHE_BREAKPOINTS) > 0
}

/// Short follow-up user turns in the same session (daily dev probes).
const DEEPSEEK_SHORT_FOLLOWUP_MAX_BYTES: usize = 8192;

/// Multi-turn: short follow-ups cache latest user; long tails anchor assistant before user;
/// single-turn bulk uses primary user chunking.
fn apply_deepseek_user_cache_breakpoints_on_messages(
    messages: &mut [Value],
    mut remaining: usize,
) -> usize {
    let user_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| {
            message
                .get("role")
                .and_then(Value::as_str)
                .is_some_and(|role| role == "user")
        })
        .map(|(index, _)| index)
        .collect();
    if user_indices.is_empty() || remaining == 0 {
        return 0;
    }
    let primary_idx = user_indices
        .iter()
        .copied()
        .max_by_key(|index| user_message_byte_len(&messages[*index]))
        .unwrap_or(user_indices[0]);
    let last_user_idx = user_indices[user_indices.len() - 1];
    let last_user_bytes = user_message_byte_len(&messages[last_user_idx]);
    let multi_turn = user_indices.len() > 1;
    let first_user_idx = user_indices[0];
    let primary_has_bulk_fixture = user_message_has_bulk_fixture(&messages[primary_idx]);
    let short_followup = multi_turn
        && last_user_bytes <= DEEPSEEK_SHORT_FOLLOWUP_MAX_BYTES
        && !primary_has_bulk_fixture
        && !user_message_has_bulk_fixture(&messages[last_user_idx]);
    let mut applied = 0usize;

    // Keep the opening user turn pinned so turn-2+ requests can still read turn-1 cache.
    if multi_turn && remaining > 0 && first_user_idx != last_user_idx {
        if let Some(object) = messages[first_user_idx].as_object_mut() {
            if add_cache_control_to_user_message_with_chunking(object) {
                applied += 1;
                remaining -= 1;
            }
        }
    }

    if multi_turn && short_followup && remaining > 0 {
        if let Some(object) = messages[last_user_idx].as_object_mut() {
            if add_cache_control_to_message(object) {
                applied += 1;
                remaining -= 1;
            }
        }
    } else if multi_turn && remaining > 0 {
        if let Some(anchor_idx) = find_last_conversation_anchor_before(messages, last_user_idx) {
            if let Some(object) = messages[anchor_idx].as_object_mut() {
                if add_cache_control_to_message(object) {
                    applied += 1;
                    remaining -= 1;
                }
            }
        }
        if let Some(object) = messages[last_user_idx].as_object_mut() {
            strip_cache_control_from_message_object(object);
        }
    }

    if remaining > 0
        && (!multi_turn
            || (primary_idx != last_user_idx
                && primary_idx != first_user_idx
                && !short_followup))
    {
        if let Some(object) = messages[primary_idx].as_object_mut() {
            if add_cache_control_to_user_message_with_chunking(object) {
                applied += 1;
            }
        }
    }
    applied
}

fn user_message_has_bulk_fixture(message: &Value) -> bool {
    message
        .as_object()
        .and_then(|object| object.get("content"))
        .is_some_and(|content| match content {
            Value::String(text) => text.contains("```rust"),
            Value::Array(items) => items.iter().any(|item| {
                item.get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.contains("```rust"))
            }),
            _ => false,
        })
}

fn find_last_conversation_anchor_before(messages: &[Value], before_idx: usize) -> Option<usize> {
    messages
        .iter()
        .enumerate()
        .take(before_idx)
        .rev()
        .find_map(|(idx, message)| {
            message
                .get("role")
                .and_then(Value::as_str)
                .filter(|role| matches!(*role, "assistant" | "tool"))
                .map(|_| idx)
        })
}

fn user_message_byte_len(message: &Value) -> usize {
    message
        .as_object()
        .and_then(|object| object.get("content"))
        .map(content_byte_len)
        .unwrap_or(0)
}

fn content_byte_len(content: &Value) -> usize {
    match content {
        Value::String(text) => text.len(),
        Value::Array(items) => items.iter().map(content_byte_len).sum(),
        Value::Object(object) => object
            .get("text")
            .and_then(Value::as_str)
            .map(str::len)
            .unwrap_or(0),
        _ => 0,
    }
}

fn strip_cache_control_from_message_object(object: &mut Map<String, Value>) {
    object.remove("cache_control");
    if let Some(content) = object.get_mut("content") {
        strip_cache_control_from_content(content);
    }
}

fn strip_cache_control_from_content(content: &mut Value) {
    match content {
        Value::Array(items) => {
            for item in items {
                if let Some(object) = item.as_object_mut() {
                    object.remove("cache_control");
                }
            }
        }
        Value::Object(object) => {
            object.remove("cache_control");
        }
        _ => {}
    }
}

fn add_cache_control_to_user_message_with_chunking(object: &mut Map<String, Value>) -> bool {
    if let Some(content) = object.get_mut("content") {
        if add_cache_control_to_user_content_with_chunking(content) {
            return true;
        }
    }
    add_cache_control(object)
}

fn add_cache_control_to_user_content_with_chunking(content: &mut Value) -> bool {
    match content {
        Value::String(text) => {
            let text = std::mem::take(text);
            write_chunked_user_text_content(content, &text);
            true
        }
        Value::Array(items) => add_cache_control_to_last_object(items),
        Value::Object(object) => add_cache_control(object),
        _ => false,
    }
}

fn write_chunked_user_text_content(content: &mut Value, text: &str) {
    let (stable, tail) = split_user_text_stable_tail(text);
    if let Some(tail) = tail {
        *content = json!([
            {
                "type": "text",
                "text": stable,
                "cache_control": {"type": "ephemeral"}
            },
            {
                "type": "text",
                "text": tail
            }
        ]);
    } else {
        *content = json!([{
            "type": "text",
            "text": stable,
            "cache_control": {"type": "ephemeral"}
        }]);
    }
}

fn model_is_deepseek_flash(model: &str) -> bool {
    let normalized: String = model
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect();
    matches!(
        normalized.as_str(),
        "deepseekv4flash" | "deepseekv4flashfree"
    )
}

fn add_cache_control_to_last_object(items: &mut [Value]) -> bool {
    items
        .iter_mut()
        .rev()
        .find_map(Value::as_object_mut)
        .is_some_and(add_cache_control)
}

fn add_cache_control_to_last_role(messages: &mut [Value], role: &str) -> bool {
    messages
        .iter_mut()
        .rev()
        .find_map(|message| {
            let object = message.as_object_mut()?;
            if object
                .get("role")
                .and_then(Value::as_str)
                .is_some_and(|value| value == role)
            {
                Some(object)
            } else {
                None
            }
        })
        .is_some_and(add_cache_control_to_message)
}

fn add_cache_control_to_message(object: &mut Map<String, Value>) -> bool {
    if let Some(content) = object.get_mut("content") {
        if add_cache_control_to_content(content) {
            return true;
        }
    }
    add_cache_control(object)
}

fn add_cache_control_to_content(content: &mut Value) -> bool {
    match content {
        Value::Array(items) => add_cache_control_to_last_object(items),
        Value::String(text) => {
            let text = std::mem::take(text);
            *content = json!([{
                "type": "text",
                "text": text,
                "cache_control": {"type": "ephemeral"}
            }]);
            true
        }
        Value::Object(object) => add_cache_control(object),
        _ => false,
    }
}

fn add_cache_control(object: &mut Map<String, Value>) -> bool {
    if object.contains_key("cache_control") {
        return false;
    }
    object.insert("cache_control".to_string(), json!({"type": "ephemeral"}));
    true
}

fn sort_json_value(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().cloned().collect();
            keys.sort();
            let mut sorted = Map::new();
            for key in keys {
                if let Some(child) = map.get(&key) {
                    sorted.insert(key, sort_json_value(child.clone()));
                }
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sort_json_value).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::{OpenAITool, OpenAIToolFunction, ToolCall, ToolFunction};
    use serde_json::Value;

    #[test]
    fn tools_epoch_is_stable_for_same_shape() {
        let tools = vec![OpenAITool {
            tool_type: "function".into(),
            function: OpenAIToolFunction {
                name: "Bash".into(),
                description: Some("run".into()),
                parameters: Some(
                    json!({"type":"object","properties":{"command":{"type":"string"}}}),
                ),
            },
        }];
        let first = freeze_tools_epoch("deepseek-v4-flash", "sess-a", &tools);
        let second = apply_tools_epoch("deepseek-v4-flash", "sess-a", &tools);
        assert_eq!(first, second);
    }

    #[test]
    fn tools_epoch_rejects_same_name_with_different_schema() {
        let original = vec![OpenAITool {
            tool_type: "function".into(),
            function: OpenAIToolFunction {
                name: "Bash".into(),
                description: Some("run".into()),
                parameters: Some(
                    json!({"type":"object","properties":{"command":{"type":"string"}}}),
                ),
            },
        }];
        let changed = vec![OpenAITool {
            tool_type: "function".into(),
            function: OpenAIToolFunction {
                name: "Bash".into(),
                description: Some("run".into()),
                parameters: Some(json!({
                    "type":"object",
                    "properties":{
                        "command":{"type":"string"},
                        "timeout_ms":{"type":"integer"}
                    }
                })),
            },
        }];

        let first = freeze_tools_epoch("deepseek-v4-flash", "sess-schema-a", &original);
        let second = apply_tools_epoch("deepseek-v4-flash", "sess-schema-a", &changed);

        assert_ne!(first, second);
        assert_eq!(second, canonical_tools_value(&changed));
    }

    #[test]
    fn tools_epoch_ignores_trf_transient_toolsearch_drift() {
        let core = vec![OpenAITool {
            tool_type: "function".into(),
            function: OpenAIToolFunction {
                name: "Bash".into(),
                description: Some("run".into()),
                parameters: Some(
                    json!({"type":"object","properties":{"command":{"type":"string"}}}),
                ),
            },
        }];
        let with_toolsearch = vec![
            core[0].clone(),
            OpenAITool {
                tool_type: "function".into(),
                function: OpenAIToolFunction {
                    name: "ToolSearch".into(),
                    description: Some("search".into()),
                    parameters: Some(json!({"type":"object","properties":{"query":{"type":"string"}}})),
                },
            },
        ];

        let frozen = freeze_tools_epoch("deepseek-v4-flash", "sess-trf-a", &core);
        let applied = apply_tools_epoch("deepseek-v4-flash", "sess-trf-a", &with_toolsearch);
        assert_eq!(frozen, applied);
    }

    #[test]
    fn upstream_message_includes_reasoning_content() {
        let message = Message {
            role: "assistant".into(),
            content: Value::String("ok".into()),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: Some("thought".into()),
        };
        let json = message_to_upstream_json(&message);
        assert_eq!(json["reasoning_content"], "thought");
    }

    #[test]
    fn tool_call_reasoning_backfill_uses_canonical_arguments() {
        let mut messages = vec![Message {
            role: "assistant".into(),
            content: Value::Null,
            tool_calls: Some(vec![ToolCall {
                id: Some("call_runtime".into()),
                call_type: "function".into(),
                function: ToolFunction {
                    name: "Bash".into(),
                    arguments: r#"{"b":2,"a":1}"#.into(),
                },
                index: Some(0),
            }]),
            tool_call_id: None,
            reasoning_content: None,
        }];

        record_tool_call_reasoning(
            "unit-tool-reasoning-scope",
            "bash",
            r#"{"a":1,"b":2}"#,
            "stored tool reasoning",
        );
        let enriched =
            enrich_messages_with_tool_call_reasoning(&mut messages, "unit-tool-reasoning-scope");

        assert_eq!(enriched, 1);
        assert_eq!(
            messages[0].reasoning_content.as_deref(),
            Some("Tool call reasoning replayed for bash.")
        );
    }

    #[test]
    fn tool_call_reasoning_backfill_uses_global_stable_fallback() {
        let mut messages = vec![Message {
            role: "assistant".into(),
            content: Value::Null,
            tool_calls: Some(vec![ToolCall {
                id: Some("call_runtime".into()),
                call_type: "function".into(),
                function: ToolFunction {
                    name: "Bash".into(),
                    arguments: r#"{"command":"pwd"}"#.into(),
                },
                index: Some(0),
            }]),
            tool_call_id: None,
            reasoning_content: None,
        }];

        record_tool_call_reasoning(
            "unit-first-provider-scope",
            "Bash",
            r#"{"command":"pwd"}"#,
            "dynamic hidden reasoning",
        );
        let enriched =
            enrich_messages_with_tool_call_reasoning(&mut messages, "unit-second-provider-scope");

        assert_eq!(enriched, 1);
        assert_eq!(
            messages[0].reasoning_content.as_deref(),
            Some("Tool call reasoning replayed for bash.")
        );
    }

    #[test]
    fn big_pickle_adds_prompt_cache_key_and_cache_control_breakpoints() {
        let request = ChatRequest {
            model: "big-pickle".into(),
            messages: vec![
                Message {
                    role: "system".into(),
                    content: Value::String("stable system".into()),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
                Message {
                    role: "user".into(),
                    content: Value::String("first".into()),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
                Message {
                    role: "assistant".into(),
                    content: Value::String("second".into()),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
                Message {
                    role: "user".into(),
                    content: Value::String("tail".into()),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
            ],
            stream: Some(true),
            max_tokens: Some(1024),
            temperature: None,
            top_p: None,
            tools: Some(vec![OpenAITool {
                tool_type: "function".into(),
                function: OpenAIToolFunction {
                    name: "Read".into(),
                    description: Some("read".into()),
                    parameters: Some(
                        json!({"type":"object","properties":{"path":{"type":"string"}}}),
                    ),
                },
            }]),
            tool_choice: None,
        };
        let package = prepare_icp_upstream_request(
            &request,
            "scope",
            "big-pickle",
            &UskContext {
                api_key_id: "key",
                public_model: "big-pickle",
                upstream_model: "big-pickle",
                source_client: "claude-code",
            },
            &CcpFlags {
                icp_enabled: true,
                prompt_cache_key: true,
                anthropic_breakpoints: true,
                reasoning_sidecar: true,
                trf_strict: true,
            },
        );

        assert!(package.body.get("prompt_cache_key").is_some());
        assert_eq!(count_cache_controls(&package.body), 3);
        assert_eq!(
            package.body["tools"][0]["cache_control"],
            json!({"type":"ephemeral"})
        );
        assert_eq!(package.body["messages"][0]["cache_control"], Value::Null);
        assert_eq!(
            package.body["messages"][0]["content"][0]["cache_control"],
            json!({"type":"ephemeral"})
        );
        assert_eq!(package.body["messages"][2]["content"], json!("second"));
        assert_eq!(
            package.body["messages"][3]["content"][0]["cache_control"],
            json!({"type":"ephemeral"})
        );
    }

    #[test]
    fn mimo_uses_breakpoint_while_hy3_uses_only_prompt_cache_key() {
        for (public_model, upstream_model) in [("mimo-v2.5", "mimo-v2.5-free"), ("hy3", "hy3-free")]
        {
            let request = ChatRequest {
                model: public_model.into(),
                messages: vec![
                    Message {
                        role: "user".into(),
                        content: Value::String("stable prefix".into()),
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                    },
                    Message {
                        role: "user".into(),
                        content: Value::String("tail".into()),
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                    },
                ],
                stream: Some(true),
                max_tokens: Some(1024),
                temperature: None,
                top_p: None,
                tools: None,
                tool_choice: None,
            };
            let package = prepare_icp_upstream_request(
                &request,
                "scope",
                upstream_model,
                &UskContext {
                    api_key_id: "key",
                    public_model,
                    upstream_model,
                    source_client: "claude-code",
                },
                &CcpFlags {
                    icp_enabled: true,
                    prompt_cache_key: true,
                    anthropic_breakpoints: true,
                    reasoning_sidecar: true,
                    trf_strict: true,
                },
            );

            assert!(package.body.get("prompt_cache_key").is_some());
            let expected_breakpoints = usize::from(public_model == "mimo-v2.5");
            assert_eq!(
                count_cache_controls(&package.body),
                expected_breakpoints,
                "{public_model}"
            );
            assert_eq!(
                package.body["messages"][0]["content"],
                json!("stable prefix"),
                "{public_model}"
            );
            if public_model == "mimo-v2.5" {
                assert_eq!(
                    package.body["messages"][1]["content"][0]["cache_control"],
                    json!({"type":"ephemeral"}),
                    "{public_model}"
                );
            } else {
                assert_eq!(package.body["messages"][1]["content"], json!("tail"));
            }
        }
    }

    #[test]
    fn deepseek_does_not_add_global_anthropic_cache_control_breakpoints() {
        let request = ChatRequest {
            model: "deepseek-v4-flash".into(),
            messages: vec![Message {
                role: "user".into(),
                content: Value::String("hello".into()),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            stream: Some(true),
            max_tokens: Some(1024),
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
        };
        let package = prepare_icp_upstream_request(
            &request,
            "scope",
            "deepseek-v4-flash-free",
            &UskContext {
                api_key_id: "key",
                public_model: "deepseek-v4-flash",
                upstream_model: "deepseek-v4-flash-free",
                source_client: "claude-code",
            },
            &CcpFlags {
                icp_enabled: true,
                prompt_cache_key: true,
                anthropic_breakpoints: true,
                reasoning_sidecar: true,
                trf_strict: true,
            },
        );

        assert!(package.body.get("prompt_cache_key").is_some());
        assert_eq!(count_cache_controls(&package.body), 0);
    }

    #[test]
    fn deepseek_stable_breakpoints_match_opencode_auto_policy() {
        let request = ChatRequest {
            model: "deepseek-v4-flash".into(),
            messages: vec![
                Message {
                    role: "system".into(),
                    content: Value::String("stable system".into()),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
                Message {
                    role: "user".into(),
                    content: Value::String("current question".into()),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
            ],
            stream: Some(true),
            max_tokens: Some(1024),
            temperature: None,
            top_p: None,
            tools: Some(vec![OpenAITool {
                tool_type: "function".into(),
                function: OpenAIToolFunction {
                    name: "Read".into(),
                    description: Some("read".into()),
                    parameters: Some(
                        json!({"type":"object","properties":{"path":{"type":"string"}}}),
                    ),
                },
            }]),
            tool_choice: None,
        };
        let package = prepare_icp_upstream_request(
            &request,
            "scope",
            "deepseek-v4-flash-free",
            &UskContext {
                api_key_id: "key",
                public_model: "deepseek-v4-flash",
                upstream_model: "deepseek-v4-flash-free",
                source_client: "claude-code",
            },
            &CcpFlags {
                icp_enabled: true,
                prompt_cache_key: true,
                anthropic_breakpoints: true,
                reasoning_sidecar: true,
                trf_strict: true,
            },
        );
        let mut body = package.body;
        assert_eq!(
            apply_deepseek_stable_cache_breakpoints(&mut body, &request),
            3
        );
        assert_eq!(count_cache_controls(&body), 3);
        assert_eq!(
            body["tools"][0]["cache_control"],
            json!({"type":"ephemeral"})
        );
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"],
            json!({"type":"ephemeral"})
        );
        assert_eq!(
            body["messages"][1]["content"][0]["cache_control"],
            json!({"type":"ephemeral"})
        );
    }

    #[test]
    fn deepseek_intelligent_chunking_splits_rust_bulk_from_suffix() {
        let bulk = "x".repeat(80_000);
        let prefix = format!(
            "以下是需要通读的大型 Rust 模块源码（测试夹具）：\n\n```rust\n{bulk}\n```\n\n"
        );
        let suffix_q1 = "编程题1：validate_session 返回什么？";
        let suffix_q2 = "编程题2：input==0 时返回什么？";
        let request = ChatRequest {
            model: "deepseek-v4-flash".into(),
            messages: vec![Message {
                role: "user".into(),
                content: Value::String(format!("{prefix}{suffix_q1}")),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            stream: Some(true),
            max_tokens: Some(2048),
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
        };
        let package = prepare_icp_upstream_request(
            &request,
            "scope",
            "deepseek-v4-flash-free",
            &UskContext {
                api_key_id: "key",
                public_model: "deepseek-v4-flash",
                upstream_model: "deepseek-v4-flash-free",
                source_client: "claude-code",
            },
            &CcpFlags {
                icp_enabled: true,
                prompt_cache_key: true,
                anthropic_breakpoints: true,
                reasoning_sidecar: true,
                trf_strict: true,
            },
        );
        let mut body = package.body;
        assert_eq!(apply_deepseek_stable_cache_breakpoints(&mut body, &request), 1);
        let user_content = &body["messages"][0]["content"];
        assert_eq!(user_content.as_array().unwrap().len(), 2);
        assert_eq!(
            user_content[0]["cache_control"],
            json!({"type":"ephemeral"})
        );
        assert!(user_content[0]["text"].as_str().unwrap().contains("```rust"));
        assert!(user_content[1].get("cache_control").is_none());
        assert_eq!(user_content[1]["text"], json!(suffix_q1));

        let request_q2 = ChatRequest {
            model: request.model.clone(),
            messages: vec![Message {
                role: "user".into(),
                content: Value::String(format!("{prefix}{suffix_q2}")),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            stream: request.stream,
            max_tokens: request.max_tokens,
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
        };
        let package_q2 = prepare_icp_upstream_request(
            &request_q2,
            "scope",
            "deepseek-v4-flash-free",
            &UskContext {
                api_key_id: "key",
                public_model: "deepseek-v4-flash",
                upstream_model: "deepseek-v4-flash-free",
                source_client: "claude-code",
            },
            &CcpFlags {
                icp_enabled: true,
                prompt_cache_key: true,
                anthropic_breakpoints: true,
                reasoning_sidecar: true,
                trf_strict: true,
            },
        );
        let mut body_q2 = package_q2.body;
        apply_deepseek_stable_cache_breakpoints(&mut body_q2, &request_q2);
        assert_eq!(
            body["messages"][0]["content"][0]["text"],
            body_q2["messages"][0]["content"][0]["text"]
        );
        assert_eq!(body_q2["messages"][0]["content"][1]["text"], json!(suffix_q2));
    }

    #[test]
    fn deepseek_intelligent_chunking_short_user_stays_single_block() {
        let request = ChatRequest {
            model: "deepseek-v4-flash".into(),
            messages: vec![Message {
                role: "user".into(),
                content: Value::String("hello".into()),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            stream: Some(true),
            max_tokens: Some(1024),
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
        };
        let package = prepare_icp_upstream_request(
            &request,
            "scope",
            "deepseek-v4-flash-free",
            &UskContext {
                api_key_id: "key",
                public_model: "deepseek-v4-flash",
                upstream_model: "deepseek-v4-flash-free",
                source_client: "claude-code",
            },
            &CcpFlags {
                icp_enabled: true,
                prompt_cache_key: true,
                anthropic_breakpoints: true,
                reasoning_sidecar: true,
                trf_strict: true,
            },
        );
        let mut body = package.body;
        assert_eq!(apply_deepseek_stable_cache_breakpoints(&mut body, &request), 1);
        let user_content = &body["messages"][0]["content"];
        assert_eq!(user_content.as_array().unwrap().len(), 1);
        assert_eq!(
            user_content[0]["cache_control"],
            json!({"type":"ephemeral"})
        );
    }

    #[test]
    fn deepseek_intelligent_chunking_splits_small_rust_bulk_via_fence() {
        let bulk = "z".repeat(30_000);
        let prefix = format!(
            "以下是需要通读的大型 Rust 模块源码（测试夹具）：\n\n```rust\n{bulk}\n```\n\n"
        );
        let request = ChatRequest {
            model: "deepseek-v4-flash".into(),
            messages: vec![Message {
                role: "user".into(),
                content: Value::String(format!("{prefix}probe suffix")),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            stream: Some(true),
            max_tokens: Some(128),
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
        };
        let package = prepare_icp_upstream_request(
            &request,
            "scope",
            "deepseek-v4-flash-free",
            &UskContext {
                api_key_id: "key",
                public_model: "deepseek-v4-flash",
                upstream_model: "deepseek-v4-flash-free",
                source_client: "claude-code",
            },
            &CcpFlags {
                icp_enabled: true,
                prompt_cache_key: true,
                anthropic_breakpoints: true,
                reasoning_sidecar: true,
                trf_strict: true,
            },
        );
        let mut body = package.body;
        apply_deepseek_stable_cache_breakpoints(&mut body, &request);
        let user_content = &body["messages"][0]["content"];
        assert_eq!(user_content.as_array().unwrap().len(), 2);
        assert!(user_content[1].get("cache_control").is_none());
    }

    #[test]
    fn deepseek_multiturn_chunks_primary_user_not_tail_question() {
        let bulk = "y".repeat(80_000);
        let prefix = format!(
            "以下是需要通读的大型 Rust 模块源码（测试夹具）：\n\n```rust\n{bulk}\n```\n\n"
        );
        let load_suffix = "通读上述源码。只回复：PROG_LOADED_01";
        let q_suffix = "编程题1：validate_session 返回什么错误？";
        let request = ChatRequest {
            model: "deepseek-v4-flash".into(),
            messages: vec![
                Message {
                    role: "user".into(),
                    content: Value::String(format!("{prefix}{load_suffix}")),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
                Message {
                    role: "assistant".into(),
                    content: Value::String("PROG_LOADED_01".into()),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
                Message {
                    role: "user".into(),
                    content: Value::String(q_suffix.into()),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
            ],
            stream: Some(true),
            max_tokens: Some(2048),
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
        };
        let package = prepare_icp_upstream_request(
            &request,
            "scope",
            "deepseek-v4-flash-free",
            &UskContext {
                api_key_id: "key",
                public_model: "deepseek-v4-flash",
                upstream_model: "deepseek-v4-flash-free",
                source_client: "claude-code",
            },
            &CcpFlags {
                icp_enabled: true,
                prompt_cache_key: true,
                anthropic_breakpoints: true,
                reasoning_sidecar: true,
                trf_strict: true,
            },
        );
        let mut body = package.body;
        assert_eq!(apply_deepseek_stable_cache_breakpoints(&mut body, &request), 2);
        let bulk_user = &body["messages"][0]["content"];
        assert_eq!(bulk_user.as_array().unwrap().len(), 2);
        assert_eq!(
            bulk_user[0]["cache_control"],
            json!({"type":"ephemeral"})
        );
        assert!(bulk_user[1].get("cache_control").is_none());
        assert_eq!(
            body["messages"][1]["content"][0]["cache_control"],
            json!({"type":"ephemeral"})
        );
        let tail_user = &body["messages"][2]["content"];
        assert!(tail_user.get("cache_control").is_none());
        if tail_user.is_array() {
            assert!(tail_user[0].get("cache_control").is_none());
        }
    }

    #[test]
    fn deepseek_multiturn_short_probe_pins_first_user_and_latest_followup() {
        let warmup = "不要工具。只回复一行：MATRIX_CASE_01_OK";
        let probe = "基于上文，用一句话确认任务已完成。最后一行只写：SESSION_PROBE_OK";
        let request = ChatRequest {
            model: "deepseek-v4-flash".into(),
            messages: vec![
                Message {
                    role: "user".into(),
                    content: Value::String(warmup.into()),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
                Message {
                    role: "assistant".into(),
                    content: Value::String("MATRIX_CASE_01_OK".into()),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
                Message {
                    role: "user".into(),
                    content: Value::String(probe.into()),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
            ],
            stream: Some(true),
            max_tokens: Some(1024),
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
        };
        let package = prepare_icp_upstream_request(
            &request,
            "scope",
            "deepseek-v4-flash-free",
            &UskContext {
                api_key_id: "key",
                public_model: "deepseek-v4-flash",
                upstream_model: "deepseek-v4-flash-free",
                source_client: "claude-code",
            },
            &CcpFlags {
                icp_enabled: true,
                prompt_cache_key: true,
                anthropic_breakpoints: true,
                reasoning_sidecar: true,
                trf_strict: true,
            },
        );
        let mut body = package.body;
        assert_eq!(apply_deepseek_stable_cache_breakpoints(&mut body, &request), 2);
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"],
            json!({"type":"ephemeral"})
        );
        assert!(body["messages"][1].get("cache_control").is_none());
        assert_eq!(
            body["messages"][2]["content"][0]["cache_control"],
            json!({"type":"ephemeral"})
        );
    }

    #[test]
    fn opencode_auto_policy_ignores_trailing_tool_result_for_message_breakpoint() {
        let request = ChatRequest {
            model: "mimo-v2.5".into(),
            messages: vec![
                Message {
                    role: "user".into(),
                    content: Value::String("current user request".into()),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
                Message {
                    role: "assistant".into(),
                    content: Value::String("need tool".into()),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
                Message {
                    role: "tool".into(),
                    content: Value::String("dynamic tool output".into()),
                    tool_calls: None,
                    tool_call_id: Some("toolu_1".into()),
                    reasoning_content: None,
                },
            ],
            stream: Some(true),
            max_tokens: Some(1024),
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
        };
        let package = prepare_icp_upstream_request(
            &request,
            "scope",
            "mimo-v2.5-free",
            &UskContext {
                api_key_id: "key",
                public_model: "mimo-v2.5",
                upstream_model: "mimo-v2.5-free",
                source_client: "claude-code",
            },
            &CcpFlags {
                icp_enabled: true,
                prompt_cache_key: true,
                anthropic_breakpoints: true,
                reasoning_sidecar: true,
                trf_strict: true,
            },
        );

        assert_eq!(count_cache_controls(&package.body), 1);
        assert_eq!(
            package.body["messages"][0]["content"][0]["cache_control"],
            json!({"type":"ephemeral"})
        );
        assert_eq!(package.body["messages"][1]["content"], json!("need tool"));
        assert_eq!(
            package.body["messages"][2]["content"],
            json!("dynamic tool output")
        );
    }

    fn count_cache_controls(value: &Value) -> usize {
        match value {
            Value::Array(items) => items.iter().map(count_cache_controls).sum(),
            Value::Object(map) => {
                usize::from(map.contains_key("cache_control"))
                    + map.values().map(count_cache_controls).sum::<usize>()
            }
            _ => 0,
        }
    }
}
