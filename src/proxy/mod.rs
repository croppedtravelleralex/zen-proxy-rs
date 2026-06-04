pub mod anthropic;
pub mod markdown;
pub mod openai;
pub mod sse;

use crate::client_profile::{ClientKind, ClientProfile};
use crate::protocol::{translate, types::ChatRequest};
use crate::zen::client::ProviderCacheSignals;

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
        estimated_total_tokens = shape.estimated_total_tokens,
        "provider cache usage observation"
    );
}
