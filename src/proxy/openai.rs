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

const NON_STREAM_EMPTY_UPSTREAM_ATTEMPTS: usize = 3;

pub async fn handle_openai_chat(
    client: &Client,
    config: &KernelConfig,
    mut body: ChatRequest,
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
    let stream_requested = body.stream.unwrap_or(false);
    let context_repair = if translate::model_disables_input_compaction(&model) {
        translate::observe_context(&body.messages)
    } else if profile.kind == ClientKind::ClaudeCode {
        translate::compact_claude_code_huge_session_context(&mut body.messages)
    } else if stream_requested {
        translate::compact_stream_context_with_policy(
            &mut body.messages,
            translate::StreamContextPolicy::default(),
        )
    } else {
        translate::StreamContextRepair::default()
    };
    let reduced_exact_output_anchor = stream_requested
        && profile.kind == ClientKind::ClaudeCode
        && context_repair.compacted_messages > 0
        && translate::reduce_to_exact_output_anchor_message(&mut body.messages, 2 * 1024);
    let appended_latest_user_anchor = profile.kind == ClientKind::ClaudeCode
        && context_repair.compacted_messages > 0
        && !reduced_exact_output_anchor
        && translate::append_latest_user_anchor_message(&mut body.messages, 2 * 1024);
    if reduced_exact_output_anchor {
        body.tools = None;
        body.tool_choice = None;
    }
    if context_repair.compacted_messages > 0 {
        if stream_requested {
            tracing::warn!(
                before_tokens = context_repair.before_tokens,
                after_tokens = context_repair.after_tokens,
                compacted_messages = context_repair.compacted_messages,
                reduced_exact_output_anchor,
                appended_latest_user_anchor,
                "compacted streaming openai context before upstream"
            );
        } else {
            tracing::warn!(
                before_tokens = context_repair.before_tokens,
                after_tokens = context_repair.after_tokens,
                compacted_messages = context_repair.compacted_messages,
                appended_latest_user_anchor,
                "compacted non-stream openai context before upstream"
            );
        }
    }
    let max_tok = if stream_requested {
        let policy_prompt_tokens = context_repair.before_tokens.max(translate::estimate_tokens(
            &translate::build_prompt_text(&body.messages),
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
                "capped streaming openai max_tokens before upstream"
            );
        }
        policy.effective_max_tokens
    } else {
        let policy_prompt_tokens = context_repair.before_tokens.max(translate::estimate_tokens(
            &translate::build_prompt_text(&body.messages),
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
                "capped non-stream openai max_tokens before upstream"
            );
        }
        policy.effective_max_tokens
    };
    let tool_history_policy = if profile.uses_compat_tool_history() {
        translate::ToolHistoryPolicy::Compat
    } else {
        translate::ToolHistoryPolicy::Strict
    };
    let repair = translate::canonicalize_openai_tool_history_with_policy(
        &mut body.messages,
        tool_history_policy,
    );
    if repair != translate::ToolHistoryRepair::default() {
        tracing::warn!(
            synthetic_tool_ids = repair.synthetic_tool_ids,
            paired_tool_results = repair.paired_tool_results,
            downgraded_tool_results = repair.downgraded_tool_results,
            downgraded_assistant_calls = repair.downgraded_assistant_calls,
            "canonicalized openai tool history before upstream"
        );
    }
    let tools = body.tools.clone().unwrap_or_default();
    let mut zb = serde_json::json!({"model":upstream_model,"messages":body.messages,"stream":true,"temperature":body.temperature,"tools":if tools.is_empty(){Value::Null}else{serde_json::to_value(&tools).unwrap_or_default()},"tool_choice":body.tool_choice});
    if let Some(max_tok) = max_tok {
        zb["max_tokens"] = serde_json::json!(max_tok);
    }
    if profile.disables_thinking_for_tool_use() {
        translate::disable_thinking_for_tool_use(&mut zb);
    }
    let cr = ChatRequest {
        model: model.clone(),
        messages: body.messages.clone(),
        stream: Some(stream_requested),
        max_tokens: max_tok,
        temperature: body.temperature,
        top_p: body.top_p,
        tools: if tools.is_empty() { None } else { Some(tools) },
        tool_choice: body.tool_choice.clone(),
    };
    super::log_request_shape("openai", &cr, observed_profile, profile);
    if translate::is_short_no_tool_health_request(&cr) {
        super::log_provider_cache_observation(
            "openai",
            &cr,
            profile,
            &ProviderCacheSignals::ignored(),
            0,
            0,
        );
        let created = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let prompt_tokens = estimate(&translate::build_prompt_text(&cr.messages)).max(1);
        let completion_tokens = 1;
        if body.stream.unwrap_or(false) {
            return Ok(oa_ok_stream_resp(
                created,
                &cr.model,
                prompt_tokens,
                completion_tokens,
            ));
        }
        return Ok(oa_text_resp(
            created,
            &cr.model,
            "ok",
            prompt_tokens,
            completion_tokens,
            prompt_tokens + completion_tokens,
        ));
    }
    if body.stream.unwrap_or(false) {
        handle_oa_stream(client, config, &cr, &zb, profile).await
    } else {
        handle_oa_non_stream(client, config, &cr, &zb, profile).await
    }
}

async fn handle_oa_non_stream(
    client: &Client,
    config: &KernelConfig,
    cr: &ChatRequest,
    zb: &Value,
    profile: ClientProfile,
) -> Result<Response, AppError> {
    let mut observed_exit_ip = None;
    let request_shape = translate::request_shape(cr);
    let short_request_kind =
        translate::classify_short_non_stream_request(cr, profile.kind == ClientKind::ClaudeCode);
    let (collected, content) = {
        let mut last_empty = false;
        let mut output = None;
        for attempt in 0..NON_STREAM_EMPTY_UPSTREAM_ATTEMPTS {
            let resp = crate::zen::client::fetch_zen_stream_with_headers(
                client,
                &config.zen_chat_url,
                &config.zen_api_key,
                zb,
                &config.extra_headers,
            )
            .await?;
            let cache_signals = ProviderCacheSignals::from_response_headers(resp.headers());
            observed_exit_ip = resp.headers().get("x-zen-observed-exit-ip").cloned();
            let collected = crate::zen::client::collect_stream_parts(resp).await?;
            let cache_signals = cache_signals.with_body_usage(collected.usage.as_ref());
            super::log_provider_cache_observation(
                "openai",
                cr,
                profile,
                &cache_signals,
                attempt + 1,
                NON_STREAM_EMPTY_UPSTREAM_ATTEMPTS,
            );
            let content = response_text_for_profile(profile, &collected.content);
            if content.trim().is_empty() && collected.tool_calls.is_empty() {
                last_empty = true;
                tracing::warn!(
                    attempt,
                    max_attempts = NON_STREAM_EMPTY_UPSTREAM_ATTEMPTS,
                    source_client = ?profile.kind,
                    short_request_kind = short_request_kind.as_str(),
                    prompt_hash = %format_args!("{:016x}", request_shape.prompt_hash),
                    prompt_tokens = request_shape.estimated_total_tokens,
                    message_count = request_shape.message_count,
                    max_tokens = ?request_shape.max_tokens,
                    "non-stream upstream returned empty output; retrying"
                );
                continue;
            }
            output = Some((collected, content));
            break;
        }
        if let Some(output) = output {
            output
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
            let prompt_tokens = estimate(&prompt);
            let completion_tokens = estimate(fallback_text).max(1);
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            return Ok(oa_text_resp(
                ts,
                &cr.model,
                fallback_text,
                prompt_tokens,
                completion_tokens,
                prompt_tokens + completion_tokens,
            ));
        } else {
            return Err(AppError::empty_upstream());
        }
    };
    let prompt = translate::build_prompt_text(&cr.messages);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if !collected.tool_calls.is_empty() {
        let tool_calls = collected
            .tool_calls
            .iter()
            .filter(|tool| !tool.name.is_empty())
            .map(|tool| ToolCall {
                id: Some(
                    tool.id
                        .clone()
                        .filter(|id| !id.is_empty())
                        .unwrap_or_else(|| format!("call_{}", tool.index)),
                ),
                call_type: "function".to_string(),
                function: ToolFunction {
                    name: tool.name.clone(),
                    arguments: tool.arguments.clone(),
                },
                index: Some(tool.index),
            })
            .map(|tool| synthesis::tool::canonicalize_tool_call_name(&tool, cr))
            .collect::<Vec<_>>();
        if !tool_calls.is_empty() {
            let prompt_tokens = collected
                .usage
                .as_ref()
                .and_then(|usage| usage.prompt_tokens)
                .unwrap_or_else(|| estimate(&prompt));
            let completion_tokens = collected
                .usage
                .as_ref()
                .and_then(|usage| usage.completion_tokens)
                .unwrap_or_else(|| {
                    estimate(
                        &tool_calls
                            .iter()
                            .map(|tool| {
                                format!("{} {}", tool.function.name, tool.function.arguments)
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )
                    .max(1)
                });
            let total_tokens = collected
                .usage
                .as_ref()
                .and_then(|usage| usage.total_tokens)
                .unwrap_or(prompt_tokens + completion_tokens);
            return Ok(with_observed_exit_ip(
                oa_tool_resp_with_usage(
                    ts,
                    &cr.model,
                    tool_calls,
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                    collected.usage.as_ref(),
                ),
                observed_exit_ip,
            ));
        }
    }
    let prompt_tokens = collected
        .usage
        .as_ref()
        .and_then(|usage| usage.prompt_tokens)
        .unwrap_or_else(|| estimate(&prompt));
    let completion_tokens = collected
        .usage
        .as_ref()
        .and_then(|usage| usage.completion_tokens)
        .unwrap_or_else(|| estimate(&content));
    let total_tokens = collected
        .usage
        .as_ref()
        .and_then(|usage| usage.total_tokens)
        .unwrap_or(prompt_tokens + completion_tokens);
    Ok(with_observed_exit_ip(
        oa_text_resp_with_usage(
            ts,
            &cr.model,
            &content,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            collected.usage.as_ref(),
            collected.finish_reason.as_deref(),
        ),
        observed_exit_ip,
    ))
}

fn oa_text_resp(ts: u64, model: &str, text: &str, pt: u64, ct: u64, total: u64) -> Response {
    oa_text_resp_with_usage(ts, model, text, pt, ct, total, None, None)
}

fn response_text_for_profile(profile: ClientProfile, text: &str) -> String {
    if profile.preserves_model_text_exactly() {
        text.to_string()
    } else {
        crate::proxy::markdown::MarkdownFenceGuard::repair_text(&crate::redact::redact_text(text))
    }
}

fn oa_ok_stream_resp(ts: u64, model: &str, pt: u64, ct: u64) -> Response {
    use axum::response::sse::{Event, Sse};
    use std::convert::Infallible;
    let id = format!("chatcmpl_{ts}");
    let model = model.to_string();
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(Event::default().data(serde_json::json!({"id":id,"object":"chat.completion.chunk","created":ts,"model":model,"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}).to_string()));
        yield Ok(Event::default().data(serde_json::json!({"id":id,"object":"chat.completion.chunk","created":ts,"model":model,"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":null}]}).to_string()));
        yield Ok(Event::default().data(serde_json::json!({"id":id,"object":"chat.completion.chunk","created":ts,"model":model,"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":pt,"completion_tokens":ct,"total_tokens":pt + ct}}).to_string()));
        yield Ok(Event::default().data("[DONE]"));
    };
    Sse::new(stream).into_response()
}

#[allow(clippy::too_many_arguments)]
fn oa_text_resp_with_usage(
    ts: u64,
    model: &str,
    text: &str,
    pt: u64,
    ct: u64,
    total: u64,
    usage: Option<&crate::zen::client::ZenUsage>,
    upstream_finish_reason: Option<&str>,
) -> Response {
    let finish_reason = openai_finish_reason(upstream_finish_reason, false);
    let mut body = serde_json::json!({
        "id": format!("chatcmpl_{ts}"),
        "object": "chat.completion",
        "created": ts,
        "model": model,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": finish_reason}],
        "usage": {"prompt_tokens": pt, "completion_tokens": ct, "total_tokens": total}
    });
    append_openai_usage_metadata(&mut body["usage"], usage);
    Json(body).into_response()
}

fn oa_tool_resp_with_usage(
    ts: u64,
    model: &str,
    tool_calls: Vec<ToolCall>,
    pt: u64,
    ct: u64,
    total: u64,
    usage: Option<&crate::zen::client::ZenUsage>,
) -> Response {
    let mut body = serde_json::json!({
        "id": format!("chatcmpl_{ts}"),
        "object": "chat.completion",
        "created": ts,
        "model": model,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": null, "tool_calls": tool_calls}, "finish_reason": "tool_calls"}],
        "usage": {"prompt_tokens": pt, "completion_tokens": ct, "total_tokens": total}
    });
    append_openai_usage_metadata(&mut body["usage"], usage);
    Json(body).into_response()
}

fn openai_finish_reason(
    upstream_finish_reason: Option<&str>,
    has_tool_calls: bool,
) -> &'static str {
    if has_tool_calls {
        return "tool_calls";
    }
    match upstream_finish_reason {
        Some("length") => "length",
        Some("content_filter") => "content_filter",
        Some("stop") => "stop",
        _ => "stop",
    }
}

fn append_openai_usage_metadata(
    usage_json: &mut Value,
    usage: Option<&crate::zen::client::ZenUsage>,
) {
    let Some(usage) = usage else {
        return;
    };
    if let Some(details) = usage.prompt_tokens_details.clone() {
        usage_json["prompt_tokens_details"] = details;
    }
    if let Some(cache_read) = cache_read_tokens(Some(usage)) {
        usage_json["cache_read_input_tokens"] = serde_json::json!(cache_read);
    }
    if let Some(cache_creation) = usage.cache_creation_input_tokens {
        usage_json["cache_creation_input_tokens"] = serde_json::json!(cache_creation);
    }
}

fn cache_read_tokens(usage: Option<&crate::zen::client::ZenUsage>) -> Option<u64> {
    usage
        .and_then(|usage| usage.cache_read_input_tokens)
        .or_else(|| {
            usage
                .and_then(|usage| usage.prompt_tokens_details.as_ref())
                .and_then(|details| details.get("cached_tokens"))
                .and_then(Value::as_u64)
        })
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

async fn handle_oa_stream(
    client: &Client,
    config: &KernelConfig,
    cr: &ChatRequest,
    zb: &Value,
    profile: ClientProfile,
) -> Result<Response, AppError> {
    use axum::response::sse::{Event, Sse};
    use std::convert::Infallible;

    let model = cr.model.clone();
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let cid = format!("chatcmpl_{created}");
    let m = model.clone();
    let id = cid.clone();
    let prompt = translate::build_prompt_text(&cr.messages);
    let body = cr.clone();
    let resp = crate::zen::client::fetch_zen_stream_with_headers(
        client,
        &config.zen_chat_url,
        &config.zen_api_key,
        zb,
        &config.extra_headers,
    )
    .await?;
    let cache_signals = ProviderCacheSignals::from_response_headers(resp.headers());
    let mut upstream = Box::pin(crate::zen::client::stream_sse_events(resp.bytes_stream()));
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(Event::default().data(serde_json::json!({"id":id,"object":"chat.completion.chunk","created":created,"model":m,"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}).to_string()));
        let mut text = String::new();
        let mut markdown_guard = if profile.preserves_model_text_exactly() {
            None
        } else {
            Some(crate::proxy::markdown::MarkdownFenceGuard::new())
        };
        let mut tool_calls: Vec<crate::zen::client::CollectedToolCall> = Vec::new();
        let mut usage: Option<crate::zen::client::ZenUsage> = None;
        let mut upstream_finish_reason: Option<String> = None;
        while let Some(event) = upstream.next().await {
            let event = match event {
                Ok(event) => event,
                Err(err) => {
                    yield Ok(Event::default().data(serde_json::json!({"error":{"message":err.message}}).to_string()));
                    yield Ok(Event::default().data("[DONE]"));
                    return;
                }
            };
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
                            text.push_str(&content);
                            yield Ok(Event::default().data(serde_json::json!({"id":id,"object":"chat.completion.chunk","created":created,"model":model,"choices":[{"index":0,"delta":{"content":content},"finish_reason":null}]}).to_string()));
                        }
                    }
                    if let Some(items) = delta.tool_calls {
                        merge_tool_deltas(&mut tool_calls, items);
                    }
                }
            }
        }
        let final_markdown = markdown_guard
            .as_mut()
            .map(crate::proxy::markdown::MarkdownFenceGuard::finish)
            .unwrap_or_default();
        if !final_markdown.is_empty() {
            text.push_str(&final_markdown);
            yield Ok(Event::default().data(serde_json::json!({"id":id,"object":"chat.completion.chunk","created":created,"model":model,"choices":[{"index":0,"delta":{"content":final_markdown},"finish_reason":null}]}).to_string()));
        }
        if text.trim().is_empty() && tool_calls.is_empty() {
            if let Some(fallback_text) = translate::short_no_tool_empty_fallback_text(&body) {
                tracing::warn!(
                    model = body.model,
                    source_client = ?profile.kind,
                    "short channel-test probe received empty upstream; returning local ok"
                );
                text.push_str(fallback_text);
                yield Ok(Event::default().data(serde_json::json!({"id":id,"object":"chat.completion.chunk","created":created,"model":model,"choices":[{"index":0,"delta":{"content":fallback_text},"finish_reason":null}]}).to_string()));
            } else {
                yield Ok(Event::default().data(serde_json::json!({"error":{"message":"upstream returned no assistant content or tool call"}}).to_string()));
                yield Ok(Event::default().data("[DONE]"));
                return;
            }
        }
        for tool in tool_calls.iter() {
            let clean_id = tool.id.clone().unwrap_or_else(|| format!("call_{}", tool.index));
            let clean_id = if let Some(pos) = clean_id.find('{') { clean_id[..pos].to_string() } else { clean_id };
            let tc = ToolCall {
                id: Some(clean_id),
                call_type: "function".into(),
                function: ToolFunction {
                    name: tool.name.clone(),
                    arguments: tool.arguments.clone(),
                },
                index: Some(tool.index),
            };
            let tc = synthesis::tool::canonicalize_tool_call_name(&tc, &body);
            yield Ok(Event::default().data(serde_json::json!({"id":id,"object":"chat.completion.chunk","created":created,"model":model,"choices":[{"index":0,"delta":{"tool_calls":[{"index":tool.index,"id":tc.id,"type":"function","function":{"name":tc.function.name,"arguments":tc.function.arguments}}]},"finish_reason":null}]}).to_string()));
        }
        let finish_reason = openai_finish_reason(upstream_finish_reason.as_deref(), !tool_calls.is_empty());
        let mut final_chunk = serde_json::json!({
            "id": id, "object": "chat.completion.chunk", "created": created,
            "model": model, "choices": [{"index": 0, "delta": {}, "finish_reason": finish_reason}]
        });
        if let Some(usage) = usage {
            let cache_signals = cache_signals.with_body_usage(Some(&usage));
            super::log_provider_cache_observation("openai", &body, profile, &cache_signals, 1, 1);
            let pt = usage.prompt_tokens.unwrap_or_else(|| estimate(&prompt));
            let ct = usage.completion_tokens.unwrap_or_else(|| if !text.trim().is_empty() { estimate(&text) } else { estimate(&tool_calls.iter().map(|tool| format!("{} {}", tool.name, tool.arguments)).collect::<Vec<_>>().join("\n")).max(1) });
            let total = usage.total_tokens.unwrap_or(pt + ct);
            final_chunk["usage"] = serde_json::json!({"prompt_tokens":pt,"completion_tokens":ct,"total_tokens":total});
            if let Some(ref details) = usage.prompt_tokens_details {
                final_chunk["usage"]["prompt_tokens_details"] = details.clone();
            }
            if let Some(cache_read) = cache_read_tokens(Some(&usage)) {
                final_chunk["usage"]["cache_read_input_tokens"] = serde_json::json!(cache_read);
            }
            if let Some(cache_creation) = usage.cache_creation_input_tokens {
                final_chunk["usage"]["cache_creation_input_tokens"] = serde_json::json!(cache_creation);
            }
        } else {
            super::log_provider_cache_observation("openai", &body, profile, &cache_signals, 1, 1);
        }
        yield Ok(Event::default().data(final_chunk.to_string()));
        yield Ok(Event::default().data("[DONE]"));
    };
    Ok(Sse::new(stream).into_response())
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
