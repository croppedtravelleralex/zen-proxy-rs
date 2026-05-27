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
    messages: Option<Value>,
    tool_choice: Option<Value>,
    thinking: Option<Value>,
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
        messages: body.get("messages").cloned(),
        tool_choice: body.get("tool_choice").cloned(),
        thinking: body.get("thinking").cloned(),
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
    if prompt.contains("broken-json") {
        return (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            "data: {not-json}\n\n",
        )
            .into_response();
    }
    if prompt.contains("truncated-stream") {
        return (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
        )
            .into_response();
    }
    if prompt.contains("empty-upstream") {
        return (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            "data: [DONE]\n\n",
        )
            .into_response();
    }
    if prompt.contains("role-only-empty") {
        return (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\ndata: [DONE]\n\n",
        )
            .into_response();
    }
    if prompt.contains("sse-protocol-fields") {
        return (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            ": ignored comment\r\nevent: completion\r\nid: evt_1\r\nretry: 1000\r\ndata:{\"choices\":[\r\ndata: {\"delta\":{\"content\":\"golden answer\"}}\r\ndata:]}\r\n\r\ndata:[DONE]\r\n\r\n",
        )
            .into_response();
    }
    if prompt.contains("finish-no-done") {
        return (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            "data: {\"choices\":[{\"delta\":{\"content\":\"golden answer\"},\"finish_reason\":\"stop\"}]}\n\n",
        )
            .into_response();
    }

    let chunk = if prompt.contains("mixed-text-tool-delta") {
        json!({
            "choices": [{
                "delta": {
                    "content": "golden answer",
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "read_file", "arguments": "{\"path\":\"README.md\"}"}
                    }]
                }
            }]
        })
    } else if prompt.contains("tool-delta") {
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
        model_mappings: vec![(
            "deepseek-v4-flash".to_string(),
            "deepseek-v4-flash-free".to_string(),
        )],
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
            tool_call_id: None,
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
        tool_choice: None,
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
            chat_request("deepseek-v4-flash", "plain", false, None),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_str(&response_text(response).await).unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "golden answer");
    assert_eq!(body["model"], "deepseek-v4-flash");
    assert_eq!(body["usage"]["prompt_tokens"], 3);
    assert_eq!(body["usage"]["completion_tokens"], 2);
    assert_eq!(body["usage"]["total_tokens"], 5);
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
async fn openai_stream_accepts_crlf_optional_space_and_multiline_data() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .openai_chat(
            &client,
            chat_request("deepseek-v4-flash-free", "sse-protocol-fields", true, None),
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    assert!(body.contains("golden answer"));
    assert!(body.contains("[DONE]"));
}

#[tokio::test]
async fn openai_non_stream_accepts_finish_reason_without_done() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .openai_chat(
            &client,
            chat_request("deepseek-v4-flash-free", "finish-no-done", false, None),
        )
        .await
        .unwrap();
    let body: Value = serde_json::from_str(&response_text(response).await).unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "golden answer");
}

#[tokio::test]
async fn openai_non_stream_rejects_eof_without_done_or_finish_reason() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let err = kernel
        .openai_chat(
            &client,
            chat_request("deepseek-v4-flash-free", "truncated-stream", false, None),
        )
        .await
        .unwrap_err();
    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    assert!(err.message.contains("stream truncated"));
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
    assert_eq!(body["usage"]["input_tokens"], 3);
    assert_eq!(body["usage"]["output_tokens"], 2);
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
    assert!(body.contains("\"input_tokens\":"));
    assert!(body.contains("event: content_block_delta"));
    assert!(body.contains("golden answer"));
    assert!(body.contains("\"output_tokens\":2"));
    assert!(body.contains("event: message_stop"));
}

#[tokio::test]
async fn anthropic_stream_mixed_text_and_tool_blocks_use_distinct_indexes() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .anthropic_messages(
            &client,
            anthropic_request("deepseek-v4-flash-free", "mixed-text-tool-delta", true),
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    assert!(body.contains("event: content_block_start"));
    assert!(body.contains("\"index\":0"));
    assert!(body.contains("\"index\":1"));
    assert!(body.contains("\"type\":\"tool_use\""));
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
async fn tool_delta_is_preserved_in_non_streaming_openai_response() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .openai_chat(
            &client,
            chat_request("deepseek-v4-flash-free", "tool-delta", false, None),
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    assert!(body.contains("README.md"));
    assert!(body.contains("tool_calls"));
    assert!(body.contains("tool_calls"));
}

#[tokio::test]
async fn tool_delta_is_preserved_in_non_streaming_anthropic_response() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .anthropic_messages(
            &client,
            anthropic_request("deepseek-v4-flash-free", "tool-delta", false),
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    assert!(body.contains("README.md"));
    assert!(body.contains("tool_use"));
    assert!(body.contains("\"stop_reason\":\"tool_use\""));
}

#[tokio::test]
async fn openai_empty_stream_with_tools_reports_empty_output_without_synthetic_tool_call() {
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
            chat_request(
                "deepseek-v4-flash-free",
                "empty-upstream",
                true,
                Some(tools),
            ),
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    assert!(body.contains("upstream returned no assistant content or tool call"));
}

#[tokio::test]
async fn anthropic_empty_stream_with_tools_reports_empty_output_without_synthetic_tool_use() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .anthropic_messages(
            &client,
            AnthropicRequest {
                tools: Some(vec![free_model_client_rs::protocol::types::ToolDef {
                    name: "Read".to_string(),
                    description: "Read a file".to_string(),
                    input_schema: free_model_client_rs::protocol::types::ToolInputSchema {
                        schema_type: "object".to_string(),
                        properties: Some(json!({"file_path":{"type":"string"}})),
                        required: Some(vec!["file_path".to_string()]),
                    },
                }]),
                ..anthropic_request("deepseek-v4-flash-free", "empty-upstream", true)
            },
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    assert!(body.contains("upstream returned no assistant content or tool call"));
}

#[tokio::test]
async fn openai_role_only_stream_is_rejected_as_empty_upstream() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .openai_chat(
            &client,
            chat_request("deepseek-v4-flash-free", "role-only-empty", true, None),
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    assert!(body.contains("upstream returned no assistant content or tool call"));
}

#[tokio::test]
async fn anthropic_role_only_stream_is_rejected_as_empty_upstream() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .anthropic_messages(
            &client,
            anthropic_request("deepseek-v4-flash-free", "role-only-empty", true),
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    assert!(body.contains("upstream returned no assistant content or tool call"));
}

#[tokio::test]
async fn anthropic_tool_use_history_is_preserved_as_openai_tool_calls() {
    let req = AnthropicRequest {
        messages: vec![
            AnthropicMessage {
                role: "assistant".to_string(),
                content: json!([
                    {"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"a.txt"}}
                ]),
            },
            AnthropicMessage {
                role: "user".to_string(),
                content: json!([
                    {"type":"tool_result","tool_use_id":"toolu_1","content":"hello"}
                ]),
            },
        ],
        ..anthropic_request("deepseek-v4-flash-free", "ignored", true)
    };

    let messages = free_model_client_rs::protocol::translate::anthropic_to_openai_messages(&req);
    assert_eq!(messages[0].role, "assistant");
    assert_eq!(messages[0].content, Value::Null);
    assert_eq!(
        messages[0].tool_calls.as_ref().unwrap()[0].id.as_deref(),
        Some("toolu_1")
    );
    assert_eq!(
        messages[0].tool_calls.as_ref().unwrap()[0].function.name,
        "Read"
    );
    assert_eq!(messages[1].role, "tool");
    assert_eq!(messages[1].tool_call_id.as_deref(), Some("toolu_1"));
    assert_eq!(messages[1].content, json!("hello"));
}

#[tokio::test]
async fn anthropic_missing_tool_result_id_is_repaired_before_upstream() {
    let (config, client, observed) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let req = AnthropicRequest {
        messages: vec![
            AnthropicMessage {
                role: "assistant".to_string(),
                content: json!([
                    {"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"a.txt"}}
                ]),
            },
            AnthropicMessage {
                role: "user".to_string(),
                content: json!([
                    {"type":"tool_result","content":"hello from tool"}
                ]),
            },
        ],
        ..anthropic_request("deepseek-v4-flash-free", "ignored", false)
    };

    let response = kernel.anthropic_messages(&client, req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let sent = observed.requests.lock().unwrap();
    let messages = sent[0].messages.as_ref().unwrap().as_array().unwrap();
    assert_eq!(messages[0]["tool_calls"][0]["id"], "toolu_1");
    assert_eq!(messages[1]["role"], "tool");
    assert_eq!(messages[1]["tool_call_id"], "toolu_1");
}

#[tokio::test]
async fn anthropic_mixed_text_and_tool_result_keeps_tool_result_adjacent() {
    let (config, client, observed) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let req = AnthropicRequest {
        messages: vec![
            AnthropicMessage {
                role: "assistant".to_string(),
                content: json!([
                    {"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"a.txt"}}
                ]),
            },
            AnthropicMessage {
                role: "user".to_string(),
                content: json!([
                    {"type":"text","text":"extra user text before result"},
                    {"type":"tool_result","tool_use_id":"toolu_1","content":"tool output"}
                ]),
            },
        ],
        ..anthropic_request("deepseek-v4-flash-free", "ignored", false)
    };

    let response = kernel.anthropic_messages(&client, req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let sent = observed.requests.lock().unwrap();
    let messages = sent[0].messages.as_ref().unwrap().as_array().unwrap();
    assert_eq!(messages[0]["role"], "assistant");
    assert_eq!(messages[1]["role"], "tool");
    assert_eq!(messages[1]["tool_call_id"], "toolu_1");
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(messages[2]["content"], "extra user text before result");
}

#[tokio::test]
async fn openai_interleaved_user_breaks_pending_tool_pair_safely() {
    let (config, client, observed) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut req = chat_request("deepseek-v4-flash-free", "ignored", false, None);
    req.messages = vec![
        Message {
            role: "assistant".to_string(),
            content: Value::Null,
            tool_calls: Some(vec![free_model_client_rs::protocol::types::ToolCall {
                id: Some("call_interleaved".to_string()),
                call_type: "function".to_string(),
                function: free_model_client_rs::protocol::types::ToolFunction {
                    name: "Read".to_string(),
                    arguments: "{}".to_string(),
                },
                index: Some(0),
            }]),
            tool_call_id: None,
        },
        Message {
            role: "user".to_string(),
            content: json!("interleaving text"),
            tool_calls: None,
            tool_call_id: None,
        },
        Message {
            role: "tool".to_string(),
            content: json!("late tool result"),
            tool_calls: None,
            tool_call_id: Some("call_interleaved".to_string()),
        },
    ];

    let response = kernel.openai_chat(&client, req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let sent = observed.requests.lock().unwrap();
    let messages = sent[0].messages.as_ref().unwrap().as_array().unwrap();
    assert!(messages[0].get("tool_calls").is_none());
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[2]["role"], "user");
    assert!(messages[2].get("tool_call_id").is_none());
}

#[tokio::test]
async fn anthropic_orphan_tool_result_is_downgraded_before_upstream() {
    let (config, client, observed) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let req = AnthropicRequest {
        messages: vec![AnthropicMessage {
            role: "user".to_string(),
            content: json!([
                {"type":"tool_result","content":"orphan tool output"}
            ]),
        }],
        ..anthropic_request("deepseek-v4-flash-free", "ignored", false)
    };

    let response = kernel.anthropic_messages(&client, req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let sent = observed.requests.lock().unwrap();
    let messages = sent[0].messages.as_ref().unwrap().as_array().unwrap();
    assert_eq!(messages[0]["role"], "user");
    assert!(messages[0].get("tool_call_id").is_none());
    assert!(messages[0]["content"]
        .as_str()
        .unwrap()
        .contains("Tool result recovered as plain context"));
}

#[tokio::test]
async fn openai_tool_choice_is_forwarded_to_upstream() {
    let (config, client, observed) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut req = chat_request(
        "deepseek-v4-flash-free",
        "use Task",
        false,
        Some(vec![OpenAITool {
            tool_type: "function".to_string(),
            function: OpenAIToolFunction {
                name: "Task".to_string(),
                description: None,
                parameters: None,
            },
        }]),
    );
    req.tool_choice = Some(json!({"type":"function","function":{"name":"Task"}}));
    let _ = kernel.openai_chat(&client, req).await.unwrap();
    let sent = observed.requests.lock().unwrap();
    assert_eq!(
        sent[0].tool_choice.as_ref(),
        Some(&json!({"type":"function","function":{"name":"Task"}}))
    );
    assert_eq!(sent[0].thinking.as_ref(), Some(&json!({"type":"disabled"})));
}

#[tokio::test]
async fn anthropic_tool_choice_is_translated_to_openai_function_choice() {
    let (config, client, observed) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let req = AnthropicRequest {
        tools: Some(vec![free_model_client_rs::protocol::types::ToolDef {
            name: "Task".to_string(),
            description: "Launch subagent".to_string(),
            input_schema: free_model_client_rs::protocol::types::ToolInputSchema {
                schema_type: "object".to_string(),
                properties: Some(json!({"prompt":{"type":"string"}})),
                required: Some(vec!["prompt".to_string()]),
            },
        }]),
        tool_choice: Some(json!({"type":"tool","name":"Task"})),
        ..anthropic_request("deepseek-v4-flash-free", "use Task", false)
    };
    let _ = kernel.anthropic_messages(&client, req).await.unwrap();
    let sent = observed.requests.lock().unwrap();
    assert_eq!(
        sent[0].tool_choice.as_ref(),
        Some(&json!({"type":"function","function":{"name":"Task"}}))
    );
    assert_eq!(sent[0].thinking.as_ref(), Some(&json!({"type":"disabled"})));
}

#[test]
fn thinking_is_disabled_when_assistant_history_has_no_reasoning() {
    let mut body = json!({
        "model":"deepseek-v4-flash-free",
        "messages":[{"role":"assistant","content":null,"tool_calls":[]}]
    });
    let messages = vec![Message {
        role: "assistant".to_string(),
        content: Value::Null,
        tool_calls: Some(vec![]),
        tool_call_id: None,
    }];

    free_model_client_rs::protocol::translate::disable_thinking_for_assistant_history(
        &mut body, &messages,
    );

    assert_eq!(body["thinking"], json!({"type":"disabled"}));
}

#[test]
fn short_user_prompt_is_stabilized_before_upstream() {
    let mut body = json!({
        "messages": [{"role": "user", "content": "1"}],
        "tools": null
    });
    free_model_client_rs::protocol::translate::stabilize_short_user_prompt(&mut body);
    assert_eq!(body["messages"][0]["content"], "只回复 ok");
}

#[test]
fn short_user_prompt_with_tools_is_not_rewritten() {
    let mut body = json!({
        "messages": [{"role": "user", "content": "1"}],
        "tools": [{"type":"function","function":{"name":"Task"}}]
    });
    free_model_client_rs::protocol::translate::stabilize_short_user_prompt(&mut body);
    assert_eq!(body["messages"][0]["content"], "1");
}

#[test]
fn anthropic_tool_result_content_is_redacted_before_upstream() {
    let req = AnthropicRequest {
        messages: vec![AnthropicMessage {
            role: "user".to_string(),
            content: json!([
                {"type":"tool_result","tool_use_id":"toolu_secret","content":"API_KEY=abc123\nNEWAPI_KEY=sk-fake-do-not-leak\nproxy.example:8080:user:pass"}
            ]),
        }],
        ..anthropic_request("deepseek-v4-flash-free", "ignored", true)
    };

    let messages = free_model_client_rs::protocol::translate::anthropic_to_openai_messages(&req);
    let content = messages[0].content.as_str().unwrap();
    assert!(!content.contains("abc123"));
    assert!(!content.contains("sk-fake-do-not-leak"));
    assert!(!content.contains("user:pass"));
    assert!(content.contains("[REDACTED]"));
}

#[tokio::test]
async fn reasoning_only_output_is_rejected_as_empty_upstream() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let err = kernel
        .openai_chat(
            &client,
            chat_request("deepseek-v4-flash-free", "reasoning-only", false, None),
        )
        .await
        .unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::BAD_GATEWAY);
    assert!(err.message.contains("no assistant content"));
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

#[tokio::test]
async fn non_stream_parse_error_is_structured_error() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let err = kernel
        .openai_chat(
            &client,
            chat_request("deepseek-v4-flash-free", "broken-json", false, None),
        )
        .await
        .unwrap_err();
    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    assert!(err.message.contains("stream parse error"));
}

#[tokio::test]
async fn stream_parse_error_is_emitted_before_done() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .openai_chat(
            &client,
            chat_request("deepseek-v4-flash-free", "broken-json", true, None),
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    assert!(body.contains("stream parse error"));
    assert!(body.contains("[DONE]"));
}

#[tokio::test]
async fn stream_truncation_is_emitted_before_done() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .openai_chat(
            &client,
            chat_request("deepseek-v4-flash-free", "truncated-stream", true, None),
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    assert!(body.contains("stream truncated"));
    assert!(body.contains("[DONE]"));
}
