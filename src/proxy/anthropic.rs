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
    let msgs = translate::anthropic_to_openai_messages(&body);
    let tools: Vec<OpenAITool> = body
        .tools
        .as_ref()
        .map(|t| translate::anthropic_tools_to_openai(t))
        .unwrap_or_default();
    let max_tok = body.max_tokens.max(32);
    let zb = serde_json::json!({"model":model,"messages":msgs,"stream":true,"max_tokens":max_tok,"temperature":body.temperature,"tools":if tools.is_empty(){Value::Null}else{serde_json::to_value(&tools).unwrap_or_default()}});
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
    let (content, reasoning, _usage) = crate::zen::client::collect_stream_text(resp).await?;
    let has_tools = translate::has_tools(cr);
    let prompt = translate::build_prompt_text(&cr.messages);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    if content.trim().is_empty() && has_tools {
        if let Some(tc) = synthesis::tool::synthesize_tool_call(cr) {
            let tc = synthesis::tool::complete_tool_call(&tc, cr);
            let input: Value = serde_json::from_str(&tc.function.arguments).unwrap_or_default();
            return Ok(tool_resp(ts, &cr.model, &tc, &input, estimate(&prompt)));
        }
    }
    if content.trim().is_empty() && !reasoning.trim().is_empty() {
        if has_tools {
            if let Some(tc) = synthesis::tool::synthesize_tool_call(cr) {
                let tc = synthesis::tool::complete_tool_call(&tc, cr);
                let input: Value = serde_json::from_str(&tc.function.arguments).unwrap_or_default();
                return Ok(tool_resp(ts, &cr.model, &tc, &input, estimate(&prompt)));
            }
        } else {
            let fb = synthesis::text::synthesize_text_fallback(&prompt);
            return Ok(
                text_resp(ts, &cr.model, &fb, estimate(&prompt), estimate(&fb)).into_response(),
            );
        }
    }
    Ok(text_resp(
        ts,
        &cr.model,
        &content,
        estimate(&prompt),
        estimate(&content),
    ))
}

fn text_resp(ts: u128, model: &str, text: &str, input_tokens: u64, output_tokens: u64) -> Response {
    Json(serde_json::json!({"id":format!("msg_{ts}"),"type":"message","role":"assistant","model":model,"content":[{"type":"text","text":text}],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":input_tokens,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":output_tokens}})).into_response()
}

fn tool_resp(ts: u128, model: &str, tc: &ToolCall, input: &Value, input_tokens: u64) -> Response {
    Json(serde_json::json!({"id":format!("msg_{ts}"),"type":"message","role":"assistant","model":model,"content":[{"type":"tool_use","id":tc.id,"name":tc.function.name,"input":input}],"stop_reason":"tool_use","stop_sequence":null,"usage":{"input_tokens":input_tokens,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":1}})).into_response()
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
    let has_tools = translate::has_tools(cr);
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
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(Event::default().event("message_start").data(serde_json::json!({"type":"message_start","message":{"id":msg_id,"type":"message","role":"assistant","model":m,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}}}).to_string()));
        let mut text = String::new();
        let mut tcs: Vec<(i64,String,String,Option<String>)> = Vec::new();
        let mut synthesized = false;
        while let Some(ev) = event_stream.next().await {
            let Ok(ev) = ev else { return; };
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
        } else if text.is_empty() && has_tools {
            synthesized = true;
            if let Some(tc)=synthesis::tool::synthesize_tool_call(&body) {
                let ct=synthesis::tool::complete_tool_call(&tc,&body);
                let input:Value=serde_json::from_str(&ct.function.arguments).unwrap_or_default();
                let js=serde_json::to_string(&input).unwrap_or_default();
                yield Ok(Event::default().event("content_block_start").data(serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":ct.id,"name":ct.function.name,"input":{}}}).to_string()));
                if js!="{}" { yield Ok(Event::default().event("content_block_delta").data(serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":js}}).to_string())); }
                yield Ok(Event::default().event("content_block_stop").data(serde_json::json!({"type":"content_block_stop","index":0}).to_string()));
            }
        } else if text.is_empty() {
            let fb=synthesis::text::synthesize_text_fallback(&translate::build_prompt_text(&body.messages));
            yield Ok(Event::default().event("content_block_start").data(serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}).to_string()));
            yield Ok(Event::default().event("content_block_delta").data(serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":fb}}).to_string()));
            yield Ok(Event::default().event("content_block_stop").data(serde_json::json!({"type":"content_block_stop","index":0}).to_string()));
        }
        let stop_reason = if !tcs.is_empty() || synthesized { "tool_use" } else { "end_turn" };
        yield Ok(Event::default().event("message_delta").data(serde_json::json!({"type":"message_delta","delta":{"stop_reason":stop_reason,"stop_sequence":null},"usage":{"output_tokens":1}}).to_string()));
        yield Ok(Event::default().event("message_stop").data(serde_json::json!({"type":"message_stop"}).to_string()));
    };
    Ok(Sse::new(stream).into_response())
}
