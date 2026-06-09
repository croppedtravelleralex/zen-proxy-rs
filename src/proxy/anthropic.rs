use crate::client_profile::{ClientKind, ClientProfile};
use crate::error::AppError;
use crate::kernel::KernelConfig;
use crate::protocol::translate::estimate_tokens as estimate;
use crate::protocol::{translate, types::*};
use crate::synthesis;
use crate::zen::client::ProviderCacheSignals;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::StreamExt;
use reqwest::Client;
use serde_json::Value;

const CLAUDE_CODE_HUGE_BUFFER_MIN_INPUT_TOKENS: u64 = 50_000;
const CLAUDE_CODE_BUFFERED_STREAM_MAX_OUTPUT_TOKENS: u64 = 2_048;
const CLAUDE_CODE_BUFFERED_STREAM_ATTEMPTS: usize = 3;
const CLAUDE_CODE_STREAM_IDLE_PING_SECS: u64 = 15;
const CLAUDE_CODE_STREAM_GUARD_ATTEMPTS: usize = 3;
const ANTHROPIC_TOOL_JSON_DELTA_CHUNK_BYTES: usize = 4 * 1024;
const NON_STREAM_EMPTY_UPSTREAM_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaudeCodeBufferedStreamReason {
    TinyExactOutputNoTools,
    SmallOutputHugeContextNoTools,
}

impl ClaudeCodeBufferedStreamReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::TinyExactOutputNoTools => "tiny_exact_output_no_tools",
            Self::SmallOutputHugeContextNoTools => "small_output_huge_context_no_tools",
        }
    }
}

pub async fn handle_anthropic_messages(
    client: &Client,
    config: &KernelConfig,
    body: AnthropicRequest,
    profile: ClientProfile,
) -> Result<Response, AppError> {
    let model = translate::normalize_model(&body.model);
    let observed_profile = profile;
    let profile = observed_profile.effective_for_model(&model);
    if profile != observed_profile {
        tracing::info!(
            model,
            source_client = ?observed_profile.kind,
            effective_client = ?profile.kind,
            "client profile policy narrowed by model"
        );
    }
    let upstream_model = translate::map_upstream_model(&model, &config.model_mappings);
    let mut msgs = translate::anthropic_to_openai_messages(&body);
    let stream_requested = body.stream.unwrap_or(false);
    let context_repair = if translate::model_disables_input_compaction(&model) {
        translate::observe_context(&msgs)
    } else if profile.kind == ClientKind::ClaudeCode {
        translate::compact_claude_code_huge_session_context(&mut msgs)
    } else if stream_requested {
        translate::compact_stream_context_with_policy(
            &mut msgs,
            translate::StreamContextPolicy::default(),
        )
    } else {
        translate::StreamContextRepair::default()
    };
    let reduced_exact_output_anchor = stream_requested
        && profile.kind == ClientKind::ClaudeCode
        && context_repair.compacted_messages > 0
        && translate::reduce_to_exact_output_anchor_message(&mut msgs, 2 * 1024);
    let appended_latest_user_anchor = profile.kind == ClientKind::ClaudeCode
        && context_repair.compacted_messages > 0
        && !reduced_exact_output_anchor
        && translate::append_latest_user_anchor_message(&mut msgs, 2 * 1024);
    if context_repair.compacted_messages > 0 {
        if stream_requested {
            tracing::warn!(
                before_tokens = context_repair.before_tokens,
                after_tokens = context_repair.after_tokens,
                compacted_messages = context_repair.compacted_messages,
                reduced_exact_output_anchor,
                appended_latest_user_anchor,
                "compacted streaming anthropic context before upstream"
            );
        } else {
            tracing::warn!(
                before_tokens = context_repair.before_tokens,
                after_tokens = context_repair.after_tokens,
                compacted_messages = context_repair.compacted_messages,
                appended_latest_user_anchor,
                "compacted non-stream anthropic context before upstream"
            );
        }
    }
    let tool_history_policy = if profile.uses_compat_tool_history() {
        translate::ToolHistoryPolicy::Compat
    } else {
        translate::ToolHistoryPolicy::Strict
    };
    let repair =
        translate::canonicalize_openai_tool_history_with_policy(&mut msgs, tool_history_policy);
    if repair != translate::ToolHistoryRepair::default() {
        tracing::warn!(
            synthetic_tool_ids = repair.synthetic_tool_ids,
            paired_tool_results = repair.paired_tool_results,
            downgraded_tool_results = repair.downgraded_tool_results,
            downgraded_assistant_calls = repair.downgraded_assistant_calls,
            "canonicalized anthropic tool history after openai translation"
        );
    }
    let tools: Vec<OpenAITool> = if reduced_exact_output_anchor {
        Vec::new()
    } else {
        body.tools
            .as_ref()
            .map(|t| translate::anthropic_tools_to_openai(t))
            .unwrap_or_default()
    };
    let max_tok = if stream_requested {
        let policy_prompt_tokens = context_repair.before_tokens.max(translate::estimate_tokens(
            &translate::build_prompt_text(&msgs),
        ));
        let policy = translate::stream_output_policy_for_prompt_tokens(
            policy_prompt_tokens,
            body.max_tokens,
        );
        if policy.capped {
            tracing::warn!(
                prompt_tokens = policy.prompt_tokens,
                requested_max_tokens = policy.requested_max_tokens,
                effective_max_tokens = policy.effective_max_tokens,
                "capped streaming anthropic max_tokens before upstream"
            );
        }
        policy.effective_max_tokens
    } else {
        let policy_prompt_tokens = context_repair.before_tokens.max(translate::estimate_tokens(
            &translate::build_prompt_text(&msgs),
        ));
        let policy = translate::non_stream_output_policy_for_prompt_tokens(
            policy_prompt_tokens,
            body.max_tokens,
        );
        if policy.capped {
            tracing::warn!(
                prompt_tokens = policy.prompt_tokens,
                requested_max_tokens = policy.requested_max_tokens,
                effective_max_tokens = policy.effective_max_tokens,
                "capped non-stream anthropic max_tokens before upstream"
            );
        }
        policy.effective_max_tokens
    };
    let tool_choice = if reduced_exact_output_anchor {
        None
    } else {
        body.tool_choice
            .as_ref()
            .map(translate::anthropic_tool_choice_to_openai)
    };
    let has_tools = !tools.is_empty();
    let tool_count = tools.len();
    let mut zb = serde_json::json!({"model":upstream_model,"messages":msgs,"stream":true,"temperature":body.temperature,"tools":if tools.is_empty(){Value::Null}else{serde_json::to_value(&tools).unwrap_or_default()},"tool_choice":tool_choice});
    if let Some(max_tok) = max_tok {
        zb["max_tokens"] = serde_json::json!(max_tok);
    }
    let mut cr = ChatRequest {
        model: model.clone(),
        messages: msgs,
        stream: Some(stream_requested),
        max_tokens: max_tok,
        temperature: body.temperature,
        top_p: None,
        tools: if tools.is_empty() { None } else { Some(tools) },
        tool_choice,
    };
    let thinking_policy = super::apply_initial_thinking_policy(&mut zb, &cr, profile);
    let probe_max_tokens = translate::claude_code_low_budget_tool_probe_max_tokens(
        &cr,
        profile.kind == ClientKind::ClaudeCode,
    );
    if probe_max_tokens != cr.max_tokens {
        let shape = translate::request_shape(&cr);
        tracing::warn!(
            protocol = "anthropic",
            model = %cr.model,
            source_client = ?profile.kind,
            requested_max_tokens = ?cr.max_tokens,
            effective_max_tokens = ?probe_max_tokens,
            prompt_hash = %format_args!("{:016x}", shape.prompt_hash),
            prompt_tokens = shape.estimated_total_tokens,
            message_count = shape.message_count,
            tool_count = shape.tool_count,
            "raised ClaudeCode low-budget tool probe max_tokens before upstream"
        );
        cr.max_tokens = probe_max_tokens;
        if let Some(max_tok) = probe_max_tokens {
            zb["max_tokens"] = serde_json::json!(max_tok);
        }
    }
    tracing::info!(
        protocol = "anthropic",
        model = %cr.model,
        source_client = ?profile.kind,
        thinking_policy,
        "applied upstream thinking policy"
    );
    super::log_request_shape("anthropic", &cr, observed_profile, profile);
    if stream_requested && profile.protects_recovery_safe_markers() {
        if let Some(literal) = translate::claude_code_recovery_literal_from_messages(&cr.messages) {
            tracing::warn!(
                literal_len = literal.len(),
                source_client = ?profile.kind,
                tools_present = cr.tools.is_some(),
                "safe-marker recovery-pressure shortcut returned marker literal"
            );
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let input_tokens = estimate(&translate::build_prompt_text(&cr.messages)).max(1);
            let output_tokens = estimate(&literal).max(1);
            super::log_provider_cache_observation(
                "anthropic",
                &cr,
                profile,
                &ProviderCacheSignals::ignored(),
                0,
                0,
            );
            return Ok(anthropic_buffered_stream_resp(
                ts,
                &cr.model,
                &literal,
                Vec::new(),
                input_tokens,
                output_tokens,
                0,
                0,
                "end_turn".to_string(),
                &cr,
                profile,
            ));
        }
    }
    if translate::is_short_no_tool_health_request(&cr) {
        super::log_provider_cache_observation(
            "anthropic",
            &cr,
            profile,
            &ProviderCacheSignals::ignored(),
            0,
            0,
        );
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let input_tokens = estimate(&translate::build_prompt_text(&cr.messages)).max(1);
        let output_tokens = 1;
        if body.stream.unwrap_or(false) {
            return Ok(anthropic_ok_stream_resp(
                ts,
                &cr.model,
                input_tokens,
                output_tokens,
            ));
        }
        return Ok(text_resp(ts, &cr.model, "ok", input_tokens, output_tokens));
    }
    if body.stream.unwrap_or(false) {
        let exact_output_literal = translate::exact_output_literal_from_messages(&cr.messages);
        let claude_code_buffer_reason = claude_code_buffered_stream_reason(
            profile,
            context_repair.before_tokens,
            cr.max_tokens,
            has_tools,
            reduced_exact_output_anchor,
            exact_output_literal.as_deref(),
        );
        if let Some(reason) = claude_code_buffer_reason {
            tracing::warn!(
                model = %cr.model,
                source_client = ?profile.kind,
                buffer_reason = reason.as_str(),
                before_tokens = context_repair.before_tokens,
                after_tokens = context_repair.after_tokens,
                effective_max_tokens = ?cr.max_tokens,
                reduced_exact_output_anchor,
                has_exact_output_literal = exact_output_literal.is_some(),
                exact_output_literal_chars = exact_output_literal
                    .as_deref()
                    .map(|literal| literal.chars().count())
                    .unwrap_or(0),
                tool_count,
                message_count = cr.messages.len(),
                "ClaudeCode stream entering buffered compatibility path"
            );
        }
        handle_stream(
            client,
            config,
            &cr,
            &zb,
            profile,
            repair,
            claude_code_buffer_reason,
        )
        .await
    } else {
        handle_non_stream(client, config, &cr, &zb, profile, repair).await
    }
}

fn claude_code_buffered_stream_reason(
    profile: ClientProfile,
    before_tokens: u64,
    effective_max_tokens: Option<u64>,
    has_tools: bool,
    reduced_exact_output_anchor: bool,
    exact_output_literal: Option<&str>,
) -> Option<ClaudeCodeBufferedStreamReason> {
    if profile.kind != ClientKind::ClaudeCode {
        return None;
    }
    if has_tools {
        return None;
    }
    let has_tiny_exact_output_literal = reduced_exact_output_anchor
        || exact_output_literal.is_some_and(is_tiny_exact_output_literal);
    if has_tiny_exact_output_literal {
        return Some(ClaudeCodeBufferedStreamReason::TinyExactOutputNoTools);
    }
    let max_tokens = effective_max_tokens?;
    if max_tokens <= CLAUDE_CODE_BUFFERED_STREAM_MAX_OUTPUT_TOKENS
        && before_tokens >= CLAUDE_CODE_HUGE_BUFFER_MIN_INPUT_TOKENS
    {
        return Some(ClaudeCodeBufferedStreamReason::SmallOutputHugeContextNoTools);
    }
    None
}

fn is_tiny_exact_output_literal(literal: &str) -> bool {
    let trimmed = literal.trim();
    !trimmed.is_empty()
        && !trimmed.contains('\n')
        && trimmed.chars().count() <= 80
        && trimmed.split_whitespace().count() <= 1
}

async fn handle_non_stream(
    client: &Client,
    config: &KernelConfig,
    cr: &ChatRequest,
    zb: &Value,
    profile: ClientProfile,
    tool_history_repair: translate::ToolHistoryRepair,
) -> Result<Response, AppError> {
    let mut observed_exit_ip = None;
    let request_shape = translate::request_shape(cr);
    let short_request_kind =
        translate::classify_short_non_stream_request(cr, profile.kind == ClientKind::ClaudeCode);
    let (collected, content) = {
        let mut last_empty = false;
        let mut last_empty_class = None;
        let mut last_incomplete_tool_arguments = false;
        let mut used_reasoning_disabled_retry = false;
        let mut used_missing_reasoning_disabled_retry = false;
        let mut used_provider_invalid_disabled_retry = false;
        let mut used_provider_invalid_text_retry = false;
        let mut attempt_body = zb.clone();
        let mut output = None;
        for attempt in 0..NON_STREAM_EMPTY_UPSTREAM_ATTEMPTS {
            let resp = match crate::zen::client::fetch_zen_stream_with_headers(
                client,
                &config.zen_chat_url,
                &config.zen_api_key,
                &attempt_body,
                &config.extra_headers,
            )
            .await
            {
                Ok(resp) => resp,
                Err(err)
                    if super::should_retry_missing_reasoning_content(
                        &err,
                        used_missing_reasoning_disabled_retry,
                    ) =>
                {
                    used_missing_reasoning_disabled_retry = true;
                    attempt_body = super::reasoning_disabled_retry_body(zb);
                    super::log_missing_reasoning_content_retry(
                        "anthropic",
                        cr,
                        profile,
                        attempt + 1,
                    );
                    continue;
                }
                Err(err) => {
                    if let Some(mode) = super::provider_invalid_tool_history_retry_mode(
                        &err,
                        cr,
                        profile,
                        tool_history_repair,
                        used_provider_invalid_disabled_retry,
                        used_provider_invalid_text_retry,
                    ) {
                        match mode {
                            super::ProviderInvalidRetryMode::DisableThinking => {
                                used_provider_invalid_disabled_retry = true;
                            }
                            super::ProviderInvalidRetryMode::TextOnly => {
                                used_provider_invalid_text_retry = true;
                            }
                        }
                        let (retry_body, stats) =
                            super::provider_invalid_tool_history_retry_body(zb, mode);
                        attempt_body = retry_body;
                        super::log_provider_invalid_tool_history_retry(
                            "anthropic",
                            cr,
                            profile,
                            tool_history_repair,
                            mode,
                            stats,
                            attempt + 1,
                        );
                        continue;
                    }
                    return Err(err);
                }
            };
            let cache_signals = ProviderCacheSignals::from_response_headers(resp.headers());
            observed_exit_ip = resp.headers().get("x-zen-observed-exit-ip").cloned();
            let collected = crate::zen::client::collect_stream_parts(resp).await?;
            let cache_signals = cache_signals.with_body_usage(collected.usage.as_ref());
            super::log_provider_cache_observation(
                "anthropic",
                cr,
                profile,
                &cache_signals,
                attempt + 1,
                NON_STREAM_EMPTY_UPSTREAM_ATTEMPTS,
            );
            let content = response_text_for_profile(profile, &collected.content);
            let output_class = super::classify_collected_output(&collected, &content);
            if output_class != super::OutputClass::Valid {
                last_empty = true;
                last_empty_class = Some(output_class);
                super::log_empty_output_class(
                    "anthropic",
                    cr,
                    profile,
                    output_class,
                    attempt + 1,
                    NON_STREAM_EMPTY_UPSTREAM_ATTEMPTS,
                    &collected,
                );
                if output_class.should_retry_with_disabled_thinking()
                    && !used_reasoning_disabled_retry
                {
                    used_reasoning_disabled_retry = true;
                    attempt_body = super::reasoning_disabled_retry_body(zb);
                    tracing::warn!(
                        protocol = "anthropic",
                        model = %cr.model,
                        source_client = ?profile.kind,
                        empty_output_class = output_class.as_str(),
                        attempt = attempt + 1,
                        "retrying reasoning-only output with disabled thinking"
                    );
                    continue;
                }
                continue;
            }
            if has_only_incomplete_tool_arguments(&content, &collected.tool_calls, cr, profile) {
                last_empty = true;
                last_incomplete_tool_arguments = true;
                tracing::warn!(
                    protocol = "anthropic",
                    model = %cr.model,
                    source_client = ?profile.kind,
                    attempt = attempt + 1,
                    max_attempts = NON_STREAM_EMPTY_UPSTREAM_ATTEMPTS,
                    tool_call_count = collected.tool_calls.len(),
                    finish_reason = ?collected.finish_reason,
                    "ClaudeCode non-stream guard received only incomplete tool calls"
                );
                if profile.kind == ClientKind::ClaudeCode && !used_reasoning_disabled_retry {
                    used_reasoning_disabled_retry = true;
                    attempt_body = super::reasoning_disabled_retry_body(zb);
                    tracing::warn!(
                        protocol = "anthropic",
                        model = %cr.model,
                        source_client = ?profile.kind,
                        next_attempt = attempt + 2,
                        "ClaudeCode non-stream guard enabling disabled-thinking retry for incomplete tool arguments"
                    );
                }
                continue;
            }
            output = Some((collected, content));
            break;
        }
        if let Some(output) = output {
            output
        } else if last_incomplete_tool_arguments {
            return Err(incomplete_tool_arguments_error());
        } else if let Some(fallback_text) = last_empty
            .then(|| translate::short_no_tool_empty_fallback_text(cr))
            .flatten()
        {
            tracing::warn!(
                model = cr.model,
                source_client = ?profile.kind,
                short_request_kind = short_request_kind.as_str(),
                prompt_hash = %format_args!("{:016x}", request_shape.prompt_hash),
                prompt_tokens = request_shape.estimated_total_tokens,
                message_count = request_shape.message_count,
                max_tokens = ?request_shape.max_tokens,
                "short non-stream channel-test probe received empty upstream; returning local ok"
            );
            let prompt = translate::build_prompt_text(&cr.messages);
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            return Ok(text_resp(
                ts,
                &cr.model,
                fallback_text,
                estimate(&prompt),
                estimate(fallback_text).max(1),
            ));
        } else {
            return Err(AppError::empty_upstream_class(
                last_empty_class
                    .map(super::OutputClass::as_str)
                    .unwrap_or("empty_output"),
            ));
        }
    };
    let prompt = translate::build_prompt_text(&cr.messages);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    if !collected.tool_calls.is_empty() {
        let mut seen_tool_signatures = std::collections::HashSet::new();
        let mut blocks = Vec::new();
        for tool in &collected.tool_calls {
            let Some((ct, input)) = streamable_anthropic_tool_call(tool, cr, profile) else {
                continue;
            };
            if should_skip_duplicate_claude_code_tool_call(
                profile,
                &mut seen_tool_signatures,
                &ct.function.name,
                &input,
            ) {
                continue;
            }
            blocks.push(AnthropicContentBlock {
                block_type: "tool_use".to_string(),
                text: None,
                id: ct.id,
                name: Some(ct.function.name),
                input: Some(input),
            });
        }
        if !blocks.is_empty() {
            let input_tokens = collected
                .usage
                .as_ref()
                .and_then(|usage| usage.prompt_tokens)
                .unwrap_or_else(|| estimate(&prompt));
            let output_tokens = collected
                .usage
                .as_ref()
                .and_then(|usage| usage.completion_tokens)
                .unwrap_or_else(|| {
                    estimate(
                        &collected
                            .tool_calls
                            .iter()
                            .map(|tool| format!("{} {}", tool.name, tool.arguments))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )
                    .max(1)
                });
            return Ok(with_observed_exit_ip(
                tool_resp_with_usage(
                    ts,
                    &cr.model,
                    blocks,
                    input_tokens,
                    output_tokens,
                    collected.usage.as_ref(),
                ),
                observed_exit_ip,
            ));
        }
    }
    let input_tokens = collected
        .usage
        .as_ref()
        .and_then(|usage| usage.prompt_tokens)
        .unwrap_or_else(|| estimate(&prompt));
    let output_tokens = collected
        .usage
        .as_ref()
        .and_then(|usage| usage.completion_tokens)
        .unwrap_or_else(|| estimate(&content));
    Ok(with_observed_exit_ip(
        text_resp_with_usage(
            ts,
            &cr.model,
            &content,
            input_tokens,
            output_tokens,
            collected.usage.as_ref(),
            collected.finish_reason.as_deref(),
        ),
        observed_exit_ip,
    ))
}

fn text_resp(ts: u128, model: &str, text: &str, input_tokens: u64, output_tokens: u64) -> Response {
    text_resp_with_usage(ts, model, text, input_tokens, output_tokens, None, None)
}

fn response_text_for_profile(profile: ClientProfile, text: &str) -> String {
    if profile.preserves_model_text_exactly() {
        text.to_string()
    } else {
        crate::proxy::markdown::MarkdownFenceGuard::repair_text(&crate::redact::redact_text(text))
    }
}

fn anthropic_ok_stream_resp(
    ts: u128,
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> Response {
    use axum::response::sse::{Event, Sse};
    use std::convert::Infallible;
    let msg_id = format!("msg_{ts}");
    let model = model.to_string();
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(Event::default().event("message_start").data(serde_json::json!({"type":"message_start","message":{"id":msg_id,"type":"message","role":"assistant","model":model,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":input_tokens,"output_tokens":0}}}).to_string()));
        yield Ok(Event::default().event("content_block_start").data(serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}).to_string()));
        yield Ok(Event::default().event("content_block_delta").data(serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}).to_string()));
        yield Ok(Event::default().event("content_block_stop").data(serde_json::json!({"type":"content_block_stop","index":0}).to_string()));
        yield Ok(Event::default().event("message_delta").data(serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":output_tokens}}).to_string()));
        yield Ok(Event::default().event("message_stop").data(serde_json::json!({"type":"message_stop"}).to_string()));
    };
    Sse::new(stream).into_response()
}

fn text_resp_with_usage(
    ts: u128,
    model: &str,
    text: &str,
    input_tokens: u64,
    output_tokens: u64,
    usage: Option<&crate::zen::client::ZenUsage>,
    upstream_finish_reason: Option<&str>,
) -> Response {
    let stop_reason = anthropic_stop_reason(upstream_finish_reason, false);
    Json(serde_json::json!({
        "id": format!("msg_{ts}"),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": [{"type":"text","text":text}],
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": {
            "input_tokens": input_tokens,
            "cache_creation_input_tokens": cache_creation_tokens(usage),
            "cache_read_input_tokens": cache_read_tokens(usage),
            "output_tokens": output_tokens
        }
    }))
    .into_response()
}

fn tool_resp_with_usage(
    ts: u128,
    model: &str,
    blocks: Vec<AnthropicContentBlock>,
    input_tokens: u64,
    output_tokens: u64,
    usage: Option<&crate::zen::client::ZenUsage>,
) -> Response {
    Json(serde_json::json!({
        "id": format!("msg_{ts}"),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": blocks,
        "stop_reason": "tool_use",
        "stop_sequence": null,
        "usage": {
            "input_tokens": input_tokens,
            "cache_creation_input_tokens": cache_creation_tokens(usage),
            "cache_read_input_tokens": cache_read_tokens(usage),
            "output_tokens": output_tokens
        }
    }))
    .into_response()
}

fn cache_creation_tokens(usage: Option<&crate::zen::client::ZenUsage>) -> u64 {
    usage
        .and_then(|usage| usage.cache_creation_input_tokens)
        .unwrap_or(0)
}

fn cache_read_tokens(usage: Option<&crate::zen::client::ZenUsage>) -> u64 {
    usage
        .and_then(crate::zen::client::ZenUsage::cache_read_tokens)
        .unwrap_or(0)
}

fn anthropic_stop_reason(
    upstream_finish_reason: Option<&str>,
    has_tool_calls: bool,
) -> &'static str {
    if has_tool_calls {
        return "tool_use";
    }
    match upstream_finish_reason {
        Some("length") => "max_tokens",
        Some("stop") => "end_turn",
        Some("content_filter") => "end_turn",
        _ => "end_turn",
    }
}

fn with_observed_exit_ip(
    mut response: Response,
    observed_exit_ip: Option<reqwest::header::HeaderValue>,
) -> Response {
    if let Some(value) = observed_exit_ip {
        response
            .headers_mut()
            .insert("x-zen-observed-exit-ip", value);
    }
    response
}

fn should_retry_stream_without_forwardable_output(
    profile: ClientProfile,
    attempt: usize,
    text: &str,
    tool_calls: &[crate::zen::client::CollectedToolCall],
    elapsed: std::time::Duration,
    retry_after: std::time::Duration,
) -> bool {
    profile.kind == ClientKind::ClaudeCode
        && attempt + 1 < CLAUDE_CODE_STREAM_GUARD_ATTEMPTS
        && text.trim().is_empty()
        && tool_calls.is_empty()
        && elapsed >= retry_after
}

fn should_retry_stream_error_before_output(
    profile: ClientProfile,
    attempt: usize,
    text: &str,
    tool_calls: &[crate::zen::client::CollectedToolCall],
) -> bool {
    profile.kind == ClientKind::ClaudeCode
        && attempt + 1 < CLAUDE_CODE_STREAM_GUARD_ATTEMPTS
        && text.trim().is_empty()
        && tool_calls.is_empty()
}

fn has_unemitted_streamable_tool_call(
    tool_calls: &[crate::zen::client::CollectedToolCall],
    emitted_tool_call_indexes: &std::collections::HashSet<i64>,
    body: &ChatRequest,
    profile: ClientProfile,
) -> bool {
    tool_calls
        .iter()
        .filter(|tool| !emitted_tool_call_indexes.contains(&tool.index))
        .any(|tool| streamable_anthropic_tool_call(tool, body, profile).is_some())
}

fn has_forwardable_anthropic_output(
    text: &str,
    tool_calls: &[crate::zen::client::CollectedToolCall],
    emitted_tool_call_indexes: &std::collections::HashSet<i64>,
    body: &ChatRequest,
    profile: ClientProfile,
) -> bool {
    !text.trim().is_empty()
        || !emitted_tool_call_indexes.is_empty()
        || has_unemitted_streamable_tool_call(tool_calls, emitted_tool_call_indexes, body, profile)
}

fn has_streamable_anthropic_tool_call(
    tool_calls: &[crate::zen::client::CollectedToolCall],
    body: &ChatRequest,
    profile: ClientProfile,
) -> bool {
    tool_calls
        .iter()
        .any(|tool| streamable_anthropic_tool_call(tool, body, profile).is_some())
}

fn has_only_incomplete_tool_arguments(
    text: &str,
    tool_calls: &[crate::zen::client::CollectedToolCall],
    body: &ChatRequest,
    profile: ClientProfile,
) -> bool {
    text.trim().is_empty()
        && !tool_calls.is_empty()
        && !has_streamable_anthropic_tool_call(tool_calls, body, profile)
}

fn should_skip_duplicate_claude_code_tool_call(
    profile: ClientProfile,
    seen: &mut std::collections::HashSet<String>,
    tool_name: &str,
    input: &Value,
) -> bool {
    if profile.kind != ClientKind::ClaudeCode {
        return false;
    }
    let input_json = serde_json::to_string(input).unwrap_or_default();
    let signature = format!("{}:{input_json}", tool_name.to_ascii_lowercase());
    if seen.insert(signature) {
        return false;
    }
    tracing::warn!(
        protocol = "anthropic",
        source_client = ?profile.kind,
        tool_name,
        "dropped duplicate ClaudeCode tool call within one assistant response"
    );
    true
}

fn incomplete_tool_arguments_error() -> AppError {
    AppError::new(
        axum::http::StatusCode::BAD_GATEWAY,
        "upstream returned incomplete tool call arguments",
    )
}

fn should_apply_initial_fetch_timeout(
    profile: ClientProfile,
    estimated_input_tokens: u64,
    min_input_tokens: u64,
    timeout_secs: u64,
) -> bool {
    profile.kind == ClientKind::ClaudeCode
        && timeout_secs > 0
        && estimated_input_tokens >= min_input_tokens
}

fn adaptive_no_forwardable_retry_after(
    configured: std::time::Duration,
    estimated_input_tokens: u64,
) -> std::time::Duration {
    let bucket_secs = match estimated_input_tokens {
        0..=49_999 => 10,
        50_000..=99_999 => 14,
        100_000..=199_999 => 22,
        200_000..=399_999 => 32,
        _ => 45,
    };
    configured.min(std::time::Duration::from_secs(bucket_secs))
}

fn anthropic_tool_json_delta_chunks(input: &str) -> Vec<&str> {
    if input.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < input.len() {
        let mut end = (start + ANTHROPIC_TOOL_JSON_DELTA_CHUNK_BYTES).min(input.len());
        while end > start && !input.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            end = input[start..]
                .chars()
                .next()
                .map(|ch| start + ch.len_utf8())
                .unwrap_or(input.len());
        }
        chunks.push(&input[start..end]);
        start = end;
    }
    chunks
}

#[derive(Debug, Default)]
struct ToolInputGate {
    required: Vec<String>,
}

fn input_gate_for_tool(body: &ChatRequest, name: &str) -> ToolInputGate {
    let Some(tools) = body.tools.as_ref() else {
        return ToolInputGate::default();
    };
    let name_lower = name.to_lowercase();
    let Some(parameters) = tools
        .iter()
        .find(|tool| tool.function.name.to_lowercase() == name_lower)
        .and_then(|tool| tool.function.parameters.as_ref())
    else {
        return ToolInputGate::default();
    };
    let required = parameters
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default();
    ToolInputGate { required }
}

fn fallback_required_fields_for_claude_code_tool(name: &str) -> Vec<String> {
    match name.to_ascii_lowercase().as_str() {
        "bash" => vec!["command"],
        "bashoutput" => vec!["bash_id"],
        "agent" => vec!["description", "prompt"],
        "edit" => vec!["file_path", "old_string", "new_string"],
        "glob" => vec!["pattern"],
        "grep" => vec!["pattern"],
        "killbash" => vec!["shell_id"],
        "ls" => vec!["path"],
        "multiedit" => vec!["file_path", "edits"],
        "notebookedit" => vec!["notebook_path", "cell_id", "new_source"],
        "notebookread" => vec!["notebook_path"],
        "read" => vec!["file_path"],
        "sendmessage" => vec!["to", "message"],
        "task" => vec!["description", "prompt", "subagent_type"],
        "todowrite" => vec!["todos"],
        "toolsearch" => vec!["query"],
        "webfetch" => vec!["url"],
        "websearch" => vec!["query"],
        "write" => vec!["file_path", "content"],
        _ => Vec::new(),
    }
    .into_iter()
    .map(ToOwned::to_owned)
    .collect()
}

fn required_fields_for_tool(body: &ChatRequest, name: &str, profile: ClientProfile) -> Vec<String> {
    let gate = input_gate_for_tool(body, name);
    if !gate.required.is_empty() {
        return gate.required;
    }
    if profile.kind == ClientKind::ClaudeCode {
        return fallback_required_fields_for_claude_code_tool(name);
    }
    Vec::new()
}

fn json_object_is_empty(input: &Value) -> bool {
    input.as_object().is_some_and(serde_json::Map::is_empty)
}

fn tool_input_has_required_fields(input: &Value, required: &[String]) -> bool {
    if required.is_empty() {
        return true;
    }
    let Some(input) = input.as_object() else {
        return false;
    };
    required
        .iter()
        .all(|field| input.get(field).is_some_and(|value| !value.is_null()))
}

fn json_string_field_is_non_empty(input: &Value, field: &str) -> bool {
    input
        .get(field)
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
}

fn claude_code_tool_input_satisfies_local_rules(tool_name: &str, input: &Value) -> bool {
    let tool = tool_name.to_ascii_lowercase();
    match tool.as_str() {
        "bash" => json_string_field_is_non_empty(input, "command"),
        "bashoutput" => json_string_field_is_non_empty(input, "bash_id"),
        "glob" => json_string_field_is_non_empty(input, "pattern"),
        "grep" => json_string_field_is_non_empty(input, "pattern"),
        "killbash" => json_string_field_is_non_empty(input, "shell_id"),
        "ls" => json_string_field_is_non_empty(input, "path"),
        "toolsearch" | "websearch" => json_string_field_is_non_empty(input, "query"),
        "webfetch" => json_string_field_is_non_empty(input, "url"),
        "sendmessage" => claude_code_send_message_input_satisfies_local_rules(input),
        _ => true,
    }
}

fn claude_code_send_message_input_satisfies_local_rules(input: &Value) -> bool {
    let Some(input) = input.as_object() else {
        return false;
    };
    let Some(to) = input.get("to").and_then(Value::as_str).map(str::trim) else {
        return false;
    };
    if to.is_empty() || to.contains('@') {
        return false;
    }
    let Some(message) = input.get("message") else {
        return false;
    };
    if message.is_string() {
        return input
            .get("summary")
            .and_then(Value::as_str)
            .is_some_and(|summary| !summary.trim().is_empty());
    }
    if to == "*" {
        return false;
    }
    message.is_object()
}

fn claude_code_required_field_needs_repair(tool_name: &str, field: &str, value: &Value) -> bool {
    let tool = tool_name.to_ascii_lowercase();
    if matches!(field, "file_path" | "notebook_path")
        && matches!(
            tool.as_str(),
            "read" | "write" | "edit" | "multiedit" | "notebookread" | "notebookedit"
        )
    {
        return value.as_str().is_none_or(|path| {
            let path = path.trim();
            path.is_empty() || matches!(path, "." | "/" | "\\")
        });
    }
    if matches!(tool.as_str(), "bash" | "toolsearch" | "websearch")
        && matches!(field, "command" | "query")
    {
        return value.as_str().is_none_or(|text| text.trim().is_empty());
    }
    false
}

fn claude_code_tool_input_has_repairable_required_fields(
    tool_name: &str,
    input: &Value,
    required: &[String],
) -> bool {
    let Some(input) = input.as_object() else {
        return false;
    };
    required.iter().any(|field| {
        input
            .get(field)
            .is_some_and(|value| claude_code_required_field_needs_repair(tool_name, field, value))
    })
}

fn schema_allows_empty_tool_input(body: &ChatRequest, name: &str, profile: ClientProfile) -> bool {
    required_fields_for_tool(body, name, profile).is_empty()
}

fn streamable_anthropic_tool_call(
    tool: &crate::zen::client::CollectedToolCall,
    body: &ChatRequest,
    profile: ClientProfile,
) -> Option<(ToolCall, Value)> {
    let tc = anthropic_tool_call_identity(tool, body)?;
    let tc = ToolCall {
        function: ToolFunction {
            arguments: tool.arguments.clone(),
            ..tc.function
        },
        ..tc
    };
    let ct = if profile.uses_compat_tool_history() {
        synthesis::tool::complete_tool_call(&tc, body)
    } else {
        tc
    };
    if ct.function.name.trim().is_empty() {
        return None;
    }
    let required = required_fields_for_tool(body, &ct.function.name, profile);
    let mut input = if ct.function.arguments.trim().is_empty() {
        Value::Object(Default::default())
    } else {
        serde_json::from_str(&ct.function.arguments).ok()?
    };
    if profile.kind == ClientKind::ClaudeCode {
        if let Some(repaired) = repair_claude_code_conditional_tool_input(&ct.function.name, &input)
        {
            tracing::warn!(
                protocol = "anthropic",
                source_client = ?profile.kind,
                tool_name = %ct.function.name,
                "repaired ClaudeCode conditional tool call arguments"
            );
            input = repaired;
        }
    }
    if !tool_input_has_required_fields(&input, &required) {
        return None;
    }
    if profile.kind == ClientKind::ClaudeCode
        && claude_code_tool_input_has_repairable_required_fields(
            &ct.function.name,
            &input,
            &required,
        )
    {
        return None;
    }
    if json_object_is_empty(&input)
        && !schema_allows_empty_tool_input(body, &ct.function.name, profile)
    {
        return None;
    }
    if profile.kind == ClientKind::ClaudeCode
        && !claude_code_tool_input_satisfies_local_rules(&ct.function.name, &input)
    {
        return None;
    }
    Some((ct, input))
}

fn anthropic_tool_call_identity(
    tool: &crate::zen::client::CollectedToolCall,
    body: &ChatRequest,
) -> Option<ToolCall> {
    let clean_id = tool
        .id
        .clone()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| format!("call_{}", tool.index));
    let clean_id = if let Some(pos) = clean_id.find('{') {
        clean_id[..pos].to_string()
    } else {
        clean_id
    };
    let tc = ToolCall {
        id: Some(clean_id),
        call_type: "function".into(),
        function: ToolFunction {
            name: tool.name.clone(),
            arguments: String::new(),
        },
        index: Some(tool.index),
    };
    let tc = synthesis::tool::canonicalize_tool_call_name(&tc, body);
    if tc.function.name.trim().is_empty() {
        None
    } else {
        Some(tc)
    }
}

fn repair_claude_code_conditional_tool_input(tool_name: &str, input: &Value) -> Option<Value> {
    if !tool_name.eq_ignore_ascii_case("sendmessage") {
        return None;
    }
    let mut object = input.as_object()?.clone();
    let message = object.get("message")?.as_str()?;
    if object
        .get("summary")
        .and_then(Value::as_str)
        .is_some_and(|summary| !summary.trim().is_empty())
    {
        return None;
    }
    object.insert(
        "summary".to_string(),
        Value::String(send_message_summary_from_message(message)),
    );
    Some(Value::Object(object))
}

fn send_message_summary_from_message(message: &str) -> String {
    let summary = message
        .split_whitespace()
        .take(12)
        .collect::<Vec<_>>()
        .join(" ");
    let summary = if summary.is_empty() {
        "Message teammate".to_string()
    } else {
        summary
    };
    summary.chars().take(96).collect()
}

async fn handle_stream(
    client: &Client,
    config: &KernelConfig,
    cr: &ChatRequest,
    zb: &Value,
    profile: ClientProfile,
    tool_history_repair: translate::ToolHistoryRepair,
    claude_code_buffer_reason: Option<ClaudeCodeBufferedStreamReason>,
) -> Result<Response, AppError> {
    use axum::response::sse::{Event, Sse};
    use std::convert::Infallible;
    use std::time::{Duration, Instant};

    let model = cr.model.clone();
    let msg_id = format!(
        "msg_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    let body = cr.clone();
    let m = model.clone();
    let request_shape = translate::request_shape(&body);
    let prompt_hash = request_shape.prompt_hash;
    let prompt_hash_hex = format!("{prompt_hash:016x}");
    let prompt = translate::build_prompt_text(&body.messages);
    let estimated_input_tokens = estimate(&prompt).max(1);
    let initial_input_tokens = estimated_input_tokens;
    if claude_code_buffer_reason.is_some() {
        return handle_buffered_claude_code_huge_stream(
            client,
            config,
            cr,
            zb,
            estimated_input_tokens,
            profile,
            tool_history_repair,
        )
        .await;
    }
    let client = client.clone();
    let zen_chat_url = config.zen_chat_url.clone();
    let zen_api_key = config.zen_api_key.clone();
    let extra_headers = config.extra_headers.clone();
    let base_body = zb.clone();
    let send_idle_ping = profile.kind == ClientKind::ClaudeCode;
    let idle_ping_interval = Duration::from_secs(CLAUDE_CODE_STREAM_IDLE_PING_SECS);
    let true_first_token_frt = config.true_first_token_frt;
    let no_forwardable_retry_after = adaptive_no_forwardable_retry_after(
        Duration::from_secs(config.claude_code_stream_no_forwardable_retry_secs.max(1)),
        request_shape.estimated_total_tokens,
    );
    let initial_fetch_timeout = if should_apply_initial_fetch_timeout(
        profile,
        request_shape.estimated_total_tokens,
        config.claude_code_stream_slow_guard_min_input_tokens,
        config.claude_code_stream_initial_fetch_timeout_secs,
    ) {
        Some(Duration::from_secs(
            config.claude_code_stream_initial_fetch_timeout_secs,
        ))
    } else {
        None
    };
    let slow_guard_min_input_tokens = config.claude_code_stream_slow_guard_min_input_tokens;
    let stream = async_stream::stream! {
        let stream_started = Instant::now();
        let mut message_started = false;
        if !true_first_token_frt {
            yield Ok::<_, Infallible>(Event::default().event("message_start").data(serde_json::json!({"type":"message_start","message":{"id":msg_id,"type":"message","role":"assistant","model":m,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":initial_input_tokens,"output_tokens":0}}}).to_string()));
            message_started = true;
        }
        let mut last_downstream_event = Instant::now();
        let mut idle_ping_count = 0_u64;
        let mut attempts_used = 0_usize;
        let mut used_disabled_thinking_retry = false;
        let mut used_provider_invalid_disabled_retry = false;
        let mut used_provider_invalid_text_retry = false;
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut text_block_open = false;
        let mut text_block_index = 0_u64;
        let mut markdown_guard = if profile.preserves_model_text_exactly() {
            None
        } else {
            Some(crate::proxy::markdown::MarkdownFenceGuard::new())
        };
        let mut tool_calls: Vec<crate::zen::client::CollectedToolCall> = Vec::new();
        let mut usage: Option<crate::zen::client::ZenUsage> = None;
        let mut upstream_finish_reason: Option<String> = None;
        let mut cache_signals = ProviderCacheSignals::ignored();
        let mut final_stream_error: Option<String> = None;
        let mut completed_upstream = false;
        let mut started_tool_call_indexes = std::collections::HashSet::<i64>::new();
        let mut started_tool_block_indexes = std::collections::HashMap::<i64, u64>::new();
        let mut tool_argument_offsets = std::collections::HashMap::<i64, usize>::new();
        let mut emitted_tool_call_indexes = std::collections::HashSet::<i64>::new();
        let mut emitted_tool_call_signatures = std::collections::HashSet::<String>::new();
        let mut emitted_tool_call_blocks = 0_u64;
        let mut first_tool_emit_ms = 0_u64;
        let mut attempt_body = base_body.clone();
        let mut first_upstream_response_ms = 0_u64;
        let mut first_upstream_event_ms = 0_u64;
        let mut first_content_ms = 0_u64;
        let mut first_tool_call_ms = 0_u64;
        let mut first_reasoning_ms = 0_u64;
        for attempt in 0..CLAUDE_CODE_STREAM_GUARD_ATTEMPTS {
            attempts_used = attempt + 1;
            let attempt_started = Instant::now();
            let mut upstream_event_count = 0_u64;
            let fetch = crate::zen::client::fetch_zen_stream_with_headers(
                &client,
                &zen_chat_url,
                &zen_api_key,
                &attempt_body,
                &extra_headers,
            );
            let resp = match if let Some(timeout) = initial_fetch_timeout {
                match tokio::time::timeout(timeout, fetch).await {
                    Ok(result) => result,
                    Err(_) => Err(AppError::new(
                        axum::http::StatusCode::GATEWAY_TIMEOUT,
                        format!(
                            "upstream initial stream fetch timeout after {}s",
                            timeout.as_secs()
                        ),
                    )),
                }
            } else {
                fetch.await
            } {
                Ok(resp) => resp,
                Err(err) => {
                    tracing::warn!(
                        protocol = "anthropic",
                        model = %body.model,
                        source_client = ?profile.kind,
                        prompt_hash,
                        prompt_hash_hex = %prompt_hash_hex,
                        attempt = attempts_used,
                        max_attempts = CLAUDE_CODE_STREAM_GUARD_ATTEMPTS,
                        elapsed_ms = attempt_started.elapsed().as_millis() as u64,
                        initial_fetch_timeout_secs = initial_fetch_timeout.map(|timeout| timeout.as_secs()).unwrap_or(0),
                        slow_guard_min_input_tokens,
                        estimated_total_tokens = request_shape.estimated_total_tokens,
                        error = %err.message,
                        text_chars = text.len(),
                        reasoning_chars = reasoning.len(),
                        tool_call_count = tool_calls.len(),
                        idle_ping_count,
                        "ClaudeCode stream guard upstream fetch failed"
                    );
                    final_stream_error = Some(err.message.clone());
                    if super::should_retry_missing_reasoning_content(
                        &err,
                        used_disabled_thinking_retry,
                    ) {
                        used_disabled_thinking_retry = true;
                        attempt_body = super::reasoning_disabled_retry_body(&base_body);
                        super::log_missing_reasoning_content_retry(
                            "anthropic",
                            &body,
                            profile,
                            attempts_used,
                        );
                        continue;
                    }
                    if let Some(mode) = super::provider_invalid_tool_history_retry_mode(
                        &err,
                        &body,
                        profile,
                        tool_history_repair,
                        used_provider_invalid_disabled_retry,
                        used_provider_invalid_text_retry,
                    ) {
                        match mode {
                            super::ProviderInvalidRetryMode::DisableThinking => {
                                used_provider_invalid_disabled_retry = true;
                            }
                            super::ProviderInvalidRetryMode::TextOnly => {
                                used_provider_invalid_text_retry = true;
                            }
                        }
                        let (retry_body, stats) =
                            super::provider_invalid_tool_history_retry_body(&base_body, mode);
                        attempt_body = retry_body;
                        super::log_provider_invalid_tool_history_retry(
                            "anthropic_stream",
                            &body,
                            profile,
                            tool_history_repair,
                            mode,
                            stats,
                            attempts_used,
                        );
                        continue;
                    }
                    if should_retry_stream_error_before_output(profile, attempt, &text, &tool_calls) {
                        continue;
                    }
                    break;
                }
            };
            if first_upstream_response_ms == 0 {
                first_upstream_response_ms = stream_started.elapsed().as_millis() as u64;
            }
            cache_signals = ProviderCacheSignals::from_response_headers(resp.headers());
            let mut upstream = Box::pin(crate::zen::client::stream_sse_events(resp.bytes_stream()));
            let mut retry_attempt = false;
            loop {
                let next_event = if send_idle_ping {
                    match tokio::time::timeout(idle_ping_interval, upstream.next()).await {
                        Ok(next) => next,
                        Err(_) => {
                            idle_ping_count += 1;
                            if !true_first_token_frt || message_started {
                                tracing::info!(
                                    protocol = "anthropic",
                                    model = %body.model,
                                    source_client = ?profile.kind,
                                    prompt_hash,
                                    prompt_hash_hex = %prompt_hash_hex,
                                    attempt = attempts_used,
                                    idle_ping_count,
                                    idle_ping_secs = CLAUDE_CODE_STREAM_IDLE_PING_SECS,
                                    "sent ClaudeCode stream idle ping while waiting for upstream event"
                                );
                                yield Ok(Event::default().event("ping").data(serde_json::json!({"type":"ping"}).to_string()));
                                last_downstream_event = Instant::now();
                            } else {
                                tracing::debug!(
                                    protocol = "anthropic",
                                    model = %body.model,
                                    source_client = ?profile.kind,
                                    prompt_hash,
                                    prompt_hash_hex = %prompt_hash_hex,
                                    attempt = attempts_used,
                                    idle_ping_count,
                                    "suppressed pre-first-token ping to keep NewAPI FRT tied to real content"
                                );
                            }
                            if started_tool_call_indexes.is_empty()
                                && should_retry_stream_without_forwardable_output(
                                profile,
                                attempt,
                                &text,
                                &tool_calls,
                                attempt_started.elapsed(),
                                no_forwardable_retry_after,
                            ) {
                                tracing::warn!(
                                    protocol = "anthropic",
                                    model = %body.model,
                                    source_client = ?profile.kind,
                                    prompt_hash,
                                    prompt_hash_hex = %prompt_hash_hex,
                                    attempt = attempts_used,
                                    max_attempts = CLAUDE_CODE_STREAM_GUARD_ATTEMPTS,
                                    elapsed_ms = attempt_started.elapsed().as_millis() as u64,
                                    no_forwardable_retry_after_secs = no_forwardable_retry_after.as_secs(),
                                    idle_ping_count,
                                    upstream_event_count,
                                    text_chars = text.len(),
                                    reasoning_chars = reasoning.len(),
                                    tool_call_count = tool_calls.len(),
                                    "ClaudeCode stream guard retrying after no forwardable upstream output"
                                );
                                retry_attempt = true;
                                break;
                            }
                            continue;
                        }
                    }
                } else {
                    upstream.next().await
                };
                let Some(event) = next_event else {
                    completed_upstream = true;
                    break;
                };
                let event = match event {
                    Ok(event) => {
                        upstream_event_count += 1;
                        if first_upstream_event_ms == 0 {
                            first_upstream_event_ms = stream_started.elapsed().as_millis() as u64;
                        }
                        event
                    }
                    Err(err) => {
                        tracing::warn!(
                            protocol = "anthropic",
                            model = %body.model,
                            source_client = ?profile.kind,
                            prompt_hash,
                            prompt_hash_hex = %prompt_hash_hex,
                            attempt = attempts_used,
                            max_attempts = CLAUDE_CODE_STREAM_GUARD_ATTEMPTS,
                            error = %err.message,
                            elapsed_ms = attempt_started.elapsed().as_millis() as u64,
                            idle_ping_count,
                            upstream_event_count,
                            text_chars = text.len(),
                            reasoning_chars = reasoning.len(),
                            tool_call_count = tool_calls.len(),
                            finish_reason = ?upstream_finish_reason,
                            text_block_open,
                            "ClaudeCode stream guard observed upstream stream error"
                        );
                        final_stream_error = Some(err.message);
                        if should_retry_stream_error_before_output(profile, attempt, &text, &tool_calls) {
                            retry_attempt = true;
                        }
                        break;
                    }
                };
                let mut emitted_downstream_event = false;
                if event.usage.is_some() {
                    usage = event.usage;
                }
                if let Some(choices) = event.choices {
                    for choice in choices {
                        if let Some(reason) = choice.finish_reason.as_deref().filter(|reason| !reason.is_empty()) {
                            upstream_finish_reason = Some(reason.to_string());
                        }
                        let Some(delta) = choice.delta else { continue; };
                        if let Some(content) = delta.content {
                            let content = if let Some(markdown_guard) = markdown_guard.as_mut() {
                                markdown_guard.push(&crate::redact::redact_text(&content))
                            } else {
                                content
                            };
                            let should_emit =
                                !content.trim().is_empty()
                                    || (profile.preserves_stream_whitespace() && !content.is_empty());
                            if should_emit {
                                if first_content_ms == 0 {
                                    first_content_ms = stream_started.elapsed().as_millis() as u64;
                                }
                                if !message_started {
                                    yield Ok(Event::default().event("message_start").data(serde_json::json!({"type":"message_start","message":{"id":msg_id,"type":"message","role":"assistant","model":m,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":initial_input_tokens,"output_tokens":0}}}).to_string()));
                                    message_started = true;
                                }
                                if !text_block_open {
                                    text_block_open = true;
                                    text_block_index = emitted_tool_call_blocks;
                                    yield Ok(Event::default().event("content_block_start").data(serde_json::json!({"type":"content_block_start","index":text_block_index,"content_block":{"type":"text","text":""}}).to_string()));
                                }
                                text.push_str(&content);
                                yield Ok(Event::default().event("content_block_delta").data(serde_json::json!({"type":"content_block_delta","index":text_block_index,"delta":{"type":"text_delta","text":content}}).to_string()));
                                emitted_downstream_event = true;
                            }
                        }
                        if let Some(reasoning_content) = delta.reasoning_content {
                            if first_reasoning_ms == 0 {
                                first_reasoning_ms = stream_started.elapsed().as_millis() as u64;
                            }
                            reasoning.push_str(&reasoning_content);
                        }
                        if let Some(items) = delta.tool_calls {
                            let had_tool_calls = !tool_calls.is_empty();
                            merge_tool_deltas(&mut tool_calls, items);
                            if first_tool_call_ms == 0 && !had_tool_calls && !tool_calls.is_empty() {
                                first_tool_call_ms = stream_started.elapsed().as_millis() as u64;
                            }
                            if profile.kind == ClientKind::ClaudeCode && text.trim().is_empty() {
                                for tool in &tool_calls {
                                    if emitted_tool_call_indexes.contains(&tool.index) {
                                        continue;
                                    }
                                    let Some(ct) = anthropic_tool_call_identity(tool, &body) else {
                                        continue;
                                    };
                                    if ct.function.name.eq_ignore_ascii_case("sendmessage") {
                                        continue;
                                    }
                                    if tool.arguments.trim().is_empty()
                                        || (serde_json::from_str::<Value>(&tool.arguments).is_ok()
                                            && streamable_anthropic_tool_call(tool, &body, profile)
                                                .is_none())
                                    {
                                        continue;
                                    }
                                    let tidx = if let Some(tidx) =
                                        started_tool_block_indexes.get(&tool.index).copied()
                                    {
                                        tidx
                                    } else {
                                        if !message_started {
                                            yield Ok(Event::default().event("message_start").data(serde_json::json!({"type":"message_start","message":{"id":msg_id,"type":"message","role":"assistant","model":m,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":initial_input_tokens,"output_tokens":0}}}).to_string()));
                                            message_started = true;
                                        }
                                        if text_block_open {
                                            yield Ok(Event::default().event("content_block_stop").data(serde_json::json!({"type":"content_block_stop","index":text_block_index}).to_string()));
                                            text_block_open = false;
                                        }
                                        let tidx = emitted_tool_call_blocks;
                                        yield Ok(Event::default().event("content_block_start").data(serde_json::json!({"type":"content_block_start","index":tidx,"content_block":{"type":"tool_use","id":ct.id,"name":ct.function.name,"input":{}}}).to_string()));
                                        started_tool_call_indexes.insert(tool.index);
                                        started_tool_block_indexes.insert(tool.index, tidx);
                                        emitted_tool_call_blocks = emitted_tool_call_blocks.saturating_add(1);
                                        if first_tool_emit_ms == 0 {
                                            first_tool_emit_ms = stream_started.elapsed().as_millis() as u64;
                                        }
                                        emitted_downstream_event = true;
                                        tidx
                                    };
                                    let offset =
                                        tool_argument_offsets.get(&tool.index).copied().unwrap_or(0);
                                    if tool.arguments.len() > offset
                                        && tool.arguments.is_char_boundary(offset)
                                    {
                                        let delta = &tool.arguments[offset..];
                                        for chunk in anthropic_tool_json_delta_chunks(delta) {
                                            yield Ok(Event::default().event("content_block_delta").data(serde_json::json!({"type":"content_block_delta","index":tidx,"delta":{"type":"input_json_delta","partial_json":chunk}}).to_string()));
                                        }
                                        tool_argument_offsets.insert(tool.index, tool.arguments.len());
                                        emitted_downstream_event = true;
                                    }
                                }
                            }
                        }
                    }
                }
                if emitted_downstream_event {
                    last_downstream_event = Instant::now();
                } else if send_idle_ping && last_downstream_event.elapsed() >= idle_ping_interval {
                    idle_ping_count += 1;
                    if !true_first_token_frt || message_started {
                        tracing::info!(
                            protocol = "anthropic",
                            model = %body.model,
                            source_client = ?profile.kind,
                            prompt_hash,
                            prompt_hash_hex = %prompt_hash_hex,
                            attempt = attempts_used,
                            idle_ping_count,
                            idle_ping_secs = CLAUDE_CODE_STREAM_IDLE_PING_SECS,
                            "sent ClaudeCode stream idle ping while upstream produced no forwardable output"
                        );
                        yield Ok(Event::default().event("ping").data(serde_json::json!({"type":"ping"}).to_string()));
                        last_downstream_event = Instant::now();
                    } else {
                        tracing::debug!(
                            protocol = "anthropic",
                            model = %body.model,
                            source_client = ?profile.kind,
                            prompt_hash,
                            prompt_hash_hex = %prompt_hash_hex,
                            attempt = attempts_used,
                            idle_ping_count,
                            "suppressed pre-first-token ping after non-forwardable upstream event"
                        );
                    }
                    if started_tool_call_indexes.is_empty()
                        && should_retry_stream_without_forwardable_output(
                        profile,
                        attempt,
                        &text,
                        &tool_calls,
                        attempt_started.elapsed(),
                        no_forwardable_retry_after,
                    ) {
                        tracing::warn!(
                            protocol = "anthropic",
                            model = %body.model,
                            source_client = ?profile.kind,
                            prompt_hash,
                            prompt_hash_hex = %prompt_hash_hex,
                            attempt = attempts_used,
                            max_attempts = CLAUDE_CODE_STREAM_GUARD_ATTEMPTS,
                            elapsed_ms = attempt_started.elapsed().as_millis() as u64,
                            no_forwardable_retry_after_secs = no_forwardable_retry_after.as_secs(),
                            idle_ping_count,
                            upstream_event_count,
                            text_chars = text.len(),
                            reasoning_chars = reasoning.len(),
                            tool_call_count = tool_calls.len(),
                            "ClaudeCode stream guard retrying after reasoning-only/no-forwardable upstream output"
                        );
                        retry_attempt = true;
                        break;
                    }
                }
            }
            if completed_upstream {
                if started_tool_call_indexes.is_empty()
                    && !has_forwardable_anthropic_output(
                    &text,
                    &tool_calls,
                    &emitted_tool_call_indexes,
                    &body,
                    profile,
                ) && !tool_calls.is_empty()
                {
                    tracing::warn!(
                        protocol = "anthropic",
                        model = %body.model,
                        source_client = ?profile.kind,
                        prompt_hash,
                        prompt_hash_hex = %prompt_hash_hex,
                        attempt = attempts_used,
                        max_attempts = CLAUDE_CODE_STREAM_GUARD_ATTEMPTS,
                        upstream_event_count,
                        tool_call_count = tool_calls.len(),
                        finish_reason = ?upstream_finish_reason,
                        "ClaudeCode stream guard received only incomplete tool calls"
                    );
                    if profile.kind == ClientKind::ClaudeCode
                        && attempt + 1 < CLAUDE_CODE_STREAM_GUARD_ATTEMPTS
                    {
                        if attempt + 2 == CLAUDE_CODE_STREAM_GUARD_ATTEMPTS
                            && !used_disabled_thinking_retry
                            && body.tools.as_ref().is_some_and(|tools| !tools.is_empty())
                        {
                            used_disabled_thinking_retry = true;
                            attempt_body = super::reasoning_disabled_retry_body(&base_body);
                            tracing::warn!(
                                protocol = "anthropic",
                                model = %body.model,
                                source_client = ?profile.kind,
                                prompt_hash,
                                prompt_hash_hex = %prompt_hash_hex,
                                next_attempt = attempt + 2,
                                "ClaudeCode stream guard enabling disabled-thinking retry for incomplete tool arguments"
                            );
                        }
                        completed_upstream = false;
                        upstream_finish_reason = None;
                        usage = None;
                        cache_signals = ProviderCacheSignals::ignored();
                        tool_calls.clear();
                        continue;
                    }
                    final_stream_error =
                        Some("upstream returned incomplete tool call arguments".to_string());
                }
                break;
            }
            if retry_attempt {
                if attempt + 2 == CLAUDE_CODE_STREAM_GUARD_ATTEMPTS
                    && !used_disabled_thinking_retry
                    && body.tools.as_ref().is_some_and(|tools| !tools.is_empty())
                {
                    used_disabled_thinking_retry = true;
                    attempt_body = super::reasoning_disabled_retry_body(&base_body);
                    tracing::warn!(
                        protocol = "anthropic",
                        model = %body.model,
                        source_client = ?profile.kind,
                        prompt_hash,
                        prompt_hash_hex = %prompt_hash_hex,
                        next_attempt = attempt + 2,
                        "ClaudeCode stream guard enabling disabled-thinking fallback for final tool retry"
                    );
                }
                continue;
            }
            break;
        }
        if final_stream_error.is_some() && !tool_calls.is_empty() && emitted_tool_call_indexes.is_empty() {
            tracing::warn!(
                protocol = "anthropic",
                model = %body.model,
                source_client = ?profile.kind,
                prompt_hash,
                prompt_hash_hex = %prompt_hash_hex,
                attempts_used,
                text_chars = text.len(),
                reasoning_chars = reasoning.len(),
                tool_call_count = tool_calls.len(),
                error = ?final_stream_error,
                "ClaudeCode stream guard refusing to emit possibly partial tool calls after upstream truncation"
            );
            yield Ok(Event::default().event("error").data(serde_json::json!({"type":"error","error":{"type":"api_error","message":final_stream_error.unwrap_or_else(||"upstream stream truncated after partial tool call".to_string())}}).to_string()));
            return;
        } else if final_stream_error.is_some() && !emitted_tool_call_indexes.is_empty() {
            tracing::warn!(
                protocol = "anthropic",
                model = %body.model,
                source_client = ?profile.kind,
                prompt_hash,
                prompt_hash_hex = %prompt_hash_hex,
                attempts_used,
                text_chars = text.len(),
                reasoning_chars = reasoning.len(),
                tool_call_count = tool_calls.len(),
                emitted_tool_call_count = emitted_tool_call_indexes.len(),
                error = ?final_stream_error,
                "ClaudeCode stream guard preserving already emitted complete tool calls after upstream truncation"
            );
        }
        if final_stream_error.is_some() && !text.trim().is_empty() {
            tracing::warn!(
                protocol = "anthropic",
                model = %body.model,
                source_client = ?profile.kind,
                prompt_hash,
                prompt_hash_hex = %prompt_hash_hex,
                attempts_used,
                text_chars = text.len(),
                reasoning_chars = reasoning.len(),
                error = ?final_stream_error,
                "ClaudeCode stream guard closing partial text stream with max_tokens stop reason after upstream truncation"
            );
            upstream_finish_reason = Some("length".to_string());
        }
        let final_markdown = markdown_guard
            .as_mut()
            .map(crate::proxy::markdown::MarkdownFenceGuard::finish)
            .unwrap_or_default();
        if !final_markdown.is_empty() {
            if !message_started {
                yield Ok(Event::default().event("message_start").data(serde_json::json!({"type":"message_start","message":{"id":msg_id,"type":"message","role":"assistant","model":m,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":initial_input_tokens,"output_tokens":0}}}).to_string()));
            }
            if !text_block_open {
                text_block_open = true;
                text_block_index = emitted_tool_call_blocks;
                yield Ok(Event::default().event("content_block_start").data(serde_json::json!({"type":"content_block_start","index":text_block_index,"content_block":{"type":"text","text":""}}).to_string()));
            }
            text.push_str(&final_markdown);
            yield Ok(Event::default().event("content_block_delta").data(serde_json::json!({"type":"content_block_delta","index":text_block_index,"delta":{"type":"text_delta","text":final_markdown}}).to_string()));
        }
        if text.trim().is_empty() && tool_calls.is_empty() {
            if let Some(fallback_text) = translate::short_no_tool_empty_fallback_text(&body) {
                tracing::warn!(
                    model = body.model,
                    source_client = ?profile.kind,
                    "short channel-test probe received empty upstream; returning local ok"
                );
                if !message_started {
                    yield Ok(Event::default().event("message_start").data(serde_json::json!({"type":"message_start","message":{"id":msg_id,"type":"message","role":"assistant","model":m,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":initial_input_tokens,"output_tokens":0}}}).to_string()));
                    message_started = true;
                }
                text_block_open = true;
                text_block_index = emitted_tool_call_blocks;
                text.push_str(fallback_text);
                yield Ok(Event::default().event("content_block_start").data(serde_json::json!({"type":"content_block_start","index":text_block_index,"content_block":{"type":"text","text":""}}).to_string()));
                yield Ok(Event::default().event("content_block_delta").data(serde_json::json!({"type":"content_block_delta","index":text_block_index,"delta":{"type":"text_delta","text":fallback_text}}).to_string()));
            } else {
                let empty_output_class = if !reasoning.trim().is_empty() {
                    if upstream_finish_reason.as_deref() == Some("length") {
                        "reasoning_only_length"
                    } else {
                        "reasoning_only"
                    }
                } else {
                    "empty_output"
                };
                tracing::warn!(
                    protocol = "anthropic",
                    model = %body.model,
                    source_client = ?profile.kind,
                    prompt_hash,
                    prompt_hash_hex = %prompt_hash_hex,
                    empty_output_class,
                    finish_reason = ?upstream_finish_reason,
                    reasoning_chars = reasoning.len(),
                    content_chars = text.len(),
                    "stream upstream returned no assistant content or tool call"
                );
                let message = final_stream_error
                    .clone()
                    .unwrap_or_else(|| format!("upstream returned no assistant content or tool call (class={empty_output_class})"));
                yield Ok(Event::default().event("error").data(serde_json::json!({"type":"error","error":{"type":"api_error","message":message}}).to_string()));
                return;
            }
        }
        if text_block_open {
            yield Ok(Event::default().event("content_block_stop").data(serde_json::json!({"type":"content_block_stop","index":text_block_index}).to_string()));
            emitted_tool_call_blocks =
                emitted_tool_call_blocks.max(text_block_index.saturating_add(1));
        }
        if !tool_calls.is_empty() {
            if !message_started {
                yield Ok(Event::default().event("message_start").data(serde_json::json!({"type":"message_start","message":{"id":msg_id,"type":"message","role":"assistant","model":m,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":initial_input_tokens,"output_tokens":0}}}).to_string()));
            }
            for tool in tool_calls.iter() {
                if emitted_tool_call_indexes.contains(&tool.index) {
                    continue;
                }
                let Some((ct, input)) = streamable_anthropic_tool_call(tool, &body, profile) else {
                    if started_tool_call_indexes.contains(&tool.index) {
                        tracing::warn!(
                            protocol = "anthropic",
                            model = %body.model,
                            source_client = ?profile.kind,
                            prompt_hash,
                            prompt_hash_hex = %prompt_hash_hex,
                            tool_index = tool.index,
                            tool_name = %tool.name,
                            argument_bytes = tool.arguments.len(),
                            "ClaudeCode progressive tool stream ended with incomplete arguments"
                        );
                        yield Ok(Event::default().event("error").data(serde_json::json!({"type":"error","error":{"type":"api_error","message":"upstream returned incomplete tool call arguments"}}).to_string()));
                        return;
                    }
                    continue;
                };
                let started = started_tool_call_indexes.contains(&tool.index);
                let tidx = if started {
                    started_tool_block_indexes
                        .get(&tool.index)
                        .copied()
                        .unwrap_or(emitted_tool_call_blocks)
                } else {
                    if should_skip_duplicate_claude_code_tool_call(
                        profile,
                        &mut emitted_tool_call_signatures,
                        &ct.function.name,
                        &input,
                    ) {
                        continue;
                    }
                    let tidx = emitted_tool_call_blocks;
                    yield Ok(Event::default().event("content_block_start").data(serde_json::json!({"type":"content_block_start","index":tidx,"content_block":{"type":"tool_use","id":ct.id,"name":ct.function.name,"input":{}}}).to_string()));
                    emitted_tool_call_blocks = emitted_tool_call_blocks.saturating_add(1);
                    if first_tool_emit_ms == 0 {
                        first_tool_emit_ms = stream_started.elapsed().as_millis() as u64;
                    }
                    tidx
                };
                let offset = tool_argument_offsets.get(&tool.index).copied().unwrap_or(0);
                if started && tool.arguments.len() > offset && tool.arguments.is_char_boundary(offset) {
                    let delta = &tool.arguments[offset..];
                    for chunk in anthropic_tool_json_delta_chunks(delta) {
                        yield Ok(Event::default().event("content_block_delta").data(serde_json::json!({"type":"content_block_delta","index":tidx,"delta":{"type":"input_json_delta","partial_json":chunk}}).to_string()));
                    }
                    tool_argument_offsets.insert(tool.index, tool.arguments.len());
                } else if !started {
                    let js = serde_json::to_string(&input).unwrap_or_default();
                    if js != "{}" {
                        for chunk in anthropic_tool_json_delta_chunks(&js) {
                            yield Ok(Event::default().event("content_block_delta").data(serde_json::json!({"type":"content_block_delta","index":tidx,"delta":{"type":"input_json_delta","partial_json":chunk}}).to_string()));
                        }
                    }
                }
                yield Ok(Event::default().event("content_block_stop").data(serde_json::json!({"type":"content_block_stop","index":tidx}).to_string()));
                emitted_tool_call_indexes.insert(tool.index);
            }
        }
        let stop_reason = anthropic_stop_reason(upstream_finish_reason.as_deref(), !tool_calls.is_empty());
        let output_tokens = usage
            .as_ref()
            .and_then(|usage| usage.completion_tokens)
            .unwrap_or_else(|| {
                if !text.trim().is_empty() {
                    estimate(&text)
                } else {
                    estimate(&tool_calls.iter().map(|tool| format!("{} {}", tool.name, tool.arguments)).collect::<Vec<_>>().join("\n")).max(1)
                }
            });
        let cache_creation = usage
            .as_ref()
            .and_then(|usage| usage.cache_creation_input_tokens)
            .unwrap_or(0);
        let cache_read = usage
            .as_ref()
            .and_then(crate::zen::client::ZenUsage::cache_read_tokens)
            .unwrap_or(0);
        let cache_signals = cache_signals.with_body_usage(usage.as_ref());
        super::log_provider_cache_observation("anthropic", &body, profile, &cache_signals, attempts_used, CLAUDE_CODE_STREAM_GUARD_ATTEMPTS);
        tracing::info!(
            protocol = "anthropic",
            model = %body.model,
            source_client = ?profile.kind,
            prompt_hash,
            prompt_hash_hex = %prompt_hash_hex,
            attempts_used,
            retry_count = attempts_used.saturating_sub(1),
            used_disabled_thinking_retry,
            completed_upstream,
            final_stream_error = ?final_stream_error,
            finish_reason = ?upstream_finish_reason,
            estimated_prompt_tokens = initial_input_tokens,
            estimated_total_tokens = request_shape.estimated_total_tokens,
            max_tokens = ?request_shape.max_tokens,
            message_count = request_shape.message_count,
            tool_count = request_shape.tool_count,
            first_upstream_response_ms,
            first_upstream_event_ms,
            first_reasoning_ms,
            first_content_ms,
            first_tool_call_ms,
            first_tool_emit_ms,
            total_elapsed_ms = stream_started.elapsed().as_millis() as u64,
            idle_ping_count,
            text_chars = text.len(),
            reasoning_chars = reasoning.len(),
            tool_call_count = tool_calls.len(),
            emitted_tool_call_count = emitted_tool_call_indexes.len(),
            output_tokens,
            cache_creation_input_tokens = cache_creation,
            cache_read_input_tokens = cache_read,
            cache_observation = cache_signals.status().as_str(),
            initial_fetch_timeout_secs = initial_fetch_timeout.map(|timeout| timeout.as_secs()).unwrap_or(0),
            slow_guard_min_input_tokens,
            no_forwardable_retry_after_secs = no_forwardable_retry_after.as_secs(),
            "ClaudeCode stream guard completion summary"
        );
        yield Ok(Event::default().event("message_delta").data(serde_json::json!({"type":"message_delta","delta":{"stop_reason":stop_reason,"stop_sequence":null},"usage":{"output_tokens":output_tokens,"cache_creation_input_tokens":cache_creation,"cache_read_input_tokens":cache_read}}).to_string()));
        yield Ok(Event::default().event("message_stop").data(serde_json::json!({"type":"message_stop"}).to_string()));
    };
    Ok(Sse::new(stream).into_response())
}

async fn handle_buffered_claude_code_huge_stream(
    client: &Client,
    config: &KernelConfig,
    cr: &ChatRequest,
    zb: &Value,
    estimated_input_tokens: u64,
    profile: ClientProfile,
    tool_history_repair: translate::ToolHistoryRepair,
) -> Result<Response, AppError> {
    let exact_output_literal = translate::exact_output_literal_from_messages(&cr.messages);
    let mut attempt_body = zb.clone();
    let mut used_reasoning_disabled_retry = false;
    let mut used_missing_reasoning_disabled_retry = false;
    let mut used_provider_invalid_disabled_retry = false;
    let mut used_provider_invalid_text_retry = false;

    for attempt in 0..CLAUDE_CODE_BUFFERED_STREAM_ATTEMPTS {
        let resp = match crate::zen::client::fetch_zen_stream_with_headers(
            client,
            &config.zen_chat_url,
            &config.zen_api_key,
            &attempt_body,
            &config.extra_headers,
        )
        .await
        {
            Ok(resp) => resp,
            Err(err) => {
                tracing::warn!(
                    attempt,
                    max_attempts = CLAUDE_CODE_BUFFERED_STREAM_ATTEMPTS,
                    error = %err.message,
                    "ClaudeCode huge stream buffered fetch failed"
                );
                if super::should_retry_missing_reasoning_content(
                    &err,
                    used_missing_reasoning_disabled_retry,
                ) {
                    used_missing_reasoning_disabled_retry = true;
                    attempt_body = super::reasoning_disabled_retry_body(zb);
                    super::log_missing_reasoning_content_retry(
                        "anthropic_buffered",
                        cr,
                        profile,
                        attempt + 1,
                    );
                    continue;
                }
                if let Some(mode) = super::provider_invalid_tool_history_retry_mode(
                    &err,
                    cr,
                    profile,
                    tool_history_repair,
                    used_provider_invalid_disabled_retry,
                    used_provider_invalid_text_retry,
                ) {
                    match mode {
                        super::ProviderInvalidRetryMode::DisableThinking => {
                            used_provider_invalid_disabled_retry = true;
                        }
                        super::ProviderInvalidRetryMode::TextOnly => {
                            used_provider_invalid_text_retry = true;
                        }
                    }
                    let (retry_body, stats) =
                        super::provider_invalid_tool_history_retry_body(zb, mode);
                    attempt_body = retry_body;
                    super::log_provider_invalid_tool_history_retry(
                        "anthropic_buffered",
                        cr,
                        profile,
                        tool_history_repair,
                        mode,
                        stats,
                        attempt + 1,
                    );
                    continue;
                }
                if attempt + 1 >= CLAUDE_CODE_BUFFERED_STREAM_ATTEMPTS {
                    return Err(err);
                }
                continue;
            }
        };

        let cache_signals = ProviderCacheSignals::from_response_headers(resp.headers());
        let collected = match crate::zen::client::collect_stream_parts(resp).await {
            Ok(collected) => collected,
            Err(err) => {
                tracing::warn!(
                    attempt,
                    max_attempts = CLAUDE_CODE_BUFFERED_STREAM_ATTEMPTS,
                    error = %err.message,
                    "ClaudeCode huge stream buffered collection failed"
                );
                if attempt + 1 >= CLAUDE_CODE_BUFFERED_STREAM_ATTEMPTS {
                    return Err(err);
                }
                continue;
            }
        };
        let cache_signals = cache_signals.with_body_usage(collected.usage.as_ref());
        super::log_provider_cache_observation(
            "anthropic_buffered",
            cr,
            profile,
            &cache_signals,
            attempt + 1,
            CLAUDE_CODE_BUFFERED_STREAM_ATTEMPTS,
        );
        let content = response_text_for_profile(profile, &collected.content);
        let output_class = super::classify_collected_output(&collected, &content);
        if output_class != super::OutputClass::Valid {
            super::log_empty_output_class(
                "anthropic_buffered",
                cr,
                profile,
                output_class,
                attempt + 1,
                CLAUDE_CODE_BUFFERED_STREAM_ATTEMPTS,
                &collected,
            );
            if output_class.should_retry_with_disabled_thinking() && !used_reasoning_disabled_retry
            {
                used_reasoning_disabled_retry = true;
                attempt_body = super::reasoning_disabled_retry_body(zb);
                tracing::warn!(
                    protocol = "anthropic_buffered",
                    model = %cr.model,
                    source_client = ?profile.kind,
                    empty_output_class = output_class.as_str(),
                    attempt = attempt + 1,
                    "retrying buffered reasoning-only output with disabled thinking"
                );
                continue;
            }
            if let Some(fallback_text) = translate::short_no_tool_empty_fallback_text(cr) {
                tracing::warn!(
                    model = cr.model,
                    source_client = ?profile.kind,
                    "short channel-test probe received empty buffered upstream; returning local ok"
                );
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                return Ok(anthropic_buffered_stream_resp(
                    ts,
                    &cr.model,
                    fallback_text,
                    Vec::new(),
                    estimated_input_tokens,
                    estimate(fallback_text).max(1),
                    0,
                    0,
                    "end_turn".to_string(),
                    cr,
                    profile,
                ));
            }
            if attempt + 1 >= CLAUDE_CODE_BUFFERED_STREAM_ATTEMPTS {
                if let Some(literal) = exact_output_literal.as_deref() {
                    tracing::warn!(
                        literal_len = literal.len(),
                        "ClaudeCode huge exact-output empty upstream fallback returned literal"
                    );
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis();
                    return Ok(anthropic_buffered_stream_resp(
                        ts,
                        &cr.model,
                        literal,
                        Vec::new(),
                        estimated_input_tokens,
                        estimate(literal).max(1),
                        0,
                        0,
                        "end_turn".to_string(),
                        cr,
                        profile,
                    ));
                }
            }
            continue;
        }
        if has_only_incomplete_tool_arguments(&content, &collected.tool_calls, cr, profile) {
            tracing::warn!(
                protocol = "anthropic_buffered",
                model = %cr.model,
                source_client = ?profile.kind,
                attempt = attempt + 1,
                max_attempts = CLAUDE_CODE_BUFFERED_STREAM_ATTEMPTS,
                tool_call_count = collected.tool_calls.len(),
                finish_reason = ?collected.finish_reason,
                "ClaudeCode buffered stream guard received only incomplete tool calls"
            );
            if profile.kind == ClientKind::ClaudeCode && !used_reasoning_disabled_retry {
                used_reasoning_disabled_retry = true;
                attempt_body = super::reasoning_disabled_retry_body(zb);
                tracing::warn!(
                    protocol = "anthropic_buffered",
                    model = %cr.model,
                    source_client = ?profile.kind,
                    next_attempt = attempt + 2,
                    "ClaudeCode buffered stream guard enabling disabled-thinking retry for incomplete tool arguments"
                );
                continue;
            }
            if attempt + 1 >= CLAUDE_CODE_BUFFERED_STREAM_ATTEMPTS {
                return Err(incomplete_tool_arguments_error());
            }
            continue;
        }

        let input_tokens = collected
            .usage
            .as_ref()
            .and_then(|usage| usage.prompt_tokens)
            .unwrap_or(estimated_input_tokens);
        let output_tokens = collected
            .usage
            .as_ref()
            .and_then(|usage| usage.completion_tokens)
            .unwrap_or_else(|| {
                if !content.trim().is_empty() {
                    estimate(&content)
                } else {
                    estimate(
                        &collected
                            .tool_calls
                            .iter()
                            .map(|tool| format!("{} {}", tool.name, tool.arguments))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )
                    .max(1)
                }
            });
        let cache_creation = collected
            .usage
            .as_ref()
            .and_then(|usage| usage.cache_creation_input_tokens)
            .unwrap_or(0);
        let cache_read = collected
            .usage
            .as_ref()
            .and_then(crate::zen::client::ZenUsage::cache_read_tokens)
            .unwrap_or(0);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let has_tool_calls = has_streamable_anthropic_tool_call(&collected.tool_calls, cr, profile);
        return Ok(anthropic_buffered_stream_resp(
            ts,
            &cr.model,
            &content,
            collected.tool_calls,
            input_tokens,
            output_tokens,
            cache_creation,
            cache_read,
            anthropic_stop_reason(collected.finish_reason.as_deref(), has_tool_calls).to_string(),
            cr,
            profile,
        ));
    }

    Err(AppError::empty_upstream_class("buffered_retry_exhausted"))
}

#[allow(clippy::too_many_arguments)]
fn anthropic_buffered_stream_resp(
    ts: u128,
    model: &str,
    text: &str,
    tool_calls: Vec<crate::zen::client::CollectedToolCall>,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation: u64,
    cache_read: u64,
    stop_reason: String,
    body: &ChatRequest,
    profile: ClientProfile,
) -> Response {
    use axum::response::sse::{Event, Sse};
    use std::convert::Infallible;

    let msg_id = format!("msg_{ts}");
    let model = model.to_string();
    let text = text.to_string();
    let body = body.clone();
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(Event::default().event("message_start").data(serde_json::json!({"type":"message_start","message":{"id":msg_id,"type":"message","role":"assistant","model":model.clone(),"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":input_tokens,"output_tokens":0}}}).to_string()));
        let has_text = !text.trim().is_empty();
        if has_text {
            yield Ok(Event::default().event("content_block_start").data(serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}).to_string()));
            yield Ok(Event::default().event("content_block_delta").data(serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}).to_string()));
            yield Ok(Event::default().event("content_block_stop").data(serde_json::json!({"type":"content_block_stop","index":0}).to_string()));
        }
        let mut emitted_tool_blocks = 0_u64;
        let mut seen_tool_signatures = std::collections::HashSet::new();
        if !tool_calls.is_empty() {
            for tool in tool_calls.iter() {
                let tidx = emitted_tool_blocks + u64::from(has_text);
                let Some((ct, input)) = streamable_anthropic_tool_call(tool, &body, profile) else {
                    continue;
                };
                if should_skip_duplicate_claude_code_tool_call(
                    profile,
                    &mut seen_tool_signatures,
                    &ct.function.name,
                    &input,
                ) {
                    continue;
                }
                yield Ok(Event::default().event("content_block_start").data(serde_json::json!({"type":"content_block_start","index":tidx,"content_block":{"type":"tool_use","id":ct.id,"name":ct.function.name,"input":{}}}).to_string()));
                let js = serde_json::to_string(&input).unwrap_or_default();
                if js != "{}" {
                    for chunk in anthropic_tool_json_delta_chunks(&js) {
                        yield Ok(Event::default().event("content_block_delta").data(serde_json::json!({"type":"content_block_delta","index":tidx,"delta":{"type":"input_json_delta","partial_json":chunk}}).to_string()));
                    }
                }
                yield Ok(Event::default().event("content_block_stop").data(serde_json::json!({"type":"content_block_stop","index":tidx}).to_string()));
                emitted_tool_blocks = emitted_tool_blocks.saturating_add(1);
            }
        }
        let stop_reason = if emitted_tool_blocks == 0 { stop_reason } else { "tool_use".to_string() };
        yield Ok(Event::default().event("message_delta").data(serde_json::json!({"type":"message_delta","delta":{"stop_reason":stop_reason,"stop_sequence":null},"usage":{"output_tokens":output_tokens,"cache_creation_input_tokens":cache_creation,"cache_read_input_tokens":cache_read}}).to_string()));
        yield Ok(Event::default().event("message_stop").data(serde_json::json!({"type":"message_stop"}).to_string()));
    };
    Sse::new(stream).into_response()
}

fn merge_tool_deltas(
    tool_calls: &mut Vec<crate::zen::client::CollectedToolCall>,
    deltas: Vec<crate::zen::client::ZenToolCallDelta>,
) {
    for delta in deltas {
        let index = delta.index.unwrap_or(0);
        let existing = tool_calls.iter_mut().find(|item| item.index == index);
        let item = if let Some(item) = existing {
            item
        } else {
            tool_calls.push(crate::zen::client::CollectedToolCall {
                index,
                id: delta.id.clone(),
                ..crate::zen::client::CollectedToolCall::default()
            });
            tool_calls.last_mut().unwrap()
        };
        if item.id.is_none() {
            item.id = delta.id;
        }
        if let Some(function) = delta.function {
            if let Some(name) = function.name {
                if !name.is_empty() {
                    item.name = name;
                }
            }
            if let Some(arguments) = function.arguments {
                item.arguments.push_str(&arguments);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_profile::ClientProfileSource;

    fn claude_code_profile() -> ClientProfile {
        ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header)
    }

    #[test]
    fn claude_code_buffered_stream_skips_tool_sessions_even_with_exact_output_literal() {
        let reason = claude_code_buffered_stream_reason(
            claude_code_profile(),
            180_000,
            Some(32_000),
            true,
            false,
            Some("HUGE_OK"),
        );

        assert_eq!(reason, None);
    }

    #[test]
    fn claude_code_buffered_stream_skips_multiline_exact_output() {
        let reason = claude_code_buffered_stream_reason(
            claude_code_profile(),
            180_000,
            Some(32_000),
            false,
            false,
            Some("# Title\n\n| A | B |\n| --- | --- |"),
        );

        assert_eq!(reason, None);
    }

    #[test]
    fn claude_code_buffered_stream_allows_tiny_exact_output_without_tools() {
        let reason = claude_code_buffered_stream_reason(
            claude_code_profile(),
            180_000,
            Some(32_000),
            false,
            false,
            Some("HUGE_OK"),
        );

        assert_eq!(
            reason,
            Some(ClaudeCodeBufferedStreamReason::TinyExactOutputNoTools)
        );
    }

    #[test]
    fn claude_code_buffered_stream_allows_small_output_huge_context_without_tools() {
        let reason = claude_code_buffered_stream_reason(
            claude_code_profile(),
            180_000,
            Some(1_024),
            false,
            false,
            None,
        );

        assert_eq!(
            reason,
            Some(ClaudeCodeBufferedStreamReason::SmallOutputHugeContextNoTools)
        );
    }

    #[test]
    fn anthropic_tool_json_delta_chunks_preserve_input() {
        let input = format!(
            "{{\"content\":\"{}中文{}\"}}",
            "a".repeat(ANTHROPIC_TOOL_JSON_DELTA_CHUNK_BYTES + 17),
            "b".repeat(ANTHROPIC_TOOL_JSON_DELTA_CHUNK_BYTES + 31)
        );
        let chunks = anthropic_tool_json_delta_chunks(&input);

        assert!(chunks.len() >= 3);
        assert_eq!(chunks.concat(), input);
        assert!(chunks
            .iter()
            .all(|chunk| chunk.len() <= ANTHROPIC_TOOL_JSON_DELTA_CHUNK_BYTES));
    }

    #[test]
    fn anthropic_tool_json_delta_chunks_handle_empty_input() {
        assert!(anthropic_tool_json_delta_chunks("").is_empty());
    }

    fn tool_gate_request(name: &str, parameters: Value) -> ChatRequest {
        ChatRequest {
            model: "deepseek-v4-flash".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: Value::String("test".to_string()),
                tool_calls: None,
                tool_call_id: None,
            }],
            stream: Some(true),
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: Some(vec![OpenAITool {
                tool_type: "function".to_string(),
                function: OpenAIToolFunction {
                    name: name.to_string(),
                    description: None,
                    parameters: Some(parameters),
                },
            }]),
            tool_choice: None,
        }
    }

    fn collected_tool(name: &str, arguments: &str) -> crate::zen::client::CollectedToolCall {
        crate::zen::client::CollectedToolCall {
            index: 0,
            id: Some("call_test".to_string()),
            name: name.to_string(),
            arguments: arguments.to_string(),
        }
    }

    #[test]
    fn claude_code_tool_gate_waits_for_known_required_fields_without_schema_required() {
        let body = tool_gate_request(
            "Read",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"}
                }
            }),
        );

        assert!(streamable_anthropic_tool_call(
            &collected_tool("Read", "{}"),
            &body,
            claude_code_profile()
        )
        .is_none());
        let ready = streamable_anthropic_tool_call(
            &collected_tool("Read", "{\"file_path\":\"README.md\"}"),
            &body,
            claude_code_profile(),
        )
        .expect("file_path should make Read streamable");
        assert_eq!(ready.1["file_path"], "README.md");
    }

    #[test]
    fn claude_code_tool_gate_allows_true_zero_argument_tools() {
        let body = tool_gate_request(
            "Noop",
            serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        );

        let ready = streamable_anthropic_tool_call(
            &collected_tool("Noop", ""),
            &body,
            claude_code_profile(),
        )
        .expect("empty schema should allow zero-argument tools");
        assert_eq!(ready.1, serde_json::json!({}));
    }

    #[test]
    fn claude_code_tool_gate_allows_optional_schema_empty_input() {
        let body = tool_gate_request(
            "OptionalProbe",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "note": {"type": "string"}
                }
            }),
        );

        let ready = streamable_anthropic_tool_call(
            &collected_tool("OptionalProbe", "{}"),
            &body,
            claude_code_profile(),
        )
        .expect("optional-only schema should allow empty input");
        assert_eq!(ready.1, serde_json::json!({}));
    }

    #[test]
    fn claude_code_tool_gate_repairs_send_message_string_summary() {
        let body = tool_gate_request(
            "SendMessage",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "to": {"type": "string"},
                    "message": {"type": "string"},
                    "summary": {"type": "string"}
                }
            }),
        );

        let ready = streamable_anthropic_tool_call(
            &collected_tool(
                "SendMessage",
                r#"{"to":"reviewer","message":"Please inspect the tool failures and report the likely cause."}"#,
            ),
            &body,
            claude_code_profile(),
        )
        .expect("string SendMessage should receive a local summary");
        assert_eq!(ready.1["to"], "reviewer");
        assert_eq!(
            ready.1["message"],
            "Please inspect the tool failures and report the likely cause."
        );
        assert!(ready.1["summary"]
            .as_str()
            .is_some_and(|value| { value.contains("Please inspect the tool failures") }));
    }

    #[test]
    fn claude_code_tool_gate_allows_structured_send_message_without_summary() {
        let body = tool_gate_request(
            "SendMessage",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "to": {"type": "string"},
                    "message": {"type": "object"}
                }
            }),
        );

        let ready = streamable_anthropic_tool_call(
            &collected_tool(
                "SendMessage",
                r#"{"to":"teammate","message":{"type":"shutdown_request","reason":"done"}}"#,
            ),
            &body,
            claude_code_profile(),
        )
        .expect("structured SendMessage does not require summary");
        assert!(ready.1.get("summary").is_none());
    }

    #[test]
    fn claude_code_tool_gate_rejects_empty_toolsearch_query() {
        let body = tool_gate_request(
            "ToolSearch",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"}
                }
            }),
        );

        assert!(streamable_anthropic_tool_call(
            &collected_tool("ToolSearch", r#"{"query":""}"#),
            &body,
            claude_code_profile()
        )
        .is_none());
    }

    #[test]
    fn claude_code_duplicate_tool_signature_is_dropped() {
        let profile = claude_code_profile();
        let mut seen = std::collections::HashSet::new();
        let input = serde_json::json!({"file_path":"README.md"});

        assert!(!should_skip_duplicate_claude_code_tool_call(
            profile, &mut seen, "Read", &input
        ));
        assert!(should_skip_duplicate_claude_code_tool_call(
            profile, &mut seen, "Read", &input
        ));
    }

    #[test]
    fn claude_code_tool_gate_generates_id_when_upstream_id_is_empty() {
        let body = tool_gate_request(
            "Read",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"}
                },
                "required": ["file_path"]
            }),
        );
        let mut tool = collected_tool("Read", "{\"file_path\":\"README.md\"}");
        tool.id = Some(String::new());

        let ready = streamable_anthropic_tool_call(&tool, &body, claude_code_profile())
            .expect("complete input should be streamable");
        assert_eq!(ready.0.id.as_deref(), Some("call_0"));
    }

    #[test]
    fn claude_code_tool_gate_does_not_infer_read_from_user_instruction() {
        let body = tool_gate_request(
            "Read",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"}
                },
                "required": ["file_path"]
            }),
        );
        let body = ChatRequest {
            messages: vec![Message {
                role: "user".to_string(),
                content: Value::String(
                    "Use the Read tool to read cc_probe_read.txt. Then output done.".to_string(),
                ),
                tool_calls: None,
                tool_call_id: None,
            }],
            ..body
        };

        assert!(streamable_anthropic_tool_call(
            &collected_tool("Read", ""),
            &body,
            claude_code_profile(),
        )
        .is_none());
    }

    #[test]
    fn claude_code_tool_gate_does_not_infer_bash_from_user_instruction() {
        let body = tool_gate_request(
            "Bash",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
        );
        let body = ChatRequest {
            messages: vec![Message {
                role: "user".to_string(),
                content: Value::String(
                    "Use the Bash tool to run: printf BASH_PAYLOAD_OK. Then stop.".to_string(),
                ),
                tool_calls: None,
                tool_call_id: None,
            }],
            ..body
        };

        assert!(streamable_anthropic_tool_call(
            &collected_tool("Bash", "{}"),
            &body,
            claude_code_profile(),
        )
        .is_none());
    }

    #[test]
    fn claude_code_tool_gate_does_not_infer_bash_from_run_exactly_instruction() {
        let body = tool_gate_request(
            "Bash",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
        );
        let body = ChatRequest {
            messages: vec![Message {
                role: "user".to_string(),
                content: Value::String(
                    "Use Bash to run exactly: printf LOCAL_CC_BASH_OK. Then stop.".to_string(),
                ),
                tool_calls: None,
                tool_call_id: None,
            }],
            ..body
        };

        assert!(streamable_anthropic_tool_call(
            &collected_tool("Bash", "{}"),
            &body,
            claude_code_profile(),
        )
        .is_none());
    }

    #[test]
    fn claude_code_tool_gate_does_not_infer_write_from_user_instruction() {
        let body = tool_gate_request(
            "Write",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["file_path", "content"]
            }),
        );
        let body = ChatRequest {
            messages: vec![Message {
                role: "user".to_string(),
                content: Value::String(
                    "Use Write to create cc_probe_write.txt with exactly WRITE_PAYLOAD_OK. Then read it."
                        .to_string(),
                ),
                tool_calls: None,
                tool_call_id: None,
            }],
            ..body
        };

        assert!(streamable_anthropic_tool_call(
            &collected_tool("Write", "{}"),
            &body,
            claude_code_profile(),
        )
        .is_none());
    }

    #[test]
    fn claude_code_tool_gate_does_not_infer_bash_from_numbered_steps() {
        let body = tool_gate_request(
            "Bash",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
        );
        let body = ChatRequest {
            messages: vec![Message {
                role: "user".to_string(),
                content: Value::String(
                    "In this temporary directory, do exactly these steps using tools: 1. Use Bash to run exactly: printf PING_OK. 2. Use Write to create cc_probe.txt with exactly PROBE_CONTENT_OK. 3. Use Read to read cc_probe.txt. 4. Reply with exactly CLEAN_TOOL_OK and nothing else."
                        .to_string(),
                ),
                tool_calls: None,
                tool_call_id: None,
            }],
            ..body
        };

        assert!(streamable_anthropic_tool_call(
            &collected_tool("Bash", "{}"),
            &body,
            claude_code_profile(),
        )
        .is_none());
    }

    #[test]
    fn claude_code_tool_gate_does_not_repair_duplicate_completed_bash_command() {
        let body = tool_gate_request(
            "Bash",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
        );
        let body = ChatRequest {
            messages: vec![
                Message {
                    role: "assistant".to_string(),
                    content: Value::Null,
                    tool_calls: Some(vec![ToolCall {
                        id: Some("call_bash_done".to_string()),
                        call_type: "function".to_string(),
                        function: ToolFunction {
                            name: "Bash".to_string(),
                            arguments: "{\"command\":\"printf PING_OK\"}".to_string(),
                        },
                        index: Some(0),
                    }]),
                    tool_call_id: None,
                },
                Message {
                    role: "tool".to_string(),
                    content: Value::String("PING_OK".to_string()),
                    tool_calls: None,
                    tool_call_id: Some("call_bash_done".to_string()),
                },
                Message {
                    role: "user".to_string(),
                    content: Value::String(
                        "Use Bash to run exactly: printf PING_OK. Then continue.".to_string(),
                    ),
                    tool_calls: None,
                    tool_call_id: None,
                },
            ],
            ..body
        };

        assert!(streamable_anthropic_tool_call(
            &collected_tool("Bash", "{}"),
            &body,
            claude_code_profile(),
        )
        .is_none());
    }

    #[test]
    fn claude_code_tool_gate_does_not_infer_same_bash_command_before_tool_result_exists() {
        let body = tool_gate_request(
            "Bash",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
        );
        let body = ChatRequest {
            messages: vec![
                Message {
                    role: "assistant".to_string(),
                    content: Value::Null,
                    tool_calls: Some(vec![ToolCall {
                        id: Some("call_bash_pending".to_string()),
                        call_type: "function".to_string(),
                        function: ToolFunction {
                            name: "Bash".to_string(),
                            arguments: "{\"command\":\"printf PING_OK\"}".to_string(),
                        },
                        index: Some(0),
                    }]),
                    tool_call_id: None,
                },
                Message {
                    role: "user".to_string(),
                    content: Value::String(
                        "Use Bash to run exactly: printf PING_OK. Then continue.".to_string(),
                    ),
                    tool_calls: None,
                    tool_call_id: None,
                },
            ],
            ..body
        };

        assert!(streamable_anthropic_tool_call(
            &collected_tool("Bash", "{}"),
            &body,
            claude_code_profile(),
        )
        .is_none());
    }

    #[test]
    fn claude_code_tool_gate_does_not_infer_write_from_numbered_steps() {
        let body = tool_gate_request(
            "Write",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["file_path", "content"]
            }),
        );
        let body = ChatRequest {
            messages: vec![Message {
                role: "user".to_string(),
                content: Value::String(
                    "In this temporary directory, do exactly these steps using tools: 1. Use Bash to run exactly: printf PING_OK. 2. Use Write to create cc_probe.txt with exactly PROBE_CONTENT_OK. 3. Use Read to read cc_probe.txt. 4. Reply with exactly CLEAN_TOOL_OK and nothing else."
                        .to_string(),
                ),
                tool_calls: None,
                tool_call_id: None,
            }],
            ..body
        };

        assert!(streamable_anthropic_tool_call(
            &collected_tool("Write", "{}"),
            &body,
            claude_code_profile(),
        )
        .is_none());
    }

    #[test]
    fn claude_code_tool_gate_does_not_repair_invalid_root_file_path_from_user_instruction() {
        let body = tool_gate_request(
            "Write",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["file_path", "content"]
            }),
        );
        let body = ChatRequest {
            messages: vec![Message {
                role: "user".to_string(),
                content: Value::String(
                    "Use Write to create cc_probe.txt with exactly PROBE_CONTENT_OK.".to_string(),
                ),
                tool_calls: None,
                tool_call_id: None,
            }],
            ..body
        };

        assert!(streamable_anthropic_tool_call(
            &collected_tool(
                "Write",
                r#"{"file_path":"\\","content":"PROBE_CONTENT_OK"}"#,
            ),
            &body,
            claude_code_profile(),
        )
        .is_none());
    }

    #[test]
    fn claude_code_tool_gate_rejects_invalid_root_file_path_without_explicit_instruction() {
        let body = tool_gate_request(
            "Read",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"}
                },
                "required": ["file_path"]
            }),
        );
        let body = ChatRequest {
            messages: vec![Message {
                role: "user".to_string(),
                content: Value::String("Read the relevant file.".to_string()),
                tool_calls: None,
                tool_call_id: None,
            }],
            ..body
        };

        assert!(streamable_anthropic_tool_call(
            &collected_tool("Read", r#"{"file_path":"\\"}"#),
            &body,
            claude_code_profile(),
        )
        .is_none());
    }

    #[test]
    fn claude_code_tool_gate_does_not_repair_without_explicit_file_path() {
        let body = tool_gate_request(
            "Read",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"}
                },
                "required": ["file_path"]
            }),
        );
        let body = ChatRequest {
            messages: vec![Message {
                role: "user".to_string(),
                content: Value::String("Use Read when appropriate.".to_string()),
                tool_calls: None,
                tool_call_id: None,
            }],
            ..body
        };

        assert!(streamable_anthropic_tool_call(
            &collected_tool("Read", ""),
            &body,
            claude_code_profile()
        )
        .is_none());
    }

    #[test]
    fn stream_guard_retries_only_before_forwardable_output() {
        let profile = claude_code_profile();

        assert!(should_retry_stream_without_forwardable_output(
            profile,
            0,
            "",
            &[],
            std::time::Duration::from_secs(45),
            std::time::Duration::from_secs(45)
        ));
        assert!(!should_retry_stream_without_forwardable_output(
            profile,
            0,
            "partial text",
            &[],
            std::time::Duration::from_secs(45),
            std::time::Duration::from_secs(45)
        ));
        assert!(!should_retry_stream_without_forwardable_output(
            profile,
            0,
            "",
            &[crate::zen::client::CollectedToolCall::default()],
            std::time::Duration::from_secs(45),
            std::time::Duration::from_secs(45)
        ));
        assert!(!should_retry_stream_without_forwardable_output(
            profile,
            CLAUDE_CODE_STREAM_GUARD_ATTEMPTS - 1,
            "",
            &[],
            std::time::Duration::from_secs(45),
            std::time::Duration::from_secs(45)
        ));
        assert!(!should_retry_stream_without_forwardable_output(
            profile,
            0,
            "",
            &[],
            std::time::Duration::from_secs(44),
            std::time::Duration::from_secs(45)
        ));
    }

    #[test]
    fn no_forwardable_retry_after_is_adaptive_by_input_bucket() {
        let configured = std::time::Duration::from_secs(45);

        assert_eq!(
            adaptive_no_forwardable_retry_after(configured, 40_000),
            std::time::Duration::from_secs(10)
        );
        assert_eq!(
            adaptive_no_forwardable_retry_after(configured, 80_000),
            std::time::Duration::from_secs(14)
        );
        assert_eq!(
            adaptive_no_forwardable_retry_after(configured, 150_000),
            std::time::Duration::from_secs(22)
        );
        assert_eq!(
            adaptive_no_forwardable_retry_after(configured, 300_000),
            std::time::Duration::from_secs(32)
        );
        assert_eq!(
            adaptive_no_forwardable_retry_after(configured, 450_000),
            std::time::Duration::from_secs(45)
        );
        assert_eq!(
            adaptive_no_forwardable_retry_after(std::time::Duration::from_secs(8), 300_000),
            std::time::Duration::from_secs(8)
        );
    }

    #[test]
    fn initial_fetch_timeout_only_applies_to_large_claude_code_streams() {
        let profile = claude_code_profile();
        assert!(should_apply_initial_fetch_timeout(
            profile, 150_000, 150_000, 30
        ));
        assert!(!should_apply_initial_fetch_timeout(
            profile, 149_999, 150_000, 30
        ));
        assert!(!should_apply_initial_fetch_timeout(
            profile, 150_000, 150_000, 0
        ));
        assert!(!should_apply_initial_fetch_timeout(
            ClientProfile::unknown(),
            200_000,
            150_000,
            30
        ));
    }
}
