use crate::client_profile::{ClientKind, ClientProfile};
use crate::protocol::{translate, types::ChatRequest};
use serde_json::Value;

/// TMCC: Thinking-Max Cache-Coherence — production ClaudeCode paths keep thinking enabled.
pub fn apply_thinking_manifest(
    body: &mut Value,
    request: &ChatRequest,
    profile: ClientProfile,
) -> &'static str {
    if profile.kind == ClientKind::ClaudeCode && is_production_claude_code_request(request, profile)
    {
        return apply_claude_code_production_thinking(body);
    }

    if profile.disables_thinking_for_tool_use()
        && !preserves_thinking_for_cache_model(&request.model)
    {
        return if translate::disable_thinking_for_tool_use(body) {
            "compat_tool_use_disabled"
        } else {
            "compat_tool_use_keep_existing"
        };
    }

    let short_kind = translate::classify_short_non_stream_request(
        request,
        profile.kind == ClientKind::ClaudeCode,
    );
    let low_output_budget = request
        .max_tokens
        .is_some_and(|max_tokens| max_tokens <= 512);
    let no_tools = request.tools.as_ref().is_none_or(|tools| tools.is_empty())
        && request
            .tool_choice
            .as_ref()
            .is_none_or(|choice| choice.is_null());
    let tiny_prompt = translate::request_shape(request).estimated_total_tokens <= 512;

    let probe_only = no_tools
        && low_output_budget
        && matches!(
            short_kind,
            translate::ShortNonStreamRequestKind::HealthProbe
                | translate::ShortNonStreamRequestKind::ChannelTest
                | translate::ShortNonStreamRequestKind::InternalClaudeCodeProbe
        )
        || (request.stream.unwrap_or(false)
            && profile.kind == ClientKind::ClaudeCode
            && tiny_prompt
            && no_tools);

    if probe_only && translate::set_thinking_disabled_if_absent(body) {
        return "probe_only_disabled";
    }

    if body.get("thinking").is_some() {
        "keep_existing"
    } else {
        "keep_default"
    }
}

fn preserves_thinking_for_cache_model(model: &str) -> bool {
    matches!(
        translate::normalize_model(model).as_str(),
        "deepseek-v4-flash" | "deepseek-v4-flash-free" | "big-pickle"
    )
}

fn apply_claude_code_production_thinking(body: &mut Value) -> &'static str {
    if body.get("thinking").is_some() {
        "claude_code_production_keep_existing"
    } else {
        "claude_code_production_default_enabled"
    }
}

fn is_production_claude_code_request(request: &ChatRequest, profile: ClientProfile) -> bool {
    if profile.kind != ClientKind::ClaudeCode {
        return false;
    }
    let short_kind = translate::classify_short_non_stream_request(request, true);
    !matches!(
        short_kind,
        translate::ShortNonStreamRequestKind::HealthProbe
            | translate::ShortNonStreamRequestKind::ChannelTest
            | translate::ShortNonStreamRequestKind::InternalClaudeCodeProbe
    )
}

pub fn preserves_thinking_on_retry(profile: ClientProfile) -> bool {
    profile.kind == ClientKind::ClaudeCode
}

pub fn reasoning_enriched_retry_body(body: &Value) -> Value {
    let mut retry = body.clone();
    let model = retry
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let session_scope = crate::canonical::tools_epoch_key(model, "retry");
    if let Some(messages) = retry.get_mut("messages").and_then(Value::as_array_mut) {
        let mut typed = messages
            .iter()
            .filter_map(|value| {
                serde_json::from_value::<crate::protocol::types::Message>(value.clone()).ok()
            })
            .collect::<Vec<_>>();
        crate::canonical::enrich_messages_with_reasoning_mode(
            &mut typed,
            &session_scope,
            crate::canonical::ReasoningEnrichMode::CurrentTurnOnly,
        );
        *messages = typed
            .into_iter()
            .map(|message| crate::canonical::message_to_upstream_json(&message))
            .collect();
    }
    retry
}
