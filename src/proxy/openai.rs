use crate::error::AppError;
use crate::kernel::KernelConfig;
use crate::protocol::translate::estimate_tokens as estimate;
use crate::protocol::{translate, types::*};
use axum::response::{IntoResponse, Response};
use axum::Json;
use reqwest::Client;
use serde_json::Value;

pub async fn handle_openai_chat(
    client: &Client,
    config: &KernelConfig,
    body: ChatRequest,
) -> Result<Response, AppError> {
    let model = translate::normalize_model(&body.model);
    let upstream_model = translate::map_upstream_model(&model, &config.model_mappings);
    let tools = body.tools.clone().unwrap_or_default();
    let max_tok = body.max_tokens.unwrap_or(1024).max(32);
    let mut zb = serde_json::json!({"model":upstream_model,"messages":body.messages,"stream":true,"max_tokens":max_tok,"temperature":body.temperature,"tools":if tools.is_empty(){Value::Null}else{serde_json::to_value(&tools).unwrap_or_default()},"tool_choice":body.tool_choice});
    translate::disable_thinking_for_assistant_history(&mut zb, &body.messages);
    translate::disable_thinking_for_tool_use(&mut zb);
    let cr = ChatRequest {
        model: model.clone(),
        messages: body.messages.clone(),
        stream: Some(true),
        max_tokens: Some(max_tok),
        temperature: body.temperature,
        top_p: body.top_p,
        tools: if tools.is_empty() { None } else { Some(tools) },
        tool_choice: body.tool_choice.clone(),
    };
    if body.stream.unwrap_or(false) {
        handle_oa_stream(client, config, &cr, &zb).await
    } else {
        handle_oa_non_stream(client, config, &cr, &zb).await
    }
}

async fn handle_oa_non_stream(
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
    let content = crate::redact::redact_text(&collected.content);
    let prompt = translate::build_prompt_text(&cr.messages);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if content.trim().is_empty() && collected.tool_calls.is_empty() {
        return Err(AppError::empty_upstream());
    }
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
                oa_tool_resp(
                    ts,
                    &cr.model,
                    tool_calls,
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
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
        oa_text_resp(
            ts,
            &cr.model,
            &content,
            prompt_tokens,
            completion_tokens,
            total_tokens,
        ),
        observed_exit_ip,
    ))
}

fn oa_text_resp(ts: u64, model: &str, text: &str, pt: u64, ct: u64, total: u64) -> Response {
    Json(serde_json::json!({"id":format!("chatcmpl_{ts}"),"object":"chat.completion","created":ts,"model":model,"choices":[{"index":0,"message":{"role":"assistant","content":text},"finish_reason":"stop"}],"usage":{"prompt_tokens":pt,"completion_tokens":ct,"total_tokens":total}})).into_response()
}

fn oa_tool_resp(
    ts: u64,
    model: &str,
    tool_calls: Vec<ToolCall>,
    pt: u64,
    ct: u64,
    total: u64,
) -> Response {
    Json(serde_json::json!({"id":format!("chatcmpl_{ts}"),"object":"chat.completion","created":ts,"model":model,"choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":tool_calls},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":pt,"completion_tokens":ct,"total_tokens":total}})).into_response()
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
) -> Result<Response, AppError> {
    use axum::response::sse::{Event, Sse};
    use futures::StreamExt;
    use std::convert::Infallible;
    let resp = crate::zen::client::fetch_zen_stream_with_headers(
        client,
        &config.zen_chat_url,
        &config.zen_api_key,
        zb,
        &config.extra_headers,
    )
    .await?;
    let byte_stream = resp.bytes_stream();
    let mut event_stream = Box::pin(crate::zen::client::stream_sse_events(byte_stream));
    let model = cr.model.clone();
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let cid = format!("chatcmpl_{created}");
    let m = model.clone();
    let id = cid.clone();
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(Event::default().data(serde_json::json!({"id":id,"object":"chat.completion.chunk","created":created,"model":m,"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}).to_string()));
        let mut text = String::new();
        let mut tcs: Vec<(i64,String,String,Option<String>)> = Vec::new();
        while let Some(ev) = event_stream.next().await {
            let ev = match ev {
                Ok(ev) => ev,
                Err(err) => {
                    yield Ok(Event::default().data(serde_json::json!({"error":{"type":"stream_error","message":err.message}}).to_string()));
                    yield Ok(Event::default().data("[DONE]"));
                    return;
                }
            };
            if let Some(ref chs) = ev.choices { for ch in chs { if let Some(ref d) = ch.delta {
                if let Some(ref c) = d.content { if !c.is_empty() {
                    text.push_str(c);
                }}
                if let Some(ref td) = d.tool_calls { for tc in td {
                    let idx = tc.index.unwrap_or(0);
                    let n = tc.function.as_ref().and_then(|f| f.name.clone()).unwrap_or_default();
                    let a = tc.function.as_ref().and_then(|f| f.arguments.clone()).unwrap_or_default();
                    if let Some(e) = tcs.iter_mut().find(|(i,_,_,_)| *i==idx) { e.2.push_str(&a); }
                    else if !n.is_empty()||!a.is_empty() { let clean_id = tc.id.clone().unwrap_or_default();
                let clean_id = if let Some(pos) = clean_id.find("{") { clean_id[..pos].to_string() } else { clean_id };
                tcs.push((idx,n.clone(),a.clone(),Some(clean_id))); }
                    yield Ok(Event::default().data(serde_json::json!({"id":id,"object":"chat.completion.chunk","created":created,"model":model,"choices":[{"index":0,"delta":{"tool_calls":[{"index":idx,"id":tc.id,"type":"function","function":{"name":n,"arguments":a}}]},"finish_reason":null}]}).to_string()));
                }}
            }}}
        }
        if !text.is_empty() {
            text = crate::redact::redact_text(&text);
            yield Ok(Event::default().data(serde_json::json!({"id":id,"object":"chat.completion.chunk","created":created,"model":model,"choices":[{"index":0,"delta":{"content":text},"finish_reason":null}]}).to_string()));
        }
        if text.is_empty() && tcs.is_empty() {
            yield Ok(Event::default().data(serde_json::json!({"error":{"type":"empty_output","message":"upstream returned no assistant content or tool call"}}).to_string()));
        }
        let finish_reason = if !tcs.is_empty() { "tool_calls" } else { "stop" };
        yield Ok(Event::default().data(serde_json::json!({
            "id": id, "object": "chat.completion.chunk", "created": created,
            "model": model, "choices": [{"index": 0, "delta": {}, "finish_reason": finish_reason}]
        }).to_string()));
        yield Ok(Event::default().data("[DONE]"));
    };
    Ok(Sse::new(stream).into_response())
}
