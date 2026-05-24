use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::to_bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use free_model_client_rs::kernel::{FreeModelKernel, KernelConfig};
use free_model_client_rs::protocol::types::{
    AnthropicMessage, AnthropicRequest, ChatRequest, Message, OpenAITool, OpenAIToolFunction,
};
use serde_json::{json, Value};

#[derive(Clone, Default)]
struct MockState {
    requests: Arc<Mutex<Vec<ObservedRequest>>>,
}

#[derive(Debug)]
struct ObservedRequest {
    proof_header: Option<String>,
    extra_header: Option<String>,
    model: Option<String>,
}

async fn mock_zen_handler(
    State(state): State<MockState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    state.requests.lock().unwrap().push(ObservedRequest {
        proof_header: headers
            .get("x-client-proof")
            .and_then(|v| v.to_str().ok())
            .map(ToOwned::to_owned),
        extra_header: headers
            .get("x-kernel-extra")
            .and_then(|v| v.to_str().ok())
            .map(ToOwned::to_owned),
        model: body
            .get("model")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned),
    });

    let prompt = body
        .get("messages")
        .and_then(|v| v.as_array())
        .and_then(|messages| messages.last())
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_str())
        .unwrap_or_default();

    if prompt.contains("rate-limit") {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "60")],
            "FreeUsageLimitError",
        )
            .into_response();
    }

    let chunk = if prompt.contains("tool-delta") {
        json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "read_file", "arguments": "{\"path\":\"README.md\"}"}
                    }]
                }
            }]
        })
    } else if prompt.contains("reasoning-only") {
        json!({"choices": [{"delta": {"reasoning_content": "hidden chain only"}}]})
    } else {
        json!({"choices": [{"delta": {"content": "golden answer"}}], "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}})
    };

    let body = format!("data: {}\n\ndata: [DONE]\n\n", chunk);
    (
        StatusCode::OK,
        [("content-type", "text/event-stream")],
        body,
    )
        .into_response()
}

async fn spawn_mock_zen() -> (KernelConfig, reqwest::Client, MockState) {
    let state = MockState::default();
    let app = Router::new()
        .route("/zen", post(mock_zen_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-client-proof", "caller-client".parse().unwrap());
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();
    let config = KernelConfig {
        zen_chat_url: format!("http://{addr}/zen"),
        zen_api_key: "public".to_string(),
        extra_headers: vec![("x-kernel-extra".to_string(), "extra-proof".to_string())],
    };
    (config, client, state)
}

fn chat_request(
    model: &str,
    prompt: &str,
    stream: bool,
    tools: Option<Vec<OpenAITool>>,
) -> ChatRequest {
    ChatRequest {
        model: model.to_string(),
        messages: vec![Message {
            role: "user".to_string(),
            content: Value::String(prompt.to_string()),
            tool_calls: None,
        }],
        stream: Some(stream),
        max_tokens: Some(64),
        temperature: None,
        top_p: None,
        tools,
        tool_choice: None,
    }
}

fn anthropic_request(model: &str, prompt: &str, stream: bool) -> AnthropicRequest {
    AnthropicRequest {
        model: model.to_string(),
        messages: vec![AnthropicMessage {
            role: "user".to_string(),
            content: Value::String(prompt.to_string()),
        }],
        stream: Some(stream),
        max_tokens: 64,
        temperature: None,
        system: None,
        tools: None,
    }
}

async fn response_text(response: Response) -> String {
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn openai_non_stream_uses_caller_client_and_returns_golden_response() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .openai_chat(
            &client,
            chat_request("deepseek-v4-flash-free", "plain", false, None),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_str(&response_text(response).await).unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "golden answer");
    assert_eq!(body["model"], "deepseek-v4-flash-free");
    let observed = state.requests.lock().unwrap();
    assert_eq!(observed[0].proof_header.as_deref(), Some("caller-client"));
    assert_eq!(observed[0].extra_header.as_deref(), Some("extra-proof"));
    assert_eq!(observed[0].model.as_deref(), Some("deepseek-v4-flash-free"));
}

#[tokio::test]
async fn openai_stream_preserves_text_delta_and_done() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .openai_chat(
            &client,
            chat_request("deepseek-v4-flash-free", "plain", true, None),
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    assert!(body.contains("golden answer"));
    assert!(body.contains("[DONE]"));
}

#[tokio::test]
async fn anthropic_non_stream_returns_golden_message_response() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .anthropic_messages(
            &client,
            anthropic_request("deepseek-v4-flash-free", "plain", false),
        )
        .await
        .unwrap();
    let body: Value = serde_json::from_str(&response_text(response).await).unwrap();
    assert_eq!(body["content"][0]["text"], "golden answer");
    assert_eq!(body["stop_reason"], "end_turn");
}

#[tokio::test]
async fn anthropic_stream_returns_golden_event_sequence() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .anthropic_messages(
            &client,
            anthropic_request("deepseek-v4-flash-free", "plain", true),
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    assert!(body.contains("event: message_start"));
    assert!(body.contains("event: content_block_delta"));
    assert!(body.contains("golden answer"));
    assert!(body.contains("event: message_stop"));
}

#[tokio::test]
async fn tool_delta_is_preserved_in_streaming_openai_response() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let tools = vec![OpenAITool {
        tool_type: "function".to_string(),
        function: OpenAIToolFunction {
            name: "read_file".to_string(),
            description: None,
            parameters: None,
        },
    }];
    let response = kernel
        .openai_chat(
            &client,
            chat_request("deepseek-v4-flash-free", "tool-delta", true, Some(tools)),
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    assert!(body.contains("read_file"));
    assert!(body.contains("README.md"));
    assert!(body.contains("tool_calls"));
}

#[tokio::test]
async fn reasoning_only_output_uses_deterministic_text_fallback() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .openai_chat(
            &client,
            chat_request("deepseek-v4-flash-free", "reasoning-only", false, None),
        )
        .await
        .unwrap();
    let body: Value = serde_json::from_str(&response_text(response).await).unwrap();
    assert_ne!(
        body["choices"][0]["message"]["content"],
        "hidden chain only"
    );
    assert_eq!(body["choices"][0]["message"]["content"], "NO_TOOL_CALL");
}

#[tokio::test]
async fn upstream_429_is_returned_as_rate_limit_error() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let err = kernel
        .openai_chat(
            &client,
            chat_request("deepseek-v4-flash-free", "rate-limit", false, None),
        )
        .await
        .unwrap_err();
    assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(err.message.contains("FreeUsageLimitError"));
    assert_eq!(
        err.upstream_headers
            .unwrap()
            .iter()
            .find(|(key, _)| key == "retry-after")
            .map(|(_, value)| value.as_str()),
        Some("60")
    );
}
