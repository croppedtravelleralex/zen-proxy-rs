use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use serde_json::{json, Map, Value};

use crate::ccp::{apply_prompt_cache_key, CcpFlags, IcpIdentity, UskContext};
use crate::protocol::types::{ChatRequest, Message, OpenAITool};

static TOOLS_EPOCH: OnceLock<RwLock<HashMap<String, Value>>> = OnceLock::new();

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
            if tools_semantically_compatible(tools, frozen) {
                return frozen.clone();
            }
        }
    }
    freeze_tools_epoch(model, session_scope, tools)
}

fn tools_semantically_compatible(current: &[OpenAITool], frozen: &Value) -> bool {
    let Some(frozen_tools) = frozen.as_array() else {
        return false;
    };
    if frozen_tools.len() != current.len() {
        return false;
    }
    current
        .iter()
        .zip(frozen_tools.iter())
        .all(|(left, right)| {
            left.function.name
                == right
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
        })
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
    let Some(key) = tool_call_reasoning_key(session_scope, tool_name, tool_arguments) else {
        return;
    };
    crate::session::reasoning_store::put_reasoning(&key, reasoning.to_string());
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
            if let Some(reasoning) = crate::session::reasoning_store::get_reasoning(&key) {
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
    if add_cache_control_to_role(messages, "system") {
        remaining -= 1;
    }
    if remaining == 0 {
        return;
    }

    let mut candidates = Vec::new();
    if messages.len() > 1 {
        candidates.push(messages.len() - 2);
    }
    if !messages.is_empty() {
        candidates.push(messages.len() - 1);
    }
    candidates.sort_unstable();
    candidates.dedup();
    for index in candidates {
        if remaining == 0 {
            break;
        }
        if add_cache_control_to_message_at(messages, index) {
            remaining -= 1;
        }
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

fn add_cache_control_to_last_object(items: &mut [Value]) -> bool {
    items
        .iter_mut()
        .rev()
        .find_map(Value::as_object_mut)
        .is_some_and(add_cache_control)
}

fn add_cache_control_to_role(messages: &mut [Value], role: &str) -> bool {
    messages
        .iter_mut()
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

fn add_cache_control_to_message_at(messages: &mut [Value], index: usize) -> bool {
    messages
        .get_mut(index)
        .and_then(Value::as_object_mut)
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
            Some("stored tool reasoning")
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
        assert_eq!(count_cache_controls(&package.body), 4);
        assert_eq!(
            package.body["tools"][0]["cache_control"],
            json!({"type":"ephemeral"})
        );
        assert_eq!(package.body["messages"][0]["cache_control"], Value::Null);
        assert_eq!(
            package.body["messages"][0]["content"][0]["cache_control"],
            json!({"type":"ephemeral"})
        );
    }

    #[test]
    fn mimo_adds_content_block_cache_control_breakpoints() {
        let request = ChatRequest {
            model: "mimo-v2.5".into(),
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

        assert!(package.body.get("prompt_cache_key").is_some());
        assert_eq!(count_cache_controls(&package.body), 2);
        assert_eq!(
            package.body["messages"][0]["content"][0]["cache_control"],
            json!({"type":"ephemeral"})
        );
        assert_eq!(
            package.body["messages"][1]["content"][0]["cache_control"],
            json!({"type":"ephemeral"})
        );
    }

    #[test]
    fn deepseek_does_not_add_anthropic_cache_control_breakpoints() {
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
