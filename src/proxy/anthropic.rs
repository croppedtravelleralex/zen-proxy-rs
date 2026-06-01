use crate::client_profile::{ClientKind, ClientProfile};
use crate::error::AppError;
use crate::kernel::KernelConfig;
use crate::protocol::translate::estimate_tokens as estimate;
use crate::protocol::{translate, types::*};
use crate::synthesis;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::StreamExt;
use reqwest::Client;
use serde_json::Value;

pub async fn handle_anthropic_messages(
    client: &Client,
    config: &KernelConfig,
    body: AnthropicRequest,
    profile: ClientProfile,
) -> Result<Response, AppError> {
    let model = translate::normalize_model(&body.model);
    let upstream_model = translate::map_upstream_model(&model, &config.model_mappings);
    let mut msgs = translate::anthropic_to_openai_messages(&body);
    let stream_requested = body.stream.unwrap_or(false);
    let context_repair = if stream_requested {
        let policy = if profile.kind == ClientKind::ClaudeCode {
            translate::StreamContextPolicy::claude_code_huge_context()
        } else {
            translate::StreamContextPolicy::default()
        };
        translate::compact_stream_context_with_policy(&mut msgs, policy)
    } else {
        translate::StreamContextRepair::default()
    };
    let appended_latest_user_anchor = stream_requested
        && profile.kind == ClientKind::ClaudeCode
        && context_repair.compacted_messages > 0
        && translate::append_latest_user_anchor_message(&mut msgs, 2 * 1024);
    if context_repair.compacted_messages > 0 {
        tracing::warn!(
            before_tokens = context_repair.before_tokens,
            after_tokens = context_repair.after_tokens,
            compacted_messages = context_repair.compacted_messages,
            appended_latest_user_anchor,
            "compacted streaming anthropic context before upstream"
        );
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
    let tools: Vec<OpenAITool> = body
        .tools
        .as_ref()
        .map(|t| translate::anthropic_tools_to_openai(t))
        .unwrap_or_default();
    let max_tok = if stream_requested {
        let policy_prompt_tokens = context_repair.before_tokens.max(translate::estimate_tokens(
            &translate::build_prompt_text(&msgs),
        ));
        let policy = translate::stream_output_policy_for_prompt_tokens(
            policy_prompt_tokens,
            Some(body.max_tokens),
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
        let policy = translate::non_stream_output_policy(&msgs, Some(body.max_tokens));
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
    let tool_choice = body
        .tool_choice
        .as_ref()
        .map(translate::anthropic_tool_choice_to_openai);
    let mut zb = serde_json::json!({"model":upstream_model,"messages":msgs,"stream":true,"max_tokens":max_tok,"temperature":body.temperature,"tools":if tools.is_empty(){Value::Null}else{serde_json::to_value(&tools).unwrap_or_default()},"tool_choice":tool_choice});
    if profile.disables_thinking_for_tool_use() {
        translate::disable_thinking_for_tool_use(&mut zb);
    }
    let cr = ChatRequest {
        model: model.clone(),
        messages: msgs,
        stream: Some(stream_requested),
        max_tokens: Some(max_tok),
        temperature: body.temperature,
        top_p: None,
        tools: if tools.is_empty() { None } else { Some(tools) },
        tool_choice,
    };
    if translate::is_short_no_tool_health_request(&cr) {
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
        handle_stream(client, config, &cr, &zb, profile).await
    } else {
        handle_non_stream(client, config, &cr, &zb).await
    }
}

async fn handle_non_stream(
    client: &Client,
    config: &KernelConfig,
    cr: &ChatRequest,
    zb: &Value,
) -> Result<Response, AppError> {
    let resp = crate::zen::client::fetch_zen_stream_with_headers(
        client,
        &config.zen_chat_url,
        &config.zen_api_key,
        zb,
        &config.extra_headers,
    )
    .await?;
    let observed_exit_ip = resp.headers().get("x-zen-observed-exit-ip").cloned();
    let collected = crate::zen::client::collect_stream_parts(resp).await?;
    let content = crate::proxy::markdown::MarkdownFenceGuard::repair_text(
        &crate::redact::redact_text(&collected.content),
    );
    let prompt = translate::build_prompt_text(&cr.messages);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    if content.trim().is_empty() && collected.tool_calls.is_empty() {
        return Err(AppError::empty_upstream());
    }
    if !collected.tool_calls.is_empty() {
        let blocks = collected
            .tool_calls
            .iter()
            .filter(|tool| !tool.name.is_empty())
            .map(|tool| AnthropicContentBlock {
                block_type: "tool_use".to_string(),
                text: None,
                id: Some(
                    tool.id
                        .clone()
                        .filter(|id| !id.is_empty())
                        .unwrap_or_else(|| format!("call_{}", tool.index)),
                ),
                name: Some(tool.name.clone()),
                input: Some(serde_json::from_str(&tool.arguments).unwrap_or(Value::Null)),
            })
            .collect::<Vec<_>>();
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
                tool_resp(ts, &cr.model, blocks, input_tokens, output_tokens),
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
        text_resp(ts, &cr.model, &content, input_tokens, output_tokens),
        observed_exit_ip,
    ))
}

fn text_resp(ts: u128, model: &str, text: &str, input_tokens: u64, output_tokens: u64) -> Response {
    Json(serde_json::json!({"id":format!("msg_{ts}"),"type":"message","role":"assistant","model":model,"content":[{"type":"text","text":text}],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":input_tokens,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":output_tokens}})).into_response()
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

fn tool_resp(
    ts: u128,
    model: &str,
    blocks: Vec<AnthropicContentBlock>,
    input_tokens: u64,
    output_tokens: u64,
) -> Response {
    Json(serde_json::json!({"id":format!("msg_{ts}"),"type":"message","role":"assistant","model":model,"content":blocks,"stop_reason":"tool_use","stop_sequence":null,"usage":{"input_tokens":input_tokens,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":output_tokens}})).into_response()
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

async fn handle_stream(
    client: &Client,
    config: &KernelConfig,
    cr: &ChatRequest,
    zb: &Value,
    profile: ClientProfile,
) -> Result<Response, AppError> {
    use axum::response::sse::{Event, Sse};
    use std::convert::Infallible;

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
    let prompt = translate::build_prompt_text(&body.messages);
    let estimated_input_tokens = estimate(&prompt).max(1);
    let initial_input_tokens = estimated_input_tokens;
    if profile.kind == ClientKind::ClaudeCode && cr.max_tokens.unwrap_or(0) <= 512 {
        return handle_buffered_claude_code_huge_stream(
            client,
            config,
            cr,
            zb,
            estimated_input_tokens,
            profile,
        )
        .await;
    }
    let resp = crate::zen::client::fetch_zen_stream_with_headers(
        client,
        &config.zen_chat_url,
        &config.zen_api_key,
        zb,
        &config.extra_headers,
    )
    .await?;
    let mut upstream = Box::pin(crate::zen::client::stream_sse_events(resp.bytes_stream()));
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(Event::default().event("message_start").data(serde_json::json!({"type":"message_start","message":{"id":msg_id,"type":"message","role":"assistant","model":m,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":initial_input_tokens,"output_tokens":0}}}).to_string()));
        let mut text = String::new();
        let mut text_block_open = false;
        let mut markdown_guard = crate::proxy::markdown::MarkdownFenceGuard::new();
        let mut tool_calls: Vec<crate::zen::client::CollectedToolCall> = Vec::new();
        let mut usage: Option<crate::zen::client::ZenUsage> = None;
        while let Some(event) = upstream.next().await {
            let event = match event {
                Ok(event) => event,
                Err(err) => {
                    yield Ok(Event::default().event("error").data(serde_json::json!({"type":"error","error":{"type":"api_error","message":err.message}}).to_string()));
                    return;
                }
            };
            if event.usage.is_some() {
                usage = event.usage;
            }
            if let Some(choices) = event.choices {
                for choice in choices {
                    let Some(delta) = choice.delta else { continue; };
                    if let Some(content) = delta.content {
                        let content = crate::redact::redact_text(&content);
                        let content = markdown_guard.push(&content);
                        let should_emit =
                            !content.trim().is_empty()
                                || (profile.preserves_stream_whitespace() && !content.is_empty());
                        if should_emit {
                            if !text_block_open {
                                text_block_open = true;
                                yield Ok(Event::default().event("content_block_start").data(serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}).to_string()));
                            }
                            text.push_str(&content);
                            yield Ok(Event::default().event("content_block_delta").data(serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":content}}).to_string()));
                        }
                    }
                    if let Some(items) = delta.tool_calls {
                        merge_tool_deltas(&mut tool_calls, items);
                    }
                }
            }
        }
        let final_markdown = markdown_guard.finish();
        if !final_markdown.is_empty() {
            if !text_block_open {
                text_block_open = true;
                yield Ok(Event::default().event("content_block_start").data(serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}).to_string()));
            }
            text.push_str(&final_markdown);
            yield Ok(Event::default().event("content_block_delta").data(serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":final_markdown}}).to_string()));
        }
        if text.trim().is_empty() && tool_calls.is_empty() {
            yield Ok(Event::default().event("error").data(serde_json::json!({"type":"error","error":{"type":"api_error","message":"upstream returned no assistant content or tool call"}}).to_string()));
            return;
        }
        if text_block_open {
            yield Ok(Event::default().event("content_block_stop").data(serde_json::json!({"type":"content_block_stop","index":0}).to_string()));
        }
        if !tool_calls.is_empty() {
            for (ti,tool) in tool_calls.iter().enumerate() {
                let tidx = ti as u64 + u64::from(text_block_open);
                let clean_id = tool.id.clone().unwrap_or_else(||format!("call_{}", tool.index));
                let clean_id = if let Some(pos) = clean_id.find('{') { clean_id[..pos].to_string() } else { clean_id };
                let tc=ToolCall{id:Some(clean_id),call_type:"function".into(),function:ToolFunction{name:tool.name.clone(),arguments:tool.arguments.clone()},index:Some(tool.index)};
                let ct=if profile.uses_compat_tool_history() { synthesis::tool::complete_tool_call(&tc,&body) } else { tc };
                let input:Value=serde_json::from_str(&ct.function.arguments).unwrap_or_default();
                yield Ok(Event::default().event("content_block_start").data(serde_json::json!({"type":"content_block_start","index":tidx,"content_block":{"type":"tool_use","id":ct.id,"name":ct.function.name,"input":{}}}).to_string()));
                let js=serde_json::to_string(&input).unwrap_or_default();
                if js!="{}" { yield Ok(Event::default().event("content_block_delta").data(serde_json::json!({"type":"content_block_delta","index":tidx,"delta":{"type":"input_json_delta","partial_json":js}}).to_string())); }
                yield Ok(Event::default().event("content_block_stop").data(serde_json::json!({"type":"content_block_stop","index":tidx}).to_string()));
            }
        }
        let stop_reason = if !tool_calls.is_empty() { "tool_use" } else { "end_turn" };
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
            .and_then(|usage| usage.cache_read_input_tokens)
            .or_else(|| {
                usage
                    .as_ref()
                    .and_then(|usage| usage.prompt_tokens_details.as_ref())
                    .and_then(|details| details.get("cached_tokens"))
                    .and_then(|value| value.as_u64())
            })
            .unwrap_or(0);
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
) -> Result<Response, AppError> {
    const ATTEMPTS: usize = 2;

    for attempt in 0..ATTEMPTS {
        let resp = match crate::zen::client::fetch_zen_stream_with_headers(
            client,
            &config.zen_chat_url,
            &config.zen_api_key,
            zb,
            &config.extra_headers,
        )
        .await
        {
            Ok(resp) => resp,
            Err(err) => {
                tracing::warn!(
                    attempt,
                    max_attempts = ATTEMPTS,
                    error = %err.message,
                    "ClaudeCode huge stream buffered fetch failed"
                );
                if attempt + 1 >= ATTEMPTS {
                    return Err(err);
                }
                continue;
            }
        };

        let collected = match crate::zen::client::collect_stream_parts(resp).await {
            Ok(collected) => collected,
            Err(err) => {
                tracing::warn!(
                    attempt,
                    max_attempts = ATTEMPTS,
                    error = %err.message,
                    "ClaudeCode huge stream buffered collection failed"
                );
                if attempt + 1 >= ATTEMPTS {
                    return Err(err);
                }
                continue;
            }
        };
        let content = crate::proxy::markdown::MarkdownFenceGuard::repair_text(
            &crate::redact::redact_text(&collected.content),
        );
        if content.trim().is_empty() && collected.tool_calls.is_empty() {
            tracing::warn!(
                attempt,
                max_attempts = ATTEMPTS,
                "ClaudeCode huge stream buffered upstream returned empty output"
            );
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
            .and_then(|usage| usage.cache_read_input_tokens)
            .or_else(|| {
                collected
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.prompt_tokens_details.as_ref())
                    .and_then(|details| details.get("cached_tokens"))
                    .and_then(|value| value.as_u64())
            })
            .unwrap_or(0);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        return Ok(anthropic_buffered_stream_resp(
            ts,
            &cr.model,
            &content,
            collected.tool_calls,
            input_tokens,
            output_tokens,
            cache_creation,
            cache_read,
            cr,
            profile,
        ));
    }

    Err(AppError::empty_upstream())
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
        if !tool_calls.is_empty() {
            for (ti, tool) in tool_calls.iter().enumerate() {
                let tidx = ti as u64 + u64::from(has_text);
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
                let ct = if profile.uses_compat_tool_history() {
                    synthesis::tool::complete_tool_call(&tc, &body)
                } else {
                    tc
                };
                let input: Value = serde_json::from_str(&ct.function.arguments).unwrap_or_default();
                yield Ok(Event::default().event("content_block_start").data(serde_json::json!({"type":"content_block_start","index":tidx,"content_block":{"type":"tool_use","id":ct.id,"name":ct.function.name,"input":{}}}).to_string()));
                let js = serde_json::to_string(&input).unwrap_or_default();
                if js != "{}" {
                    yield Ok(Event::default().event("content_block_delta").data(serde_json::json!({"type":"content_block_delta","index":tidx,"delta":{"type":"input_json_delta","partial_json":js}}).to_string()));
                }
                yield Ok(Event::default().event("content_block_stop").data(serde_json::json!({"type":"content_block_stop","index":tidx}).to_string()));
            }
        }
        let stop_reason = if tool_calls.is_empty() { "end_turn" } else { "tool_use" };
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
