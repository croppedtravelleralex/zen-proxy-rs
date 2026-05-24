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
    let zb = serde_json::json!({"model":upstream_model,"messages":msgs,"stream":true,"max_tokens":max_tok,"temperature":body.temperature,"tools":if tools.is_empty(){Value::Null}else{serde_json::to_value(&tools).unwrap_or_default()}});
    let cr = ChatRequest {
        model: model.clone(),
        messages: msgs,
        stream: Some(true),
        max_tokens: Some(max_tok),
        temperature: body.temperature,
        top_p: None,
        tools: if tools.is_empty() { None } else { Some(tools) },
        tool_choice: None,
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
    let (content, _reasoning, usage) = crate::zen::client::collect_stream_text(resp).await?;
    let prompt = translate::build_prompt_text(&cr.messages);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    if content.trim().is_empty() {
        let fb = synthesis::text::synthesize_text_fallback(&prompt);
        return Ok(with_observed_exit_ip(
            text_resp(ts, &cr.model, &fb, estimate(&prompt), estimate(&fb)).into_response(),
            observed_exit_ip,
        ));
    }
    let input_tokens = usage
        .as_ref()
        .and_then(|usage| usage.prompt_tokens)
        .unwrap_or_else(|| estimate(&prompt));
    let output_tokens = usage
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
    let msg_id = format!(
        "msg_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    let body = cr.clone();
    let m = model.clone();
    let input_tokens = estimate(&translate::build_prompt_text(&body.messages));
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(Event::default().event("message_start").data(serde_json::json!({"type":"message_start","message":{"id":msg_id,"type":"message","role":"assistant","model":m,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":input_tokens,"output_tokens":0}}}).to_string()));
        let mut text = String::new();
        let mut tcs: Vec<(i64,String,String,Option<String>)> = Vec::new();
        while let Some(ev) = event_stream.next().await {
            let ev = match ev {
                Ok(ev) => ev,
                Err(err) => {
                    yield Ok(Event::default().event("error").data(serde_json::json!({"type":"error","error":{"type":"stream_error","message":err.message}}).to_string()));
                    yield Ok(Event::default().event("message_stop").data(serde_json::json!({"type":"message_stop"}).to_string()));
                    return;
                }
            };
            if let Some(ref chs) = ev.choices { for ch in chs { if let Some(ref d) = ch.delta {
                if let Some(ref c) = d.content { if !c.is_empty() {
                    if text.is_empty() { yield Ok(Event::default().event("content_block_start").data(serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}).to_string())); }
                    text.push_str(c);
                    yield Ok(Event::default().event("content_block_delta").data(serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":c}}).to_string()));
                }}
                if let Some(ref td) = d.tool_calls { for tc in td {
                    let idx = tc.index.unwrap_or(0);
                    let n = tc.function.as_ref().and_then(|f| f.name.clone()).unwrap_or_default();
                    let a = tc.function.as_ref().and_then(|f| f.arguments.clone()).unwrap_or_default();
                    if let Some(e) = tcs.iter_mut().find(|(i,_,_,_)| *i==idx) { e.2.push_str(&a); }
                    else if !n.is_empty()||!a.is_empty() { tcs.push((idx,n.clone(),a.clone(),Some(tc.id.clone().unwrap_or_default()))); }
                }}
            }}}
        }
        if !text.is_empty() { yield Ok(Event::default().event("content_block_stop").data(serde_json::json!({"type":"content_block_stop","index":0}).to_string())); }
        if !tcs.is_empty() {
            for (ti,(idx,name,args,cid)) in tcs.iter().enumerate() {
                let tidx=ti as u64;
                let clean_id = cid.clone().unwrap_or_else(||format!("call_{idx}"));
                let clean_id = if let Some(pos) = clean_id.find('{') { clean_id[..pos].to_string() } else { clean_id };
                let tc=ToolCall{id:Some(clean_id),call_type:"function".into(),function:ToolFunction{name:name.clone(),arguments:args.clone()},index:Some(*idx)};
                let ct=synthesis::tool::complete_tool_call(&tc,&body);
                let input:Value=serde_json::from_str(&ct.function.arguments).unwrap_or_default();
                yield Ok(Event::default().event("content_block_start").data(serde_json::json!({"type":"content_block_start","index":tidx,"content_block":{"type":"tool_use","id":ct.id,"name":ct.function.name,"input":{}}}).to_string()));
                let js=serde_json::to_string(&input).unwrap_or_default();
                if js!="{}" { yield Ok(Event::default().event("content_block_delta").data(serde_json::json!({"type":"content_block_delta","index":tidx,"delta":{"type":"input_json_delta","partial_json":js}}).to_string())); }
                yield Ok(Event::default().event("content_block_stop").data(serde_json::json!({"type":"content_block_stop","index":tidx}).to_string()));
            }
        } else if text.is_empty() {
            let fb=synthesis::text::synthesize_text_fallback(&translate::build_prompt_text(&body.messages));
            yield Ok(Event::default().event("content_block_start").data(serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}).to_string()));
            yield Ok(Event::default().event("content_block_delta").data(serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":fb}}).to_string()));
            yield Ok(Event::default().event("content_block_stop").data(serde_json::json!({"type":"content_block_stop","index":0}).to_string()));
            text = fb;
        }
        let stop_reason = if !tcs.is_empty() { "tool_use" } else { "end_turn" };
        let output_tokens = if !text.is_empty() {
            estimate(&text)
        } else if !tcs.is_empty() {
            estimate(&tcs.iter().map(|(_,name,args,_)| format!("{name} {args}")).collect::<Vec<_>>().join("\n")).max(1)
        } else {
            1
        };
        yield Ok(Event::default().event("message_delta").data(serde_json::json!({"type":"message_delta","delta":{"stop_reason":stop_reason,"stop_sequence":null},"usage":{"output_tokens":output_tokens}}).to_string()));
        yield Ok(Event::default().event("message_stop").data(serde_json::json!({"type":"message_stop"}).to_string()));
    };
    Ok(Sse::new(stream).into_response())
}
