pub mod anthropic;
pub mod markdown;
pub mod openai;
pub mod sse;

use crate::client_profile::{ClientKind, ClientProfile};
use crate::protocol::{translate, types::ChatRequest};

pub(crate) fn log_request_shape(
    protocol: &'static str,
    request: &ChatRequest,
    profile: ClientProfile,
) {
    let shape = translate::request_shape(request);
    let short_request_kind = translate::classify_short_non_stream_request(
        request,
        profile.kind == ClientKind::ClaudeCode,
    );
    tracing::info!(
        protocol,
        model = %request.model,
        source_client = ?profile.kind,
        profile_source = ?profile.source,
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
