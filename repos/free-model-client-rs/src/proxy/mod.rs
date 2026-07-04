pub mod anthropic;
pub mod markdown;
pub mod openai;
pub mod sse;

use crate::canonical;
use crate::ccp::{self, CcpFlags, UskContext};
use crate::client_profile::{ClientKind, ClientProfile};
use crate::error::AppError;
use crate::protocol::{translate, types::ChatRequest};
use crate::thinking_manifest;
use crate::zen::client::{CollectedStream, ProviderCacheSignals};
use serde_json::{json, Value};

const PROVIDER_INVALID_RETRY_LARGE_USER_BYTES: usize = 12 * 1024;
const PROVIDER_INVALID_RETRY_HEAD_CHARS: usize = 6 * 1024;
const PROVIDER_INVALID_RETRY_TAIL_CHARS: usize = 2 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputClass {
    Valid,
    Empty,
    ReasoningOnly,
    ReasoningOnlyLength,
}

impl OutputClass {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Empty => "empty_output",
            Self::ReasoningOnly => "reasoning_only",
            Self::ReasoningOnlyLength => "reasoning_only_length",
        }
    }

    pub(crate) fn should_retry_with_enriched_reasoning(self, profile: ClientProfile) -> bool {
        thinking_manifest::preserves_thinking_on_retry(profile)
            && matches!(self, Self::ReasoningOnly | Self::ReasoningOnlyLength)
    }
}

pub(crate) fn classify_collected_output(
    collected: &CollectedStream,
    rendered_content: &str,
) -> OutputClass {
    if !rendered_content.trim().is_empty() || !collected.tool_calls.is_empty() {
        return OutputClass::Valid;
    }
    if !collected.reasoning.trim().is_empty() {
        if collected.finish_reason.as_deref() == Some("length") {
            return OutputClass::ReasoningOnlyLength;
        }
        return OutputClass::ReasoningOnly;
    }
    OutputClass::Empty
}

pub(crate) fn apply_initial_thinking_policy(
    body: &mut serde_json::Value,
    request: &ChatRequest,
    profile: ClientProfile,
) -> &'static str {
    thinking_manifest::apply_thinking_manifest(body, request, profile)
}

pub(crate) fn prune_null_optional_upstream_fields(body: &mut serde_json::Value) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    for key in ["tools", "tool_choice"] {
        if object.get(key).is_some_and(Value::is_null) {
            object.remove(key);
        }
    }
}

pub(crate) fn downgrade_claude_code_forced_tool_choice_for_upstream_model(
    body: &mut serde_json::Value,
    request: &mut ChatRequest,
    profile: ClientProfile,
    upstream_model: &str,
) -> Option<&'static str> {
    let _ = (body, request, profile, upstream_model);
    None
}

pub(crate) fn client_kind_label(profile: ClientProfile) -> &'static str {
    match profile.kind {
        ClientKind::ClaudeCode => "claude-code",
        ClientKind::Hermes => "hermes",
        ClientKind::OpenClaw => "openclaw",
        ClientKind::CherryStudio => "cherrystudio",
        ClientKind::AnthropicSdk => "anthropic-sdk",
        ClientKind::OpenAiSdk => "openai-sdk",
        ClientKind::Unknown => "unknown",
    }
}

pub(crate) fn api_key_bucket(api_key: &str) -> String {
    ccp::api_key_id_for_cache(api_key)
}

pub(crate) fn build_icp_upstream_package(
    request: &ChatRequest,
    upstream_model: &str,
    profile: ClientProfile,
    api_key: &str,
) -> canonical::IcpUpstreamPackage {
    let session_scope = session_scope_for_request(request);
    let flags = CcpFlags::from_env();
    canonical::prepare_icp_upstream_request(
        request,
        &session_scope,
        upstream_model,
        &UskContext {
            api_key_id: &api_key_bucket(api_key),
            public_model: &request.model,
            upstream_model,
            source_client: client_kind_label(profile),
        },
        &flags,
    )
}

pub(crate) fn zen_session_headers(identity: &ccp::IcpIdentity) -> Vec<(String, String)> {
    vec![
        ("x-opencode-session".into(), identity.zen_session_id.clone()),
        ("x-opencode-project".into(), "global".into()),
    ]
}

pub(crate) fn merge_extra_headers(
    base: &[(String, String)],
    extra: &[(String, String)],
) -> Vec<(String, String)> {
    let mut merged = base.to_vec();
    merged.extend(extra.iter().cloned());
    merged
}

pub(crate) fn session_scope_from_upstream_body(body: &Value) -> String {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let messages_key = body
        .get("messages")
        .and_then(|value| serde_json::to_string(value).ok())
        .unwrap_or_default();
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    messages_key.hash(&mut hasher);
    format!("{model}:{:016x}", hasher.finish())
}

pub(crate) fn reasoning_scope_from_upstream_body(body: &Value) -> String {
    body.get("prompt_cache_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| session_scope_from_upstream_body(body))
}

pub(crate) fn session_scope_for_request(request: &ChatRequest) -> String {
    let shape = translate::request_shape(request);
    format!("{}:{:016x}", request.model, shape.prompt_hash)
}

pub(crate) fn record_collected_reasoning_for_request(request: &ChatRequest, reasoning: &str) {
    if reasoning.trim().is_empty() {
        return;
    }
    let scope = session_scope_for_request(request);
    let assistant_index = request
        .messages
        .iter()
        .filter(|message| message.role == "assistant")
        .count();
    canonical::record_collected_reasoning(&scope, assistant_index, reasoning);
}

pub(crate) fn reasoning_disabled_retry_body(body: &serde_json::Value) -> serde_json::Value {
    thinking_manifest::reasoning_enriched_retry_body(body)
}

pub(crate) fn reasoning_retry_body(
    body: &serde_json::Value,
    profile: ClientProfile,
) -> serde_json::Value {
    reasoning_retry_body_with_scope(body, profile, "")
}

pub(crate) fn reasoning_retry_body_with_scope(
    body: &serde_json::Value,
    profile: ClientProfile,
    reasoning_scope: &str,
) -> serde_json::Value {
    if thinking_manifest::preserves_thinking_on_retry(profile) {
        let mut retry = body.clone();
        let session_scope = if reasoning_scope.trim().is_empty() {
            reasoning_scope_from_upstream_body(&retry)
        } else {
            reasoning_scope.to_string()
        };
        if let Some(messages) = retry.get_mut("messages").and_then(Value::as_array_mut) {
            let mut typed = messages
                .iter()
                .filter_map(|value| {
                    serde_json::from_value::<crate::protocol::types::Message>(value.clone()).ok()
                })
                .collect::<Vec<_>>();
            canonical::enrich_messages_with_tool_call_reasoning(&mut typed, &session_scope);
            canonical::enrich_messages_with_reasoning_mode(
                &mut typed,
                &session_scope,
                canonical::ReasoningEnrichMode::CurrentTurnOnly,
            );
            *messages = typed
                .into_iter()
                .map(|message| canonical::message_to_upstream_json(&message))
                .collect();
        }
        retry
    } else {
        reasoning_disabled_retry_body(body)
    }
}

pub(crate) fn enrich_tool_call_reasoning_body(
    body: &mut serde_json::Value,
    profile: ClientProfile,
    reasoning_scope: &str,
) -> usize {
    if !thinking_manifest::preserves_thinking_on_retry(profile) {
        return 0;
    }
    let session_scope = if reasoning_scope.trim().is_empty() {
        reasoning_scope_from_upstream_body(body)
    } else {
        reasoning_scope.to_string()
    };
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return 0;
    };
    let mut typed = messages
        .iter()
        .filter_map(|value| {
            serde_json::from_value::<crate::protocol::types::Message>(value.clone()).ok()
        })
        .collect::<Vec<_>>();
    let enriched = canonical::enrich_messages_with_tool_call_reasoning(&mut typed, &session_scope);
    if enriched > 0 {
        *messages = typed
            .into_iter()
            .map(|message| canonical::message_to_upstream_json(&message))
            .collect();
    }
    enriched
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderInvalidRetryMode {
    EnrichReasoning,
    TextOnly,
}

impl ProviderInvalidRetryMode {
    const fn strips_tools(self) -> bool {
        matches!(self, Self::TextOnly)
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::EnrichReasoning => "enrich_reasoning",
            Self::TextOnly => "text_only",
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProviderInvalidRetryStats {
    pub sanitized_tools: usize,
    pub compacted_user_messages: usize,
    pub flattened_tool_history_messages: usize,
    pub stripped_tools: bool,
}

pub(crate) fn provider_invalid_tool_history_retry_mode(
    err: &AppError,
    request: &ChatRequest,
    profile: ClientProfile,
    repair: translate::ToolHistoryRepair,
    used_enriched_retry: bool,
    used_text_only_retry: bool,
) -> Option<ProviderInvalidRetryMode> {
    let missing_reasoning = err.is_missing_reasoning_content();
    let invalid_tool_history = err.is_provider_invalid_request();
    if !(invalid_tool_history || missing_reasoning) {
        return None;
    }
    if !is_risky_claude_code_tool_history_request(request, profile, repair, missing_reasoning) {
        return None;
    }
    if profile.kind == ClientKind::ClaudeCode && !used_enriched_retry {
        return Some(ProviderInvalidRetryMode::EnrichReasoning);
    }
    if !used_text_only_retry {
        return Some(ProviderInvalidRetryMode::TextOnly);
    }
    None
}

pub(crate) fn provider_invalid_tool_history_retry_body(
    body: &Value,
    mode: ProviderInvalidRetryMode,
) -> (Value, ProviderInvalidRetryStats) {
    let mut retry = match mode {
        ProviderInvalidRetryMode::EnrichReasoning => reasoning_retry_body(
            body,
            ClientProfile::new(
                ClientKind::ClaudeCode,
                crate::client_profile::ClientProfileSource::Unknown,
            ),
        ),
        ProviderInvalidRetryMode::TextOnly => body.clone(),
    };
    let sanitized_tools = sanitize_upstream_tools(&mut retry);
    let compacted_user_messages = compact_large_user_messages_for_retry(&mut retry);
    if mode.strips_tools() {
        let flattened_tool_history_messages = flatten_tool_history_for_text_only_retry(&mut retry);
        retry["tools"] = Value::Null;
        retry["tool_choice"] = Value::Null;
        return (
            retry,
            ProviderInvalidRetryStats {
                sanitized_tools,
                compacted_user_messages,
                flattened_tool_history_messages,
                stripped_tools: true,
            },
        );
    }
    (
        retry,
        ProviderInvalidRetryStats {
            sanitized_tools,
            compacted_user_messages,
            flattened_tool_history_messages: 0,
            stripped_tools: mode.strips_tools(),
        },
    )
}

fn is_risky_claude_code_tool_history_request(
    request: &ChatRequest,
    profile: ClientProfile,
    repair: translate::ToolHistoryRepair,
    missing_reasoning: bool,
) -> bool {
    if profile.kind != ClientKind::ClaudeCode
        || request.tools.as_ref().is_none_or(|tools| tools.is_empty())
    {
        return false;
    }
    if repair.downgraded_tool_results > 0 || repair.downgraded_assistant_calls > 0 {
        return true;
    }
    missing_reasoning
        && request.messages.iter().any(|message| {
            message.role == "assistant"
                && message
                    .tool_calls
                    .as_ref()
                    .is_some_and(|calls| !calls.is_empty())
                && message
                    .reasoning_content
                    .as_ref()
                    .is_none_or(|reasoning| reasoning.trim().is_empty())
        })
}

fn sanitize_upstream_tools(body: &mut Value) -> usize {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return 0;
    };
    let mut changed = 0usize;
    for tool in tools {
        let Some(function) = tool.get_mut("function").and_then(Value::as_object_mut) else {
            continue;
        };
        let params = function
            .entry("parameters")
            .or_insert_with(|| json!({"type":"object","properties":{}}));
        if !params.is_object() {
            *params = json!({"type":"object","properties":{}});
            changed += 1;
            continue;
        }
        let Some(params_obj) = params.as_object_mut() else {
            continue;
        };
        if params_obj.get("type").and_then(Value::as_str) != Some("object") {
            params_obj.insert("type".to_string(), Value::String("object".to_string()));
            changed += 1;
        }
        if !params_obj.get("properties").is_some_and(Value::is_object) {
            params_obj.insert("properties".to_string(), Value::Object(Default::default()));
            changed += 1;
        }
        let property_keys = params_obj
            .get("properties")
            .and_then(Value::as_object)
            .map(|props| props.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        if let Some(required) = params_obj.get_mut("required") {
            if let Some(items) = required.as_array_mut() {
                let before = items.len();
                items.retain(|item| {
                    item.as_str()
                        .is_some_and(|key| property_keys.iter().any(|known| known == key))
                });
                if items.len() != before {
                    changed += 1;
                }
            } else {
                *required = Value::Array(Vec::new());
                changed += 1;
            }
        }
    }
    changed
}

fn compact_large_user_messages_for_retry(body: &mut Value) -> usize {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return 0;
    };
    let mut changed = 0usize;
    for message in messages {
        if message.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let Some(content) = message.get_mut("content") else {
            continue;
        };
        let Some(text) = content.as_str() else {
            continue;
        };
        if text.len() <= PROVIDER_INVALID_RETRY_LARGE_USER_BYTES {
            continue;
        }
        *content = Value::String(compact_text_for_provider_invalid_retry(text));
        changed += 1;
    }
    changed
}

fn flatten_tool_history_for_text_only_retry(body: &mut Value) -> usize {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return 0;
    };
    let mut changed = 0usize;
    for message in messages {
        let Some(object) = message.as_object_mut() else {
            continue;
        };
        let role = object.get("role").and_then(Value::as_str).unwrap_or_default();
        match role {
            "assistant" if object.get("tool_calls").is_some_and(|calls| !calls.is_null()) => {
                let content = object
                    .get("content")
                    .and_then(value_to_text)
                    .unwrap_or_default();
                let tool_summary = object
                    .get("tool_calls")
                    .map(summarize_tool_calls_for_text_only_retry)
                    .unwrap_or_default();
                let flattened = if content.trim().is_empty() {
                    tool_summary
                } else if tool_summary.trim().is_empty() {
                    content
                } else {
                    format!("{content}\n{tool_summary}")
                };
                object.insert(
                    "content".to_string(),
                    Value::String(if flattened.trim().is_empty() {
                        "[provider text-only retry: previous assistant tool call omitted]"
                            .to_string()
                    } else {
                        flattened
                    }),
                );
                object.remove("tool_calls");
                object.remove("function_call");
                changed += 1;
            }
            "tool" => {
                let content = object
                    .get("content")
                    .and_then(value_to_text)
                    .unwrap_or_default();
                let tool_name = object
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_string();
                object.insert("role".to_string(), Value::String("user".to_string()));
                object.insert(
                    "content".to_string(),
                    Value::String(format!(
                        "[provider text-only retry: previous {tool_name} result]\n{content}"
                    )),
                );
                object.remove("tool_call_id");
                object.remove("name");
                changed += 1;
            }
            _ => {}
        }
    }
    changed
}

fn value_to_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Null => Some(String::new()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(|part| {
                    part.get("text")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                        .or_else(|| {
                            part.get("content")
                                .and_then(Value::as_str)
                                .map(ToString::to_string)
                        })
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => None,
    }
}

fn summarize_tool_calls_for_text_only_retry(tool_calls: &Value) -> String {
    let Some(calls) = tool_calls.as_array() else {
        return "[provider text-only retry: previous assistant tool call omitted]".to_string();
    };
    let summaries = calls
        .iter()
        .filter_map(|call| {
            let function = call.get("function")?;
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("tool");
            let args = function
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("");
            Some(format!(
                "[provider text-only retry: previous assistant requested tool {name} with arguments {args}]"
            ))
        })
        .collect::<Vec<_>>();
    if summaries.is_empty() {
        "[provider text-only retry: previous assistant tool call omitted]".to_string()
    } else {
        summaries.join("\n")
    }
}

fn compact_text_for_provider_invalid_retry(text: &str) -> String {
    let head = text
        .chars()
        .take(PROVIDER_INVALID_RETRY_HEAD_CHARS)
        .collect::<String>();
    let tail_reversed = text
        .chars()
        .rev()
        .take(PROVIDER_INVALID_RETRY_TAIL_CHARS)
        .collect::<String>();
    let tail = tail_reversed.chars().rev().collect::<String>();
    format!(
        "{head}\n[free-model-client-rs provider-invalid retry: omitted stale recovered tool context; original_bytes={}]\n{tail}",
        text.len()
    )
}

pub(crate) fn should_retry_missing_reasoning_content(
    err: &AppError,
    used_reasoning_retry: bool,
) -> bool {
    err.is_missing_reasoning_content() && !used_reasoning_retry
}

pub(crate) fn log_provider_invalid_tool_history_retry(
    protocol: &'static str,
    request: &ChatRequest,
    profile: ClientProfile,
    repair: translate::ToolHistoryRepair,
    mode: ProviderInvalidRetryMode,
    stats: ProviderInvalidRetryStats,
    attempt: usize,
) {
    let shape = translate::request_shape(request);
    tracing::warn!(
        protocol,
        model = %request.model,
        source_client = ?profile.kind,
        attempt,
        retry_mode = mode.as_str(),
        downgraded_tool_results = repair.downgraded_tool_results,
        downgraded_assistant_calls = repair.downgraded_assistant_calls,
        sanitized_tools = stats.sanitized_tools,
        compacted_user_messages = stats.compacted_user_messages,
        flattened_tool_history_messages = stats.flattened_tool_history_messages,
        stripped_tools = stats.stripped_tools,
        prompt_hash = %format_args!("{:016x}", shape.prompt_hash),
        prompt_tokens = shape.estimated_total_tokens,
        message_count = shape.message_count,
        tool_count = shape.tool_count,
        "retrying provider invalid_request_error for repaired ClaudeCode tool history"
    );
}

pub(crate) fn log_missing_reasoning_content_retry(
    protocol: &'static str,
    request: &ChatRequest,
    profile: ClientProfile,
    attempt: usize,
) {
    let shape = translate::request_shape(request);
    tracing::warn!(
        protocol,
        model = %request.model,
        source_client = ?profile.kind,
        attempt,
        prompt_hash = %format_args!("{:016x}", shape.prompt_hash),
        prompt_tokens = shape.estimated_total_tokens,
        message_count = shape.message_count,
        tool_count = shape.tool_count,
        "retrying upstream missing reasoning_content error with reasoning enrichment"
    );
}

pub(crate) fn log_request_shape(
    protocol: &'static str,
    request: &ChatRequest,
    observed_profile: ClientProfile,
    effective_profile: ClientProfile,
) {
    let shape = translate::request_shape(request);
    let short_request_kind = translate::classify_short_non_stream_request(
        request,
        effective_profile.kind == ClientKind::ClaudeCode,
    );
    tracing::info!(
        protocol,
        model = %request.model,
        source_client = ?observed_profile.kind,
        profile_source = ?observed_profile.source,
        effective_client = ?effective_profile.kind,
        effective_profile_source = ?effective_profile.source,
        stream = shape.stream,
        max_tokens = ?shape.max_tokens,
        message_count = shape.message_count,
        system_tokens = shape.system_tokens,
        messages_tokens = shape.messages_tokens,
        tools_tokens = shape.tools_tokens,
        tool_count = shape.tool_count,
        tool_name_classes = ?shape.tool_name_classes,
        largest_message_tokens = shape.largest_message_tokens,
        last_user_tokens = shape.last_user_tokens,
        estimated_total_tokens = shape.estimated_total_tokens,
        tool_choice_present = shape.tool_choice_present,
        short_request_kind = short_request_kind.as_str(),
        prompt_hash = %format_args!("{:016x}", shape.prompt_hash),
        prefix_4k_hash = %format_args!("{:016x}", shape.prefix_4k_hash),
        prefix_32k_hash = %format_args!("{:016x}", shape.prefix_32k_hash),
        prefix_128k_hash = %format_args!("{:016x}", shape.prefix_128k_hash),
        prefix_256k_hash = %format_args!("{:016x}", shape.prefix_256k_hash),
        cache_material_bytes = shape.cache_material_bytes,
        "desensitized request shape before upstream"
    );
}

pub(crate) fn log_provider_cache_observation(
    protocol: &'static str,
    request: &ChatRequest,
    profile: ClientProfile,
    signals: &ProviderCacheSignals,
    attempt: usize,
    max_attempts: usize,
) {
    let shape = translate::request_shape(request);
    tracing::info!(
        protocol,
        model = %request.model,
        source_client = ?profile.kind,
        cache_observation = signals.status().as_str(),
        attempt,
        max_attempts,
        provider_response_signal = signals.response_seen,
        provider_header_usage_signal = signals.header_usage_signal,
        provider_body_usage_signal = signals.body_usage_signal,
        provider_header_cache_hit = ?signals.header_cache_hit,
        provider_header_cache_read_input_tokens = ?signals.header_cache_read_input_tokens,
        provider_header_cache_creation_input_tokens = ?signals.header_cache_creation_input_tokens,
        provider_header_cached_tokens = ?signals.header_cached_tokens,
        provider_body_cache_read_input_tokens = ?signals.body_cache_read_input_tokens,
        provider_body_cache_creation_input_tokens = ?signals.body_cache_creation_input_tokens,
        provider_body_cached_tokens = ?signals.body_cached_tokens,
        provider_body_cache_miss_input_tokens = ?signals.body_cache_miss_input_tokens,
        prompt_hash = %format_args!("{:016x}", shape.prompt_hash),
        prefix_4k_hash = %format_args!("{:016x}", shape.prefix_4k_hash),
        prefix_32k_hash = %format_args!("{:016x}", shape.prefix_32k_hash),
        prefix_128k_hash = %format_args!("{:016x}", shape.prefix_128k_hash),
        prefix_256k_hash = %format_args!("{:016x}", shape.prefix_256k_hash),
        cache_material_bytes = shape.cache_material_bytes,
        estimated_total_tokens = shape.estimated_total_tokens,
        "provider cache usage observation"
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn log_empty_output_class(
    protocol: &'static str,
    request: &ChatRequest,
    profile: ClientProfile,
    class: OutputClass,
    attempt: usize,
    max_attempts: usize,
    collected: &CollectedStream,
) {
    let shape = translate::request_shape(request);
    let short_request_kind = translate::classify_short_non_stream_request(
        request,
        profile.kind == ClientKind::ClaudeCode,
    );
    tracing::warn!(
        protocol,
        model = %request.model,
        source_client = ?profile.kind,
        empty_output_class = class.as_str(),
        attempt,
        max_attempts,
        short_request_kind = short_request_kind.as_str(),
        prompt_hash = %format_args!("{:016x}", shape.prompt_hash),
        prompt_tokens = shape.estimated_total_tokens,
        message_count = shape.message_count,
        max_tokens = ?shape.max_tokens,
        finish_reason = ?collected.finish_reason,
        reasoning_chars = collected.reasoning.len(),
        content_chars = collected.content.len(),
        tool_call_count = collected.tool_calls.len(),
        "upstream returned no assistant content or tool call"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_profile::ClientProfileSource;
    use crate::error::UpstreamErrorKind;
    use crate::protocol::types::{Message, OpenAITool, OpenAIToolFunction};
    use serde_json::Value;

    fn request_with_tool_choice(tool_choice: Option<Value>) -> ChatRequest {
        ChatRequest {
            model: "deepseek-v4-flash".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: Value::String("call the selected tool".to_string()),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            stream: Some(true),
            max_tokens: Some(512),
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice,
        }
    }

    #[test]
    fn claude_code_forced_tool_choice_keeps_thinking_enabled() {
        let request = request_with_tool_choice(Some(serde_json::json!({
            "type": "function",
            "function": { "name": "Write" }
        })));
        let mut body = serde_json::json!({});
        let policy = apply_initial_thinking_policy(
            &mut body,
            &request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        );

        assert_eq!(policy, "claude_code_production_default_enabled");
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn claude_code_auto_tool_choice_keeps_default_thinking() {
        let request = request_with_tool_choice(Some(Value::String("auto".to_string())));
        let mut body = serde_json::json!({});
        let policy = apply_initial_thinking_policy(
            &mut body,
            &request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        );

        assert_eq!(policy, "claude_code_production_default_enabled");
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn unknown_forced_tool_choice_keeps_default_thinking() {
        let request = request_with_tool_choice(Some(serde_json::json!({
            "type": "function",
            "function": { "name": "Write" }
        })));
        let mut body = serde_json::json!({});
        let policy = apply_initial_thinking_policy(&mut body, &request, ClientProfile::unknown());

        assert_eq!(policy, "keep_default");
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn claude_code_forced_tool_choice_keeps_tool_choice_for_selected_models() {
        for model in ["mimo-v2.5-free", "north-mini-code", "nemotron-3-ultra-free"] {
            let mut request = request_with_tool_choice(Some(serde_json::json!({
                "type": "function",
                "function": { "name": "Bash" }
            })));
            request.model = model.to_string();
            let mut body = serde_json::json!({
                "model": model,
                "tool_choice": request.tool_choice,
            });
            let profile = ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header);

            let policy = apply_initial_thinking_policy(&mut body, &request, profile);
            let tool_choice_policy = downgrade_claude_code_forced_tool_choice_for_upstream_model(
                &mut body,
                &mut request,
                profile,
                model,
            );

            assert_eq!(policy, "claude_code_production_default_enabled");
            assert!(body.get("thinking").is_none());
            assert_eq!(tool_choice_policy, None);
            assert_eq!(
                body["tool_choice"],
                serde_json::json!({"type":"function","function":{"name":"Bash"}})
            );
        }
    }

    #[test]
    fn claude_code_forced_tool_choice_keeps_for_other_models() {
        let forced = serde_json::json!({
            "type": "function",
            "function": { "name": "Bash" }
        });
        let mut request = request_with_tool_choice(Some(forced.clone()));
        request.model = "deepseek-v4-flash-free".to_string();
        let mut body = serde_json::json!({
            "model": "deepseek-v4-flash-free",
            "tool_choice": forced,
        });

        let tool_choice_policy = downgrade_claude_code_forced_tool_choice_for_upstream_model(
            &mut body,
            &mut request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
            "deepseek-v4-flash-free",
        );

        assert_eq!(tool_choice_policy, None);
        assert_eq!(
            body["tool_choice"],
            serde_json::json!({"type":"function","function":{"name":"Bash"}})
        );
    }

    fn provider_invalid_error() -> AppError {
        AppError {
            status: axum::http::StatusCode::BAD_REQUEST,
            message: "upstream provider error (status=400, code=invalid_request_error)".to_string(),
            upstream_headers: None,
            upstream_error_kind: Some(UpstreamErrorKind::ProviderInvalidRequest),
        }
    }

    fn missing_reasoning_error() -> AppError {
        AppError {
            status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            message: "upstream provider rejected transformed tool-history request (code=provider_missing_reasoning_content)".to_string(),
            upstream_headers: None,
            upstream_error_kind: Some(UpstreamErrorKind::MissingReasoningContent),
        }
    }

    fn repaired_claude_code_nonstream_tool_request() -> ChatRequest {
        ChatRequest {
            model: "deepseek-v4-flash".to_string(),
            messages: vec![
                Message {
                    role: "user".to_string(),
                    content: Value::String("x".repeat(14 * 1024)),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
                Message {
                    role: "user".to_string(),
                    content: Value::String("now continue".to_string()),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
            ],
            stream: Some(false),
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: Some(vec![OpenAITool {
                tool_type: "function".to_string(),
                function: OpenAIToolFunction {
                    name: "TodoWrite".to_string(),
                    description: Some("todo".to_string()),
                    parameters: Some(serde_json::json!({
                        "type": "array",
                        "properties": [],
                        "required": ["items", "missing"]
                    })),
                },
            }]),
            tool_choice: Some(Value::String("auto".to_string())),
        }
    }

    #[test]
    fn provider_invalid_retries_repaired_claude_code_tool_history() {
        let request = repaired_claude_code_nonstream_tool_request();
        let repair = translate::ToolHistoryRepair {
            downgraded_tool_results: 1,
            ..Default::default()
        };
        let mode = provider_invalid_tool_history_retry_mode(
            &provider_invalid_error(),
            &request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
            repair,
            false,
            false,
        );

        assert_eq!(mode, Some(ProviderInvalidRetryMode::EnrichReasoning));
    }

    #[test]
    fn provider_invalid_retry_body_enriches_without_disabling_thinking() {
        let request = repaired_claude_code_nonstream_tool_request();
        let body = serde_json::json!({
            "messages": request.messages,
            "tools": request.tools,
            "tool_choice": request.tool_choice,
            "thinking": {"type":"enabled"}
        });

        let (retry, stats) = provider_invalid_tool_history_retry_body(
            &body,
            ProviderInvalidRetryMode::EnrichReasoning,
        );

        assert_eq!(retry["thinking"], serde_json::json!({"type":"enabled"}));
        assert!(retry["tools"].is_array());
        assert_eq!(
            retry["tools"][0]["function"]["parameters"]["type"],
            Value::String("object".to_string())
        );
        assert_eq!(
            retry["tools"][0]["function"]["parameters"]["properties"],
            serde_json::json!({})
        );
        assert!(retry["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("provider-invalid retry"));
        assert!(stats.sanitized_tools > 0);
        assert_eq!(stats.compacted_user_messages, 1);
        assert!(!stats.stripped_tools);
    }

    #[test]
    fn provider_invalid_text_only_retry_strips_tools() {
        let request = repaired_claude_code_nonstream_tool_request();
        let body = serde_json::json!({
            "messages": request.messages,
            "tools": request.tools,
            "tool_choice": request.tool_choice
        });

        let (retry, stats) =
            provider_invalid_tool_history_retry_body(&body, ProviderInvalidRetryMode::TextOnly);

        assert!(retry["tools"].is_null());
        assert!(retry["tool_choice"].is_null());
        assert!(stats.stripped_tools);
    }

    #[test]
    fn provider_invalid_text_only_retry_flattens_tool_history() {
        let mut request = repaired_claude_code_nonstream_tool_request();
        request.messages.push(Message {
            role: "assistant".to_string(),
            content: Value::Null,
            tool_calls: Some(vec![crate::protocol::types::ToolCall {
                id: Some("call_1".to_string()),
                call_type: "function".to_string(),
                function: crate::protocol::types::ToolFunction {
                    name: "Read".to_string(),
                    arguments: r#"{"file_path":"README.md"}"#.to_string(),
                },
                index: Some(0),
            }]),
            tool_call_id: None,
            reasoning_content: None,
        });
        let body = serde_json::json!({
            "messages": [
                request.messages[0],
                request.messages[1],
                request.messages[2],
                {
                    "role": "tool",
                    "tool_call_id": "call_1",
                    "name": "Read",
                    "content": "README contents"
                }
            ],
            "tools": request.tools,
            "tool_choice": request.tool_choice
        });

        let (retry, stats) =
            provider_invalid_tool_history_retry_body(&body, ProviderInvalidRetryMode::TextOnly);

        assert!(retry["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|message| message.get("tool_calls").is_none()));
        assert!(retry["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|message| message.get("role").and_then(Value::as_str) != Some("tool")));
        assert!(retry["messages"][2]["content"]
            .as_str()
            .unwrap()
            .contains("previous assistant requested tool Read"));
        assert_eq!(retry["messages"][3]["role"], Value::String("user".to_string()));
        assert!(retry["messages"][3].get("tool_call_id").is_none());
        assert_eq!(stats.flattened_tool_history_messages, 2);
    }

    #[test]
    fn provider_invalid_retries_repaired_claude_code_stream_tool_history() {
        let mut request = repaired_claude_code_nonstream_tool_request();
        request.stream = Some(true);
        let mode = provider_invalid_tool_history_retry_mode(
            &provider_invalid_error(),
            &request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
            translate::ToolHistoryRepair {
                downgraded_tool_results: 1,
                ..Default::default()
            },
            false,
            false,
        );

        assert_eq!(mode, Some(ProviderInvalidRetryMode::EnrichReasoning));
    }

    #[test]
    fn missing_reasoning_retries_unrepaired_claude_code_tool_history() {
        let mut request = repaired_claude_code_nonstream_tool_request();
        request.messages.push(Message {
            role: "assistant".to_string(),
            content: Value::Null,
            tool_calls: Some(vec![crate::protocol::types::ToolCall {
                id: Some("call_1".to_string()),
                call_type: "function".to_string(),
                function: crate::protocol::types::ToolFunction {
                    name: "Read".to_string(),
                    arguments: r#"{"file_path":"docs/OPERATING_RULES.md"}"#.to_string(),
                },
                index: Some(0),
            }]),
            tool_call_id: None,
            reasoning_content: None,
        });

        let mode = provider_invalid_tool_history_retry_mode(
            &missing_reasoning_error(),
            &request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
            translate::ToolHistoryRepair::default(),
            false,
            false,
        );

        assert_eq!(mode, Some(ProviderInvalidRetryMode::EnrichReasoning));
    }

    #[test]
    fn missing_reasoning_uses_text_only_after_enrichment_retry() {
        let mut request = repaired_claude_code_nonstream_tool_request();
        request.messages.push(Message {
            role: "assistant".to_string(),
            content: Value::Null,
            tool_calls: Some(vec![crate::protocol::types::ToolCall {
                id: Some("call_1".to_string()),
                call_type: "function".to_string(),
                function: crate::protocol::types::ToolFunction {
                    name: "Read".to_string(),
                    arguments: r#"{"file_path":"docs/OPERATING_RULES.md"}"#.to_string(),
                },
                index: Some(0),
            }]),
            tool_call_id: None,
            reasoning_content: None,
        });

        let mode = provider_invalid_tool_history_retry_mode(
            &missing_reasoning_error(),
            &request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
            translate::ToolHistoryRepair::default(),
            true,
            false,
        );

        assert_eq!(mode, Some(ProviderInvalidRetryMode::TextOnly));
    }
}
