pub mod anthropic;
pub mod markdown;
pub mod openai;
pub mod sse;

use crate::client_profile::{ClientKind, ClientProfile};
use crate::error::AppError;
use crate::protocol::{translate, types::ChatRequest};
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

    pub(crate) const fn should_retry_with_disabled_thinking(self) -> bool {
        matches!(self, Self::ReasoningOnlyLength)
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
    if profile.disables_thinking_for_tool_use() {
        return if translate::disable_thinking_for_tool_use(body) {
            "compat_tool_use_disabled"
        } else {
            "compat_tool_use_keep_existing"
        };
    }

    let shape = translate::request_shape(request);
    let short_kind = translate::classify_short_non_stream_request(
        request,
        profile.kind == ClientKind::ClaudeCode,
    );
    let low_output_budget = request
        .max_tokens
        .is_some_and(|max_tokens| max_tokens <= 512);
    let no_tools = shape.tool_count == 0 && !shape.tool_choice_present;
    let tiny_prompt = shape.estimated_total_tokens <= 512;
    let claude_code_forced_tool_choice = profile.kind == ClientKind::ClaudeCode
        && is_forced_tool_choice(request.tool_choice.as_ref());
    let low_budget_tool_probe = translate::is_claude_code_low_budget_tool_probe(
        request,
        profile.kind == ClientKind::ClaudeCode,
    );
    let claude_code_large_stream_tool_request = profile.kind == ClientKind::ClaudeCode
        && request.stream.unwrap_or(false)
        && !no_tools
        && shape.estimated_total_tokens >= 80_000;

    let should_disable = low_budget_tool_probe
        || claude_code_forced_tool_choice
        || claude_code_large_stream_tool_request
        || (no_tools
            && low_output_budget
            && (matches!(
                short_kind,
                translate::ShortNonStreamRequestKind::HealthProbe
                    | translate::ShortNonStreamRequestKind::ChannelTest
                    | translate::ShortNonStreamRequestKind::InternalClaudeCodeProbe
            ) || (request.stream.unwrap_or(false)
                && profile.kind == ClientKind::ClaudeCode
                && tiny_prompt)));

    if should_disable && translate::set_thinking_disabled_if_absent(body) {
        return if low_budget_tool_probe {
            "low_budget_tool_probe_disabled"
        } else if claude_code_forced_tool_choice {
            "claude_code_forced_tool_choice_disabled"
        } else if claude_code_large_stream_tool_request {
            "claude_code_large_stream_tool_request_disabled"
        } else {
            "low_budget_probe_disabled"
        };
    }
    if body.get("thinking").is_some() {
        "keep_existing"
    } else {
        "keep_default"
    }
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

fn is_forced_tool_choice(tool_choice: Option<&serde_json::Value>) -> bool {
    let Some(choice) = tool_choice else {
        return false;
    };
    if choice.is_null() {
        return false;
    }
    if choice.as_str().is_some_and(|value| value == "auto") {
        return false;
    }
    true
}

pub(crate) fn reasoning_disabled_retry_body(body: &serde_json::Value) -> serde_json::Value {
    let mut retry = body.clone();
    retry["thinking"] = json!({"type":"disabled"});
    retry
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderInvalidRetryMode {
    DisableThinking,
    TextOnly,
}

impl ProviderInvalidRetryMode {
    const fn strips_tools(self) -> bool {
        matches!(self, Self::TextOnly)
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::DisableThinking => "disable_thinking",
            Self::TextOnly => "text_only",
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProviderInvalidRetryStats {
    pub sanitized_tools: usize,
    pub compacted_user_messages: usize,
    pub stripped_tools: bool,
}

pub(crate) fn provider_invalid_tool_history_retry_mode(
    err: &AppError,
    request: &ChatRequest,
    profile: ClientProfile,
    repair: translate::ToolHistoryRepair,
    used_disabled_thinking_retry: bool,
    used_text_only_retry: bool,
) -> Option<ProviderInvalidRetryMode> {
    if !(err.is_provider_invalid_request() || err.is_missing_reasoning_content())
        || !is_risky_claude_code_tool_history_request(request, profile, repair)
    {
        return None;
    }
    if !used_disabled_thinking_retry {
        return Some(ProviderInvalidRetryMode::DisableThinking);
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
    let mut retry = reasoning_disabled_retry_body(body);
    let sanitized_tools = sanitize_upstream_tools(&mut retry);
    let compacted_user_messages = compact_large_user_messages_for_retry(&mut retry);
    if mode.strips_tools() {
        retry["tools"] = Value::Null;
        retry["tool_choice"] = Value::Null;
    }
    (
        retry,
        ProviderInvalidRetryStats {
            sanitized_tools,
            compacted_user_messages,
            stripped_tools: mode.strips_tools(),
        },
    )
}

fn is_risky_claude_code_tool_history_request(
    request: &ChatRequest,
    profile: ClientProfile,
    repair: translate::ToolHistoryRepair,
) -> bool {
    profile.kind == ClientKind::ClaudeCode
        && request
            .tools
            .as_ref()
            .is_some_and(|tools| !tools.is_empty())
        && (repair.downgraded_tool_results > 0 || repair.downgraded_assistant_calls > 0)
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
    used_disabled_thinking_retry: bool,
) -> bool {
    err.is_missing_reasoning_content() && !used_disabled_thinking_retry
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
        "retrying upstream missing reasoning_content error with disabled thinking"
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
    fn claude_code_forced_tool_choice_disables_thinking() {
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

        assert_eq!(policy, "claude_code_forced_tool_choice_disabled");
        assert_eq!(body["thinking"], serde_json::json!({"type":"disabled"}));
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

        assert_eq!(policy, "keep_default");
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

    fn provider_invalid_error() -> AppError {
        AppError {
            status: axum::http::StatusCode::BAD_REQUEST,
            message: "upstream provider error (status=400, code=invalid_request_error)".to_string(),
            upstream_headers: None,
            upstream_error_kind: Some(UpstreamErrorKind::ProviderInvalidRequest),
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
                },
                Message {
                    role: "user".to_string(),
                    content: Value::String("now continue".to_string()),
                    tool_calls: None,
                    tool_call_id: None,
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

        assert_eq!(mode, Some(ProviderInvalidRetryMode::DisableThinking));
    }

    #[test]
    fn provider_invalid_retry_body_sanitizes_without_stripping_tools_first() {
        let request = repaired_claude_code_nonstream_tool_request();
        let body = serde_json::json!({
            "messages": request.messages,
            "tools": request.tools,
            "tool_choice": request.tool_choice,
            "thinking": {"type":"enabled"}
        });

        let (retry, stats) = provider_invalid_tool_history_retry_body(
            &body,
            ProviderInvalidRetryMode::DisableThinking,
        );

        assert_eq!(retry["thinking"], serde_json::json!({"type":"disabled"}));
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

        assert_eq!(mode, Some(ProviderInvalidRetryMode::DisableThinking));
    }
}
