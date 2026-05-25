use crate::error::AppError;
use crate::kernel::KernelConfig;
use crate::protocol::translate::estimate_tokens as estimate;
use crate::protocol::{translate, types::*};
use crate::synthesis;
use axum::response::{IntoResponse, Response};
use axum::Json;
use reqwest::Client;
use serde_json::Value;

pub async fn handle_anthropic_messages(
    client: &Client,
    config: &KernelConfig,
    body: AnthropicRequest,
) -> Result<Response, AppError> {
    let model = translate::normalize_model(&body.model);
    let upstream_model = translate::map_upstream_model(&model, &config.model_mappings);
    let msgs = translate::anthropic_to_openai_messages(&body);
    let tools: Vec<OpenAITool> = body
        .tools
        .as_ref()
        .map(|t| translate::anthropic_tools_to_openai(t))
        .unwrap_or_default();
    let max_tok = body.max_tokens.max(32);
    let tool_choice = body
        .tool_choice
        .as_ref()
        .map(translate::anthropic_tool_choice_to_openai);
    let mut zb = serde_json::json!({"model":upstream_model,"messages":msgs,"stream":true,"max_tokens":max_tok,"temperature":body.temperature,"tools":if tools.is_empty(){Value::Null}else{serde_json::to_value(&tools).unwrap_or_default()},"tool_choice":tool_choice});
    translate::disable_thinking_for_assistant_history(&mut zb, &msgs);
    translate::disable_thinking_for_tool_use(&mut zb);
    translate::stabilize_short_user_prompt(&mut zb);
    let cr = ChatRequest {
        model: model.clone(),
        messages: msgs,
        stream: Some(true),
        max_tokens: Some(max_tok),
        temperature: body.temperature,
        top_p: None,
        tools: if tools.is_empty() { None } else { Some(tools) },
        tool_choice,
    };
    if body.stream.unwrap_or(false) {
        handle_stream(client, config, &cr, &zb).await
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
    let content = crate::redact::redact_text(&collected.content);
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
) -> Result<Response, AppError> {
    use axum::response::sse::{Event, Sse};
    use std::convert::Infallible;
    let resp = crate::zen::client::fetch_zen_stream_with_headers(
        client,
        &config.zen_chat_url,
        &config.zen_api_key,
        zb,
        &config.extra_headers,
    )
    .await?;
    let collected = crate::zen::client::collect_stream_parts(resp).await?;
    let text = crate::redact::redact_text(&collected.content);
    if text.trim().is_empty() && collected.tool_calls.is_empty() {
        return Err(AppError::empty_upstream());
    }
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
            if !text.trim().is_empty() {
                estimate(&text)
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
    let tool_calls = collected.tool_calls;
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(Event::default().event("message_start").data(serde_json::json!({"type":"message_start","message":{"id":msg_id,"type":"message","role":"assistant","model":m,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":input_tokens,"output_tokens":0}}}).to_string()));
        if !text.trim().is_empty() {
            yield Ok(Event::default().event("content_block_start").data(serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}).to_string()));
            yield Ok(Event::default().event("content_block_delta").data(serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}).to_string()));
            yield Ok(Event::default().event("content_block_stop").data(serde_json::json!({"type":"content_block_stop","index":0}).to_string()));
        }
        if !tool_calls.is_empty() {
            for (ti,tool) in tool_calls.iter().enumerate() {
                let tidx=ti as u64;
                let clean_id = tool.id.clone().unwrap_or_else(||format!("call_{}", tool.index));
                let clean_id = if let Some(pos) = clean_id.find('{') { clean_id[..pos].to_string() } else { clean_id };
                let tc=ToolCall{id:Some(clean_id),call_type:"function".into(),function:ToolFunction{name:tool.name.clone(),arguments:tool.arguments.clone()},index:Some(tool.index)};
                let ct=synthesis::tool::complete_tool_call(&tc,&body);
                let input:Value=serde_json::from_str(&ct.function.arguments).unwrap_or_default();
                yield Ok(Event::default().event("content_block_start").data(serde_json::json!({"type":"content_block_start","index":tidx,"content_block":{"type":"tool_use","id":ct.id,"name":ct.function.name,"input":{}}}).to_string()));
                let js=serde_json::to_string(&input).unwrap_or_default();
                if js!="{}" { yield Ok(Event::default().event("content_block_delta").data(serde_json::json!({"type":"content_block_delta","index":tidx,"delta":{"type":"input_json_delta","partial_json":js}}).to_string())); }
                yield Ok(Event::default().event("content_block_stop").data(serde_json::json!({"type":"content_block_stop","index":tidx}).to_string()));
            }
        }
        let stop_reason = if !tool_calls.is_empty() { "tool_use" } else { "end_turn" };
        yield Ok(Event::default().event("message_delta").data(serde_json::json!({"type":"message_delta","delta":{"stop_reason":stop_reason,"stop_sequence":null},"usage":{"output_tokens":output_tokens}}).to_string()));
        yield Ok(Event::default().event("message_stop").data(serde_json::json!({"type":"message_stop"}).to_string()));
    };
    Ok(Sse::new(stream).into_response())
}
