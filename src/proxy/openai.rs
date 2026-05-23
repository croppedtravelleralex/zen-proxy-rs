use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;
use crate::error::AppError;
use crate::protocol::{translate, types::*};
use crate::routes::AppState;
use crate::synthesis;

pub async fn handle_openai_chat(state: &AppState, body: ChatRequest) -> Result<Response, AppError> {
    let model = translate::normalize_model(&body.model);
    let tools = body.tools.clone().unwrap_or_default();
    let max_tok = body.max_tokens.unwrap_or(1024).max(32);
    let zb = serde_json::json!({"model":model,"messages":body.messages,"stream":true,"max_tokens":max_tok,"temperature":body.temperature,"tools":if tools.is_empty(){Value::Null}else{serde_json::to_value(&tools).unwrap_or_default()}});
    let cr = ChatRequest{model:model.clone(),messages:body.messages.clone(),stream:Some(true),max_tokens:Some(max_tok),temperature:body.temperature,top_p:body.top_p,tools:if tools.is_empty(){None}else{Some(tools)},tool_choice:body.tool_choice.clone()};
    if body.stream.unwrap_or(false) { handle_oa_stream(state, &cr, &zb).await }
    else { handle_oa_non_stream(state, &cr, &zb).await }
}

async fn handle_oa_non_stream(state: &AppState, cr: &ChatRequest, zb: &Value) -> Result<Response, AppError> {
    let resp = crate::zen::client::fetch_zen_stream(&state.http_client, &state.config.zen_chat_url, &state.config.zen_api_key, zb).await?;
    let (content, reasoning, _u) = crate::zen::client::collect_stream_text(resp).await?;
    let has_tools = translate::has_tools(cr);
    let prompt = translate::build_prompt_text(&cr.messages);
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    if content.trim().is_empty() && has_tools {
        if let Some(tc) = synthesis::tool::synthesize_tool_call(cr) {
            let tc = synthesis::tool::complete_tool_call(&tc, cr);
            return Ok(oa_tool_resp(ts, &cr.model, &tc));
        }
    }
    if content.trim().is_empty() && !reasoning.trim().is_empty() {
        if has_tools {
            if let Some(tc) = synthesis::tool::synthesize_tool_call(cr) {
                let tc = synthesis::tool::complete_tool_call(&tc, cr);
                return Ok(oa_tool_resp(ts, &cr.model, &tc));
            }
        } else {
            return Ok(oa_text_resp(ts, &cr.model, &synthesis::text::synthesize_text_fallback(&prompt)));
        }
    }
    Ok(oa_text_resp(ts, &cr.model, &content))
}

fn oa_text_resp(ts: u64, model: &str, text: &str) -> Response {
    Json(serde_json::json!({"id":format!("chatcmpl_{ts}"),"object":"chat.completion","created":ts,"model":model,"choices":[{"index":0,"message":{"role":"assistant","content":text},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2},"cost":"0"})).into_response()
}

fn oa_tool_resp(ts: u64, model: &str, tc: &ToolCall) -> Response {
    Json(serde_json::json!({"id":format!("chatcmpl_{ts}"),"object":"chat.completion","created":ts,"model":model,"choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[tc]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2},"cost":"0"})).into_response()
}

async fn handle_oa_stream(state: &AppState, cr: &ChatRequest, zb: &Value) -> Result<Response, AppError> {
    use axum::response::sse::{Event, Sse};
    use std::convert::Infallible;
        use futures::StreamExt;
    let resp = crate::zen::client::fetch_zen_stream(&state.http_client, &state.config.zen_chat_url, &state.config.zen_api_key, zb).await?;
    let byte_stream = resp.bytes_stream();
    let mut event_stream = Box::pin(crate::zen::client::stream_sse_events(byte_stream));
    let has_tools = translate::has_tools(cr);
    let model = cr.model.clone();
    let created = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let cid = format!("chatcmpl_{created}");
    let body = cr.clone();
    let m = model.clone();
    let id = cid.clone();
    let stream = async_stream::stream! {
        yield Ok::<_, Infallible>(Event::default().data(serde_json::json!({"id":id,"object":"chat.completion.chunk","created":created,"model":m,"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}).to_string()));
        let mut text = String::new();
        let mut tcs: Vec<(i64,String,String,Option<String>)> = Vec::new();
        let mut _synthesized = false;
        while let Some(ev) = event_stream.next().await {
            let Ok(ev) = ev else { return; };
            if let Some(ref chs) = ev.choices { for ch in chs { if let Some(ref d) = ch.delta {
                if let Some(ref c) = d.content { if !c.is_empty() { text.push_str(c);
                    yield Ok(Event::default().data(serde_json::json!({"id":id,"object":"chat.completion.chunk","created":created,"model":model,"choices":[{"index":0,"delta":{"content":c},"finish_reason":null}]}).to_string()));
                }}
                if let Some(ref td) = d.tool_calls { for tc in td {
                    let idx = tc.index.unwrap_or(0);
                    let n = tc.function.as_ref().and_then(|f| f.name.clone()).unwrap_or_default();
                    let a = tc.function.as_ref().and_then(|f| f.arguments.clone()).unwrap_or_default();
                    if let Some(e) = tcs.iter_mut().find(|(i,_,_,_)| *i==idx) { if !n.is_empty() && e.2.is_empty() {e.2 = n.clone();} e.2.push_str(&a); }
                    else if !n.is_empty()||!a.is_empty() { let clean_id = tc.id.clone().unwrap_or_default();
                let clean_id = if let Some(pos) = clean_id.find("{") { clean_id[..pos].to_string() } else { clean_id };
                tcs.push((idx,n.clone(),a.clone(),Some(clean_id))); }
                    yield Ok(Event::default().data(serde_json::json!({"id":id,"object":"chat.completion.chunk","created":created,"model":model,"choices":[{"index":0,"delta":{"tool_calls":[{"index":idx,"id":tc.id,"type":"function","function":{"name":n,"arguments":a}}]},"finish_reason":null}]}).to_string()));
                }}
            }}}
        }
        if text.is_empty() && tcs.is_empty() && has_tools {
            if let Some(tc)=synthesis::tool::synthesize_tool_call(&body) {
                let ct=synthesis::tool::complete_tool_call(&tc,&body);
                yield Ok(Event::default().data(serde_json::json!({"id":id,"object":"chat.completion.chunk","created":created,"model":model,"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":ct.id,"type":"function","function":{"name":ct.function.name,"arguments":ct.function.arguments}}]},"finish_reason":"tool_calls"}]}).to_string()));
            }
        } else if text.is_empty() && tcs.is_empty() {
            let fb=synthesis::text::synthesize_text_fallback(&translate::build_prompt_text(&body.messages));
            yield Ok(Event::default().data(serde_json::json!({"id":id,"object":"chat.completion.chunk","created":created,"model":model,"choices":[{"index":0,"delta":{"content":fb},"finish_reason":"stop"}]}).to_string()));
        }
        yield Ok(Event::default().data("[DONE]".to_string()));
    };
    Ok(Sse::new(stream).into_response())
}
