pub mod anthropic;
pub mod markdown;
pub mod openai;
pub mod sse;

use crate::client_profile::{ClientKind, ClientProfile};
use crate::protocol::{translate, types::ChatRequest};
use crate::zen::client::{CollectedStream, ProviderCacheSignals};

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

    let should_disable = no_tools
        && low_output_budget
        && (matches!(
            short_kind,
            translate::ShortNonStreamRequestKind::HealthProbe
                | translate::ShortNonStreamRequestKind::ChannelTest
                | translate::ShortNonStreamRequestKind::InternalClaudeCodeProbe
        ) || (request.stream.unwrap_or(false)
            && profile.kind == ClientKind::ClaudeCode
            && tiny_prompt));

    if should_disable && translate::set_thinking_disabled_if_absent(body) {
        return "low_budget_probe_disabled";
    }
    if body.get("thinking").is_some() {
        "keep_existing"
    } else {
        "keep_default"
    }
}

pub(crate) fn reasoning_disabled_retry_body(body: &serde_json::Value) -> serde_json::Value {
    let mut retry = body.clone();
    translate::set_thinking_disabled_if_absent(&mut retry);
    retry
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
