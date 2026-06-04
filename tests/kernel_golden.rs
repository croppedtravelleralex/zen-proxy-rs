use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::to_bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use free_model_client_rs::client_profile::{ClientKind, ClientProfile, ClientProfileSource};
use free_model_client_rs::kernel::{FreeModelKernel, KernelConfig};
use free_model_client_rs::protocol::types::{
    AnthropicMessage, AnthropicRequest, ChatRequest, Message, OpenAITool, OpenAIToolFunction,
    ToolDef, ToolInputSchema,
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
    max_tokens_present: bool,
    max_tokens: Option<u64>,
}

async fn mock_zen_handler(
    State(state): State<MockState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let observed = ObservedRequest {
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
        max_tokens_present: body.get("max_tokens").is_some(),
        max_tokens: body.get("max_tokens").and_then(Value::as_u64),
    };
    let request_count = {
        let mut requests = state.requests.lock().unwrap();
        requests.push(observed);
        requests.len()
    };

    let prompt = body
        .get("messages")
        .and_then(|v| v.as_array())
        .map(|messages| {
            messages
                .iter()
                .filter_map(|message| message.get("content").and_then(|content| content.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        })
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
    if prompt.contains("tiny-empty") {
        return (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            "data: [DONE]\n\n",
        )
            .into_response();
    }
    if prompt.trim().eq_ignore_ascii_case("echo hi") {
        return (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            "data: [DONE]\n\n",
        )
            .into_response();
    }
    if prompt.contains("HUGE_EMPTY_OK") {
        return (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            "data: [DONE]\n\n",
        )
            .into_response();
    }
    if prompt.contains("HUGE_TTL_OK") {
        return (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            "data: {\"choices\":[{\"delta\":{\"content\":\"HUGE_TTL_OK\"},\"finish_reason\":\"stop\"}]}\n\n",
        )
            .into_response();
    }
    if prompt.contains("HUGE_OK") {
        return (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            "data: {\"choices\":[{\"delta\":{\"content\":\"HUGE_OK\"},\"finish_reason\":\"stop\"}]}\n\n",
        )
            .into_response();
    }
    if prompt.contains("empty-once") && request_count == 1 {
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
    if prompt.contains("finish-length") {
        return (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial answer\"},\"finish_reason\":\"length\"}]}\n\n",
        )
            .into_response();
    }

    if prompt.contains("inline-fence-markdown") {
        return (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            "data: {\"choices\":[{\"delta\":{\"content\":\"```text\\nProcessBTCmd```\\n## Result\\n| a | b |\\n\"}}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":8,\"total_tokens\":11}}\n\ndata: [DONE]\n\n",
        )
            .into_response();
    }
    if prompt.contains("unclosed-fence-markdown") {
        return (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            "data: {\"choices\":[{\"delta\":{\"content\":\"```text\\nlog line\"}}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":4,\"total_tokens\":7}}\n\ndata: [DONE]\n\n",
        )
            .into_response();
    }
    if prompt.contains("whitespace-delta") {
        return (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            "data: {\"choices\":[{\"delta\":{\"content\":\"alpha\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"\\n    \"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"beta\"}}]}\n\ndata: [DONE]\n\n",
        )
            .into_response();
    }
    if prompt.contains("secret-output") {
        return (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            "data: {\"choices\":[{\"delta\":{\"content\":\"API_KEY=abc123\\nsk-fake-do-not-leak\"}}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":8,\"total_tokens\":11}}\n\ndata: [DONE]\n\n",
        )
            .into_response();
    }

    if prompt.contains("cache-usage-tool") {
        let body = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_cache_1\",\"type\":\"function\",\"function\":{\"name\":\"Read\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":30,\"completion_tokens\":5,\"total_tokens\":35,\"prompt_tokens_details\":{\"cached_tokens\":22},\"cache_creation_input_tokens\":11,\"cache_read_input_tokens\":22}}\n\ndata: [DONE]\n\n";
        return (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            body,
        )
            .into_response();
    }

    if prompt.contains("cache-usage") {
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"golden answer\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":30,\"completion_tokens\":5,\"total_tokens\":35,\"prompt_tokens_details\":{\"cached_tokens\":22},\"cache_creation_input_tokens\":11,\"cache_read_input_tokens\":22}}\n\ndata: [DONE]\n\n";
        return (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            body,
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
    } else if prompt.contains("web-tool-delta") {
        json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_web_1",
                        "type": "function",
                        "function": {"name": "web_search", "arguments": "{\"query\":\"today weather\"}"}
                    }]
                }
            }]
        })
    } else if prompt.contains("task-lower-delta") {
        json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_task_1",
                        "type": "function",
                        "function": {"name": "task", "arguments": "{\"description\":\"check\",\"prompt\":\"check\",\"subagent_type\":\"general-purpose\"}"}
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
        max_tokens: Some(64),
        temperature: None,
        system: None,
        tools: None,
        tool_choice: None,
    }
}

fn anthropic_tool(name: &str, properties: Value, required: &[&str]) -> ToolDef {
    ToolDef {
        name: name.to_string(),
        description: format!("{name} tool"),
        input_schema: ToolInputSchema {
            schema_type: "object".to_string(),
            properties: Some(properties),
            required: Some(required.iter().map(|item| item.to_string()).collect()),
        },
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
async fn openai_non_stream_preserves_cache_usage_metadata() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .openai_chat(
            &client,
            chat_request("deepseek-v4-flash", "cache-usage", false, None),
        )
        .await
        .unwrap();

    let body: Value = serde_json::from_str(&response_text(response).await).unwrap();
    assert_eq!(body["usage"]["prompt_tokens"], 30);
    assert_eq!(body["usage"]["completion_tokens"], 5);
    assert_eq!(body["usage"]["total_tokens"], 35);
    assert_eq!(body["usage"]["cache_creation_input_tokens"], 11);
    assert_eq!(body["usage"]["cache_read_input_tokens"], 22);
    assert_eq!(body["usage"]["prompt_tokens_details"]["cached_tokens"], 22);
}

#[tokio::test]
async fn openai_non_stream_tool_response_preserves_cache_usage_metadata() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .openai_chat(
            &client,
            chat_request("deepseek-v4-flash", "cache-usage-tool", false, None),
        )
        .await
        .unwrap();

    let body: Value = serde_json::from_str(&response_text(response).await).unwrap();
    assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(body["usage"]["cache_creation_input_tokens"], 11);
    assert_eq!(body["usage"]["cache_read_input_tokens"], 22);
}

#[tokio::test]
async fn openai_stream_preserves_cache_usage_metadata() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .openai_chat(
            &client,
            chat_request("deepseek-v4-flash", "cache-usage", true, None),
        )
        .await
        .unwrap();

    let body = response_text(response).await;
    assert!(body.contains("\"cache_creation_input_tokens\":11"));
    assert!(body.contains("\"cache_read_input_tokens\":22"));
    assert!(body.contains("\"prompt_tokens_details\":{\"cached_tokens\":22}"));
}

#[tokio::test]
async fn openai_non_stream_preserves_short_user_prompt_upstream() {
    for prompt in ["1", "继续", "执行"] {
        let (config, client, state) = spawn_mock_zen().await;
        let kernel = FreeModelKernel::new(config);
        let response = kernel
            .openai_chat(
                &client,
                chat_request("deepseek-v4-flash", prompt, false, None),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let observed = state.requests.lock().unwrap();
        let messages = observed[0].messages.as_ref().unwrap().as_array().unwrap();
        assert_eq!(messages[0]["content"], prompt);
        assert!(
            observed[0].thinking.is_none(),
            "ordinary short prompt should not disable thinking by default"
        );
    }
}

#[tokio::test]
async fn deepseek_flash_hermes_profile_does_not_apply_compat_thinking_policy() {
    for model in ["deepseek-v4-flash", "deepseek-v4-flash-free"] {
        for kind in [ClientKind::Hermes, ClientKind::OpenClaw] {
            let (config, client, state) = spawn_mock_zen().await;
            let kernel = FreeModelKernel::new(config);
            let tools = vec![OpenAITool {
                tool_type: "function".to_string(),
                function: OpenAIToolFunction {
                    name: "Read".to_string(),
                    description: None,
                    parameters: None,
                },
            }];

            let response = kernel
                .openai_chat_with_profile(
                    &client,
                    chat_request(model, "use tool", true, Some(tools)),
                    ClientProfile::new(kind, ClientProfileSource::Header),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            let observed = state.requests.lock().unwrap();
            assert!(
                observed[0].thinking.is_none(),
                "{model} must not apply Hermes/OpenClaw compat thinking policy"
            );
        }
    }
}

#[tokio::test]
async fn deepseek_flash_lite_claude_code_profile_does_not_apply_claude_format_policy() {
    for model in ["deepseek-v4-flash-lite", "big-pickle"] {
        let (config, client, _) = spawn_mock_zen().await;
        let kernel = FreeModelKernel::new(config);

        let response = kernel
            .openai_chat_with_profile(
                &client,
                chat_request(model, "whitespace-delta", true, None),
                ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
            )
            .await
            .unwrap();

        let body = response_text(response).await;
        assert!(body.contains("alpha"));
        assert!(body.contains("beta"));
        assert!(
            !body.contains("\\n    "),
            "{model} must not apply ClaudeCode exact text policy"
        );
    }
}

#[tokio::test]
async fn openai_non_stream_preserves_large_max_tokens_before_upstream() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut req = chat_request("deepseek-v4-flash", "plain", false, None);
    req.max_tokens = Some(20_000);

    let response = kernel.openai_chat(&client, req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let observed = state.requests.lock().unwrap();
    assert!(observed[0].max_tokens_present);
    assert_eq!(observed[0].max_tokens, Some(20_000));
}

#[tokio::test]
async fn anthropic_non_stream_preserves_large_max_tokens_before_upstream() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut req = anthropic_request("deepseek-v4-flash", "plain", false);
    req.max_tokens = Some(20_000);

    let response = kernel.anthropic_messages(&client, req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let observed = state.requests.lock().unwrap();
    assert!(observed[0].max_tokens_present);
    assert_eq!(observed[0].max_tokens, Some(20_000));
}

#[tokio::test]
async fn openai_non_stream_omits_missing_max_tokens_before_upstream() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut req = chat_request("deepseek-v4-flash", "plain", false, None);
    req.max_tokens = None;

    let response = kernel.openai_chat(&client, req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let observed = state.requests.lock().unwrap();
    assert!(!observed[0].max_tokens_present);
    assert_eq!(observed[0].max_tokens, None);
}

#[tokio::test]
async fn openai_stream_omits_missing_max_tokens_before_upstream() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut req = chat_request("deepseek-v4-flash", "plain", true, None);
    req.max_tokens = None;

    let response = kernel.openai_chat(&client, req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let observed = state.requests.lock().unwrap();
    assert!(!observed[0].max_tokens_present);
    assert_eq!(observed[0].max_tokens, None);
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
async fn openai_non_stream_preserves_length_finish_reason() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .openai_chat(
            &client,
            chat_request("deepseek-v4-flash-free", "finish-length", false, None),
        )
        .await
        .unwrap();
    let body: Value = serde_json::from_str(&response_text(response).await).unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "partial answer");
    assert_eq!(body["choices"][0]["finish_reason"], "length");
}

#[tokio::test]
async fn openai_stream_preserves_length_finish_reason() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .openai_chat(
            &client,
            chat_request("deepseek-v4-flash-free", "finish-length", true, None),
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    assert!(body.contains("partial answer"));
    assert!(body.contains("\"finish_reason\":\"length\""));
}

#[tokio::test]
async fn openai_non_stream_repairs_markdown_fence_boundaries() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .openai_chat(
            &client,
            chat_request(
                "deepseek-v4-flash-free",
                "inline-fence-markdown",
                false,
                None,
            ),
        )
        .await
        .unwrap();
    let body: Value = serde_json::from_str(&response_text(response).await).unwrap();
    assert_eq!(
        body["choices"][0]["message"]["content"],
        "```text\nProcessBTCmd\n```\n## Result\n| a | b |\n"
    );
}

#[tokio::test]
async fn openai_stream_closes_unclosed_markdown_fence() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .openai_chat(
            &client,
            chat_request(
                "deepseek-v4-flash-free",
                "unclosed-fence-markdown",
                true,
                None,
            ),
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    assert!(body.contains("```text\\nlog line"));
    assert!(body.contains("\\n```\\n"));
    assert!(body.contains("[DONE]"));
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
async fn anthropic_non_stream_maps_length_finish_reason_to_max_tokens() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .anthropic_messages(
            &client,
            anthropic_request("deepseek-v4-flash-free", "finish-length", false),
        )
        .await
        .unwrap();
    let body: Value = serde_json::from_str(&response_text(response).await).unwrap();
    assert_eq!(body["content"][0]["text"], "partial answer");
    assert_eq!(body["stop_reason"], "max_tokens");
}

#[tokio::test]
async fn anthropic_stream_maps_length_finish_reason_to_max_tokens() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .anthropic_messages(
            &client,
            anthropic_request("deepseek-v4-flash-free", "finish-length", true),
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    assert!(body.contains("partial answer"));
    assert!(body.contains("\"stop_reason\":\"max_tokens\""));
}

#[tokio::test]
async fn anthropic_non_stream_preserves_cache_usage_metadata() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .anthropic_messages(
            &client,
            anthropic_request("deepseek-v4-flash-free", "cache-usage", false),
        )
        .await
        .unwrap();

    let body: Value = serde_json::from_str(&response_text(response).await).unwrap();
    assert_eq!(body["usage"]["input_tokens"], 30);
    assert_eq!(body["usage"]["output_tokens"], 5);
    assert_eq!(body["usage"]["cache_creation_input_tokens"], 11);
    assert_eq!(body["usage"]["cache_read_input_tokens"], 22);
}

#[tokio::test]
async fn anthropic_non_stream_tool_response_preserves_cache_usage_metadata() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .anthropic_messages(
            &client,
            anthropic_request("deepseek-v4-flash-free", "cache-usage-tool", false),
        )
        .await
        .unwrap();

    let body: Value = serde_json::from_str(&response_text(response).await).unwrap();
    assert_eq!(body["stop_reason"], "tool_use");
    assert_eq!(body["usage"]["cache_creation_input_tokens"], 11);
    assert_eq!(body["usage"]["cache_read_input_tokens"], 22);
}

#[tokio::test]
async fn anthropic_stream_preserves_cache_usage_metadata() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .anthropic_messages(
            &client,
            anthropic_request("deepseek-v4-flash-free", "cache-usage", true),
        )
        .await
        .unwrap();

    let body = response_text(response).await;
    assert!(body.contains("\"cache_creation_input_tokens\":11"));
    assert!(body.contains("\"cache_read_input_tokens\":22"));
}

#[tokio::test]
async fn anthropic_non_stream_repairs_markdown_fence_boundaries() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .anthropic_messages(
            &client,
            anthropic_request("deepseek-v4-flash-free", "inline-fence-markdown", false),
        )
        .await
        .unwrap();
    let body: Value = serde_json::from_str(&response_text(response).await).unwrap();
    assert_eq!(
        body["content"][0]["text"],
        "```text\nProcessBTCmd\n```\n## Result\n| a | b |\n"
    );
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
async fn anthropic_stream_closes_unclosed_markdown_fence() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .anthropic_messages(
            &client,
            anthropic_request("deepseek-v4-flash-free", "unclosed-fence-markdown", true),
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    assert!(body.contains("```text\\nlog line"));
    assert!(body.contains("\\n```\\n"));
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
async fn claude_code_anthropic_stream_canonicalizes_web_tool_name() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut request = anthropic_request("deepseek-v4-flash-free", "web-tool-delta", true);
    request.tools = Some(vec![
        anthropic_tool("WebSearch", json!({"query":{"type":"string"}}), &["query"]),
        anthropic_tool("WebFetch", json!({"url":{"type":"string"}}), &["url"]),
    ]);

    let response = kernel
        .anthropic_messages_with_profile(
            &client,
            request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();
    let body = response_text(response).await;

    assert!(body.contains("\"name\":\"WebSearch\""));
    assert!(!body.contains("\"name\":\"web_search\""));
}

#[tokio::test]
async fn claude_code_anthropic_nonstream_canonicalizes_task_tool_name() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut request = anthropic_request("deepseek-v4-flash-free", "task-lower-delta", false);
    request.tools = Some(vec![anthropic_tool(
        "Task",
        json!({
            "description":{"type":"string"},
            "prompt":{"type":"string"},
            "subagent_type":{"type":"string"}
        }),
        &["description", "prompt", "subagent_type"],
    )]);

    let response = kernel
        .anthropic_messages_with_profile(
            &client,
            request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();
    let body = response_text(response).await;

    assert!(body.contains("\"name\":\"Task\""));
    assert!(!body.contains("\"name\":\"task\""));
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
async fn openai_non_stream_channel_probe_empty_upstream_returns_local_ok() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .openai_chat(
            &client,
            chat_request("deepseek-v4-flash-free", "echo hi", false, None),
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    assert!(body.contains("\"content\":\"ok\""));
    assert_eq!(state.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn openai_claude_code_tiny_non_probe_empty_upstream_stays_error() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let err = kernel
        .openai_chat_with_profile(
            &client,
            chat_request("deepseek-v4-flash-free", "tiny-empty", false, None),
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap_err();

    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    assert_eq!(
        err.message,
        "upstream returned no assistant content or tool call"
    );
    assert_eq!(state.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn openai_claude_code_explicit_smoke_empty_upstream_returns_pass() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut request = chat_request(
        "deepseek-v4-flash-lite",
        "strict smoke: reply PASS only empty-upstream",
        false,
        None,
    );
    request.max_tokens = Some(16);

    let response = kernel
        .openai_chat_with_profile(
            &client,
            request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();

    let body = response_text(response).await;
    assert!(body.contains("\"content\":\"PASS\""));
    assert_eq!(state.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn anthropic_non_stream_channel_probe_empty_upstream_returns_local_ok() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .anthropic_messages(
            &client,
            anthropic_request("deepseek-v4-flash-free", "echo hi", false),
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    assert!(body.contains("\"text\":\"ok\""));
    assert_eq!(state.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn anthropic_claude_code_explicit_smoke_empty_upstream_returns_pass() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut request = anthropic_request(
        "deepseek-v4-flash-free",
        "strict smoke: reply PASS only empty-upstream",
        false,
    );
    request.max_tokens = Some(16);

    let response = kernel
        .anthropic_messages_with_profile(
            &client,
            request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();

    let body = response_text(response).await;
    assert!(body.contains("\"text\":\"PASS\""));
    assert_eq!(state.requests.lock().unwrap().len(), 3);
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
    assert_eq!(
        messages[0]["content"].as_str().unwrap(),
        "orphan tool output"
    );
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
    assert!(
        sent[0].thinking.is_none(),
        "unknown/default profile must not force disabled thinking"
    );
}

#[tokio::test]
async fn claude_code_tools_do_not_disable_thinking() {
    let (config, client, observed) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let req = chat_request(
        "deepseek-v4-flash-lite",
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
    let _ = kernel
        .openai_chat_with_profile(
            &client,
            req,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();
    let sent = observed.requests.lock().unwrap();
    assert!(
        sent[0].thinking.is_none(),
        "ClaudeCode tool requests must not be forced into disabled thinking"
    );
}

#[tokio::test]
async fn hermes_tools_keep_compat_thinking_policy() {
    let (config, client, observed) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let req = chat_request(
        "deepseek-v4-flash-lite",
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
    let _ = kernel
        .openai_chat_with_profile(
            &client,
            req,
            ClientProfile::new(ClientKind::Hermes, ClientProfileSource::Header),
        )
        .await
        .unwrap();
    let sent = observed.requests.lock().unwrap();
    assert_eq!(sent[0].thinking.as_ref(), Some(&json!({"type":"disabled"})));
}

#[tokio::test]
async fn claude_code_stream_preserves_whitespace_delta() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .openai_chat_with_profile(
            &client,
            chat_request("deepseek-v4-flash-free", "whitespace-delta", true, None),
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    assert!(body.contains("alpha"));
    assert!(body.contains("\\n    "));
    assert!(body.contains("beta"));
}

#[tokio::test]
async fn claude_code_openai_stream_preserves_raw_markdown_text() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .openai_chat_with_profile(
            &client,
            chat_request(
                "deepseek-v4-flash-free",
                "inline-fence-markdown",
                true,
                None,
            ),
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();

    let body = response_text(response).await;
    assert!(body.contains("ProcessBTCmd```\\n## Result"));
    assert!(!body.contains("ProcessBTCmd\\n```\\n## Result"));
}

#[tokio::test]
async fn claude_code_anthropic_stream_preserves_raw_markdown_text() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .anthropic_messages_with_profile(
            &client,
            anthropic_request("deepseek-v4-flash-free", "inline-fence-markdown", true),
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();

    let body = response_text(response).await;
    assert!(body.contains("ProcessBTCmd```\\n## Result"));
    assert!(!body.contains("ProcessBTCmd\\n```\\n## Result"));
}

#[tokio::test]
async fn claude_code_non_stream_preserves_sensitive_looking_model_text() {
    let (config, client, _) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let response = kernel
        .openai_chat_with_profile(
            &client,
            chat_request("deepseek-v4-flash-free", "secret-output", false, None),
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();

    let body = response_text(response).await;
    assert!(body.contains("API_KEY=abc123"));
    assert!(body.contains("sk-fake-do-not-leak"));
    assert!(!body.contains("[REDACTED]"));
}

#[test]
fn explicit_client_profile_header_overrides_body_heuristic() {
    let mut headers = HeaderMap::new();
    headers.insert("x-fmc-client", "openclaw".parse().unwrap());
    let req = chat_request(
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

    let profile = ClientProfile::from_openai(&headers, &req);
    assert_eq!(profile.kind, ClientKind::OpenClaw);
    assert_eq!(profile.source, ClientProfileSource::Header);
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
        ..anthropic_request("deepseek-v4-flash-lite", "use Task", false)
    };
    let _ = kernel
        .anthropic_messages_with_profile(
            &client,
            req,
            ClientProfile::new(ClientKind::Hermes, ClientProfileSource::Header),
        )
        .await
        .unwrap();
    let sent = observed.requests.lock().unwrap();
    assert_eq!(
        sent[0].tool_choice.as_ref(),
        Some(&json!({"type":"function","function":{"name":"Task"}}))
    );
    assert_eq!(sent[0].thinking.as_ref(), Some(&json!({"type":"disabled"})));
}

#[test]
fn thinking_is_not_disabled_for_plain_assistant_history() {
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

    assert!(body.get("thinking").is_none());
}

#[test]
fn short_user_prompts_are_preserved_before_upstream() {
    for prompt in ["1", "继续", "执行"] {
        let mut body = json!({
            "messages": [{"role": "user", "content": prompt}],
            "tools": null
        });
        free_model_client_rs::protocol::translate::stabilize_short_user_prompt(&mut body);
        assert_eq!(body["messages"][0]["content"], prompt);
    }
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
fn non_stream_output_policy_preserves_requested_max_tokens_by_prompt_size() {
    fn msg(chars: usize) -> Vec<Message> {
        vec![Message {
            role: "user".to_string(),
            content: Value::String("x".repeat(chars)),
            tool_calls: None,
            tool_call_id: None,
        }]
    }

    let missing =
        free_model_client_rs::protocol::translate::non_stream_output_policy(&msg(4), None);
    assert_eq!(missing.effective_max_tokens, None);
    assert!(!missing.capped);

    let small =
        free_model_client_rs::protocol::translate::non_stream_output_policy(&msg(4), Some(20_000));
    assert_eq!(small.effective_max_tokens, Some(20_000));
    assert!(!small.capped);

    let tiny =
        free_model_client_rs::protocol::translate::non_stream_output_policy(&msg(4), Some(1));
    assert_eq!(tiny.effective_max_tokens, Some(1));
    assert!(!tiny.capped);

    let fifty_k = free_model_client_rs::protocol::translate::non_stream_output_policy(
        &msg(200_000),
        Some(20_000),
    );
    assert_eq!(fifty_k.prompt_tokens, 50_000);
    assert_eq!(fifty_k.effective_max_tokens, Some(20_000));
    assert!(!fifty_k.capped);

    let hundred_k = free_model_client_rs::protocol::translate::non_stream_output_policy(
        &msg(400_000),
        Some(20_000),
    );
    assert_eq!(hundred_k.prompt_tokens, 100_000);
    assert_eq!(hundred_k.effective_max_tokens, Some(20_000));
    assert!(!hundred_k.capped);
}

#[test]
fn stream_output_policy_preserves_explicit_max_tokens_by_prompt_size() {
    fn msg(chars: usize) -> Vec<Message> {
        vec![Message {
            role: "user".to_string(),
            content: Value::String("x".repeat(chars)),
            tool_calls: None,
            tool_call_id: None,
        }]
    }

    let small =
        free_model_client_rs::protocol::translate::stream_output_policy(&msg(4), Some(20_000));
    assert_eq!(small.effective_max_tokens, Some(20_000));
    assert!(!small.capped);

    let tiny = free_model_client_rs::protocol::translate::stream_output_policy(&msg(4), Some(1));
    assert_eq!(tiny.effective_max_tokens, Some(1));
    assert!(!tiny.capped);

    let fifty_k = free_model_client_rs::protocol::translate::stream_output_policy(
        &msg(200_000),
        Some(20_000),
    );
    assert_eq!(fifty_k.prompt_tokens, 50_000);
    assert_eq!(fifty_k.effective_max_tokens, Some(20_000));
    assert!(!fifty_k.capped);

    let hundred_k = free_model_client_rs::protocol::translate::stream_output_policy(
        &msg(400_000),
        Some(20_000),
    );
    assert_eq!(hundred_k.prompt_tokens, 100_000);
    assert_eq!(hundred_k.effective_max_tokens, Some(20_000));
    assert!(!hundred_k.capped);

    let missing =
        free_model_client_rs::protocol::translate::stream_output_policy(&msg(400_000), None);
    assert_eq!(missing.effective_max_tokens, None);
    assert!(!missing.capped);
}

#[test]
fn stream_context_compactor_preserves_latest_tail() {
    let tail = "FINAL_MARKER";
    let mut messages = vec![Message {
        role: "user".to_string(),
        content: Value::String(format!("{}{}", "x".repeat(360_000), tail)),
        tool_calls: None,
        tool_call_id: None,
    }];

    let repair = free_model_client_rs::protocol::translate::compact_stream_context(&mut messages);

    assert!(repair.compacted_messages >= 1);
    assert!(repair.after_tokens < repair.before_tokens);
    let compacted = messages[0].content.as_str().unwrap();
    assert!(compacted.contains("context compactor"));
    assert!(compacted.ends_with(tail));
}

#[test]
fn claude_code_stream_context_policy_compacts_more_aggressively() {
    let tail = "FINAL_MARKER";
    let mut default_messages = vec![Message {
        role: "user".to_string(),
        content: Value::String(format!("{}{}", "x".repeat(1_000_000), tail)),
        tool_calls: None,
        tool_call_id: None,
    }];
    let mut claude_messages = default_messages.clone();

    let default_repair =
        free_model_client_rs::protocol::translate::compact_stream_context(&mut default_messages);
    let claude_repair =
        free_model_client_rs::protocol::translate::compact_stream_context_with_policy(
            &mut claude_messages,
            free_model_client_rs::protocol::translate::StreamContextPolicy::claude_code_huge_context(
            ),
        );

    assert!(claude_repair.after_tokens < default_repair.after_tokens);
    assert!(claude_repair.after_tokens <= 14_000);
    assert!(claude_messages[0].content.as_str().unwrap().ends_with(tail));
}

#[test]
fn claude_code_stream_context_policy_trims_oversized_system_tail() {
    let mut messages = vec![
        Message {
            role: "system".to_string(),
            content: Value::String(format!(
                "{}GIT_STATUS_NOISE",
                "system instruction. ".repeat(40_000)
            )),
            tool_calls: None,
            tool_call_id: None,
        },
        Message {
            role: "user".to_string(),
            content: Value::String(format!("{}FINAL_MARKER", "x".repeat(360_000))),
            tool_calls: None,
            tool_call_id: None,
        },
    ];

    let repair = free_model_client_rs::protocol::translate::compact_stream_context_with_policy(
        &mut messages,
        free_model_client_rs::protocol::translate::StreamContextPolicy::claude_code_huge_context(),
    );

    assert_eq!(repair.compacted_messages, 2);
    let system = messages[0].content.as_str().unwrap();
    let user = messages[1].content.as_str().unwrap();
    assert!(system.contains("omitted tail of oversized system context"));
    assert!(!system.contains("GIT_STATUS_NOISE"));
    assert!(user.ends_with("FINAL_MARKER"));
}

#[test]
fn claude_code_stream_context_policy_anchors_latest_user_final_question() {
    let final_instruction = "Final question: output HUGE_OK only.";
    let stale_transcript =
        "Understood. Let me pick up by examining the current state of the workspace. git diff\n";
    let mut messages = vec![Message {
        role: "user".to_string(),
        content: Value::String(format!(
            "{}{}{}",
            stale_transcript.repeat(3_000),
            "old transcript noise\n".repeat(12_000),
            final_instruction
        )),
        tool_calls: None,
        tool_call_id: None,
    }];

    let repair = free_model_client_rs::protocol::translate::compact_stream_context_with_policy(
        &mut messages,
        free_model_client_rs::protocol::translate::StreamContextPolicy::claude_code_huge_context(),
    );

    assert!(repair.compacted_messages >= 1);
    let compacted = messages[0].content.as_str().unwrap();
    assert!(compacted.contains(final_instruction));
    assert!(
        !compacted.contains("Let me pick up"),
        "ClaudeCode huge compaction should remove stale transcript recovery pressure"
    );
    assert!(compacted.contains("latest user excerpt preserved"));
}

#[test]
fn claude_code_huge_anchor_skips_later_resume_transcript_pressure() {
    let final_instruction = "Final question: output HUGE_OK only.";
    let huge_request = format!(
        "Read this huge controlled local context.\n{}{}",
        "huge-section\n".repeat(30_000),
        final_instruction
    );
    let mut messages = vec![
        Message {
            role: "user".to_string(),
            content: Value::String(huge_request),
            tool_calls: None,
            tool_call_id: None,
        },
        Message {
            role: "user".to_string(),
            content: Value::String(
                "I need to pick up where we left off. Read the latest transcript in .claude/projects/session.jsonl and run git status."
                    .to_string(),
            ),
            tool_calls: None,
            tool_call_id: None,
        },
    ];

    let repair = free_model_client_rs::protocol::translate::compact_stream_context_with_policy(
        &mut messages,
        free_model_client_rs::protocol::translate::StreamContextPolicy::claude_code_huge_context(),
    );
    assert!(repair.compacted_messages >= 1);
    let appended = free_model_client_rs::protocol::translate::append_latest_user_anchor_message(
        &mut messages,
        2 * 1024,
    );

    assert!(appended);
    let anchor = messages.last().unwrap().content.as_str().unwrap();
    assert!(anchor.contains(final_instruction));
    assert!(anchor.contains("exact-output guard"));
    assert!(!anchor.contains(".claude/projects"));
    assert!(!anchor.contains("git status"));
}

#[test]
fn claude_code_huge_context_sanitizes_stale_resume_lines() {
    let final_instruction = "Final question: output HUGE_OK only.";
    let mut messages = vec![Message {
        role: "user".to_string(),
        content: Value::String(format!(
            "{}\n{}\n{}",
            "I need to pick up where we left off by reading .claude/projects/session.jsonl and running git status.",
            "controlled context line\n".repeat(40_000),
            final_instruction
        )),
        tool_calls: None,
        tool_call_id: None,
    }];

    let repair = free_model_client_rs::protocol::translate::compact_stream_context_with_policy(
        &mut messages,
        free_model_client_rs::protocol::translate::StreamContextPolicy::claude_code_huge_context(),
    );

    assert!(repair.compacted_messages >= 1);
    let compacted = messages[0].content.as_str().unwrap();
    assert!(compacted.contains("omitted stale ClaudeCode transcript/session recovery lines"));
    assert!(compacted.contains(final_instruction));
    assert!(!compacted.contains(".claude/projects/session.jsonl"));
    assert!(!compacted.contains("git status"));
}

#[test]
fn claude_code_huge_session_folds_old_short_tool_history() {
    let mut messages = vec![Message {
        role: "system".to_string(),
        content: Value::String("ClaudeCode system prompt.".to_string()),
        tool_calls: None,
        tool_call_id: None,
    }];

    for idx in 0..700 {
        messages.push(Message {
            role: if idx % 3 == 0 {
                "tool".to_string()
            } else if idx % 3 == 1 {
                "assistant".to_string()
            } else {
                "user".to_string()
            },
            content: Value::String(format!(
                "old export loop {idx}: QCE 502 timeout running=117 Interrupted; do not keep repeating this stale action. {}",
                "x".repeat(180)
            )),
            tool_calls: None,
            tool_call_id: None,
        });
    }

    messages.push(Message {
        role: "user".to_string(),
        content: Value::String(
            "当前要求：只汇报导出状态，不要重启 NapCat，不要重新生成二维码。".to_string(),
        ),
        tool_calls: None,
        tool_call_id: None,
    });

    let before_len = messages.len();
    let repair =
        free_model_client_rs::protocol::translate::compact_claude_code_huge_session_context(
            &mut messages,
        );
    let prompt = free_model_client_rs::protocol::translate::build_prompt_text(&messages);

    assert!(repair.compacted_messages > 0);
    assert!(messages.len() < before_len / 4);
    assert!(prompt.contains("folded stale ClaudeCode tool/session history"));
    assert!(prompt.contains("running=117") || prompt.contains("502"));
    assert!(prompt.contains("当前要求"));
    assert!(prompt.contains("不要重启"));
    assert!(repair.after_tokens < repair.before_tokens / 2);
}

#[test]
fn claude_code_mid_sized_tool_history_folds_when_latest_user_is_tiny() {
    let mut messages = vec![Message {
        role: "system".to_string(),
        content: Value::String("ClaudeCode system prompt.".to_string()),
        tool_calls: None,
        tool_call_id: None,
    }];

    for idx in 0..86 {
        messages.push(Message {
            role: if idx % 2 == 0 {
                "assistant".to_string()
            } else {
                "user".to_string()
            },
            content: Value::String(format!("old short session message {idx}")),
            tool_calls: None,
            tool_call_id: None,
        });
    }
    messages.insert(
        10,
        Message {
            role: "tool".to_string(),
            content: Value::String(format!(
                "OLD_TOOL_OUTPUT_START\n{}\nOLD_TOOL_OUTPUT_END",
                "tool result line\n".repeat(7_000)
            )),
            tool_calls: None,
            tool_call_id: Some("call_old_tool".to_string()),
        },
    );
    messages.push(Message {
        role: "user".to_string(),
        content: Value::String("继续".to_string()),
        tool_calls: None,
        tool_call_id: None,
    });

    let before_len = messages.len();
    let repair =
        free_model_client_rs::protocol::translate::compact_claude_code_huge_session_context(
            &mut messages,
        );
    let prompt = free_model_client_rs::protocol::translate::build_prompt_text(&messages);

    assert!(repair.compacted_messages > 0);
    assert!(messages.len() < before_len);
    assert!(prompt.contains("folded stale ClaudeCode tool/session history"));
    assert!(prompt.contains("继续"));
    assert!(!prompt.contains("OLD_TOOL_OUTPUT_START"));
}

#[tokio::test]
async fn deepseek_flash_claude_code_huge_exact_output_preserves_upstream_prompt_without_input_wall()
{
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut request = anthropic_request(
        "deepseek-v4-flash",
        &format!(
            "Read this huge controlled local context.\n{}\nFinal question: output HUGE_OK only.",
            "huge-section\n".repeat(80_000)
        ),
        true,
    );
    request.max_tokens = Some(20_000);

    let response = kernel
        .anthropic_messages_with_profile(
            &client,
            request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;
    assert!(body.contains("HUGE_OK"));
    let sent = state.requests.lock().unwrap();
    assert_eq!(sent.len(), 1);
    let messages = sent[0].messages.as_ref().unwrap().as_array().unwrap();
    let prompt = messages
        .iter()
        .filter_map(|message| message["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!prompt.contains("context compactor"));
    assert!(prompt.contains("huge-section"));
    assert!(prompt.contains("Final question: output HUGE_OK only."));
}

#[tokio::test]
async fn deepseek_flash_claude_code_anthropic_non_stream_preserves_large_input_before_upstream() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut request = anthropic_request("deepseek-v4-flash", "placeholder", false);
    request.max_tokens = Some(20_000);
    request.messages.clear();
    request.system = Some(Value::String("ClaudeCode system prompt.".to_string()));

    for idx in 0..700 {
        request.messages.push(AnthropicMessage {
            role: if idx % 2 == 0 {
                "assistant".to_string()
            } else {
                "user".to_string()
            },
            content: Value::String(format!(
                "old non-stream fallback loop {idx}: QCE 502 timeout running=117. {}",
                "x".repeat(620)
            )),
        });
    }
    request.messages.push(AnthropicMessage {
        role: "user".to_string(),
        content: Value::String(
            "当前要求：只汇报导出状态，不要重启 NapCat，不要重新生成二维码。".to_string(),
        ),
    });

    let response = kernel
        .anthropic_messages_with_profile(
            &client,
            request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let sent = state.requests.lock().unwrap();
    assert_eq!(sent[0].max_tokens, Some(20_000));
    let messages = sent[0].messages.as_ref().unwrap().as_array().unwrap();
    let prompt = messages
        .iter()
        .filter_map(|message| message["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(messages.len() > 650);
    assert!(!prompt.contains("folded stale ClaudeCode tool/session history"));
    assert!(prompt.contains("old non-stream fallback loop 0"));
    assert!(prompt.contains("当前要求"));
    assert!(prompt.contains("不要重启"));
}

#[tokio::test]
async fn deepseek_flash_openai_stream_preserves_large_context_before_upstream() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut request = chat_request(
        "deepseek-v4-flash",
        &format!("{}FINAL_MARKER", "x".repeat(420_000)),
        true,
        None,
    );
    request.max_tokens = Some(20_000);
    let response = kernel
        .openai_chat_with_profile(
            &client,
            request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let sent = state.requests.lock().unwrap();
    assert_eq!(sent[0].max_tokens, Some(20_000));
    let messages = sent[0].messages.as_ref().unwrap().as_array().unwrap();
    let content = messages[0]["content"].as_str().unwrap();
    assert!(!content.contains("context compactor"));
    assert!(content.len() > 420_000);
    assert!(content.ends_with("FINAL_MARKER"));
}

#[tokio::test]
async fn deepseek_flash_claude_code_stream_preserves_large_context_without_output_cap() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut request = chat_request(
        "deepseek-v4-flash",
        &format!("{}FINAL_MARKER", "x".repeat(1_000_000)),
        true,
        None,
    );
    request.max_tokens = Some(20_000);
    let response = kernel
        .openai_chat_with_profile(
            &client,
            request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let sent = state.requests.lock().unwrap();
    assert_eq!(sent[0].max_tokens, Some(20_000));
    let messages = sent[0].messages.as_ref().unwrap().as_array().unwrap();
    assert_eq!(messages.len(), 1);
    let content = messages[0]["content"].as_str().unwrap();
    assert!(!content.contains("context compactor"));
    assert!(content.len() > 1_000_000);
    assert!(content.ends_with("FINAL_MARKER"));
}

#[tokio::test]
async fn deepseek_flash_claude_code_anthropic_stream_preserves_large_context_before_upstream() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut request = anthropic_request(
        "deepseek-v4-flash",
        &format!(
            "empty-once\n{}Final question: describe the HUGE_OK marker.",
            "x".repeat(1_000_000)
        ),
        true,
    );
    request.max_tokens = Some(20_000);

    let response = kernel
        .anthropic_messages_with_profile(
            &client,
            request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();

    let body = response_text(response).await;
    assert!(body.contains("HUGE_OK"));
    assert!(body.contains("event: message_stop"));
    let sent = state.requests.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].max_tokens, Some(20_000));
    let messages = sent[0].messages.as_ref().unwrap().as_array().unwrap();
    let prompt = messages
        .iter()
        .filter_map(|message| message["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!prompt.contains("context compactor"));
    assert!(prompt.contains("Final question: describe the HUGE_OK marker."));
}

#[tokio::test]
async fn anthropic_empty_stream_probe_shortcuts_without_upstream() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut request = anthropic_request("deepseek-v4-flash", "ignored", true);
    request.messages.clear();
    request.max_tokens = Some(64);

    let response = kernel
        .anthropic_messages_with_profile(
            &client,
            request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;
    assert!(body.contains("\"text\":\"ok\""));
    assert!(body.contains("event: message_stop"));
    assert_eq!(state.requests.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn openai_empty_stream_probe_shortcuts_without_upstream() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut request = chat_request("deepseek-v4-flash", "ignored", true, None);
    request.messages.clear();
    request.max_tokens = Some(64);

    let response = kernel
        .openai_chat_with_profile(
            &client,
            request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;
    assert!(body.contains("\"content\":\"ok\""));
    assert_eq!(state.requests.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn claude_code_small_low_max_tokens_stream_uses_buffer_retry() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut request = anthropic_request("deepseek-v4-flash", "empty-once", true);
    request.max_tokens = Some(64);

    let response = kernel
        .anthropic_messages_with_profile(
            &client,
            request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();

    let body = response_text(response).await;
    assert!(body.contains("golden answer"));
    assert!(!body.contains("upstream returned no assistant content or tool call"));
    assert_eq!(state.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn claude_code_huge_stream_uses_buffer_retry_for_short_output_guard() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let huge_prompt = format!(
        "Read this long ClaudeCode session.\n{}\nLatest task: trigger empty-once retry and then answer normally.",
        "old tool output line\n".repeat(12_000)
    );
    let mut request = anthropic_request("deepseek-v4-flash", &huge_prompt, true);
    request.max_tokens = Some(2_048);

    let response = kernel
        .anthropic_messages_with_profile(
            &client,
            request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();

    let body = response_text(response).await;
    assert!(body.contains("golden answer"));
    assert!(!body.contains("upstream returned no assistant content or tool call"));
    let requests = state.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].max_tokens, Some(2_048));
    assert_eq!(requests[1].max_tokens, Some(2_048));
}

#[tokio::test]
async fn claude_code_huge_stream_large_output_direct_streams_without_buffer() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let huge_prompt = format!(
        "Read this long ClaudeCode session.\n{}\nLatest task: answer normally.",
        "old tool output line\n".repeat(12_000)
    );
    let mut request = anthropic_request("deepseek-v4-flash", &huge_prompt, true);
    request.max_tokens = Some(32_000);

    let response = kernel
        .anthropic_messages_with_profile(
            &client,
            request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();

    let body = response_text(response).await;
    assert!(body.contains("golden answer"));
    let requests = state.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].max_tokens, Some(32_000));
}

#[tokio::test]
async fn anthropic_channel_test_probe_empty_upstream_falls_back_to_ok() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut request = anthropic_request("deepseek-v4-flash", "echo hi", true);
    request.max_tokens = Some(16);

    let response = kernel
        .anthropic_messages_with_profile(
            &client,
            request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;
    assert!(body.contains("\"text\":\"ok\""));
    assert!(body.contains("event: message_stop"));
    assert!(!body.contains("upstream returned no assistant content or tool call"));
    assert_eq!(state.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn openai_channel_test_probe_empty_upstream_falls_back_to_ok() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut request = chat_request("deepseek-v4-flash", "echo hi", true, None);
    request.max_tokens = Some(16);

    let response = kernel
        .openai_chat_with_profile(
            &client,
            request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;
    assert!(body.contains("\"content\":\"ok\""));
    assert!(!body.contains("upstream returned no assistant content or tool call"));
    assert_eq!(state.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn claude_code_huge_exact_output_uses_upstream_before_literal_fallback() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut request = anthropic_request(
        "deepseek-v4-flash",
        &format!(
            "{}Final question: output HUGE_EMPTY_OK only.",
            "huge-section\n".repeat(80_000)
        ),
        true,
    );
    request.max_tokens = Some(20_000);

    let response = kernel
        .anthropic_messages_with_profile(
            &client,
            request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();

    let body = response_text(response).await;
    assert!(body.contains("HUGE_EMPTY_OK"));
    assert!(body.contains("event: message_stop"));
    assert_eq!(state.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn claude_code_multiline_exact_output_does_not_shortcut_normal_stream() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let literal =
        "# 格式检查\n## 结论\n1. 第一项\n2. 第二项\n\n| 字段 | 值 |\n| --- | --- |\n| 状态 | OK |";
    let request = anthropic_request(
        "deepseek-v4-flash",
        &format!(
            "<system-reminder>\nThe following skills are available.\n</system-reminder>\n\n请严格只输出以下 Markdown 原文，不要加解释，不要改空格，不要增删字符：\n{literal}"
        ),
        true,
    );

    let response = kernel
        .anthropic_messages_with_profile(
            &client,
            request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();

    let body = response_text(response).await;
    assert!(body.contains("golden answer"));
    assert_eq!(state.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn claude_code_recovery_pressure_does_not_shortcut_safe_marker() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let request = anthropic_request(
        "deepseek-v4-flash",
        "Let me check the full transcript to understand where we left off. Read /home/user/.claude/projects/demo/session.jsonl. Previous assistant answer: HUGE_OK.",
        true,
    );

    let response = kernel
        .anthropic_messages_with_profile(
            &client,
            request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();

    let body = response_text(response).await;
    assert!(body.contains("HUGE_OK"));
    assert_eq!(state.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn openclaw_session_summary_pressure_shortcuts_safe_marker_without_upstream() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let request = anthropic_request(
        "deepseek-v4-flash-lite",
        "Previous assistant answer: HUGE_OK.\nThe session is complete. The working tree has 5 files with uncommitted changes. All tests pass with no warnings.",
        true,
    );

    let response = kernel
        .anthropic_messages_with_profile(
            &client,
            request,
            ClientProfile::new(ClientKind::OpenClaw, ClientProfileSource::Header),
        )
        .await
        .unwrap();

    let body = response_text(response).await;
    assert!(body.contains("HUGE_OK"));
    assert_eq!(state.requests.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn openclaw_session_summary_pressure_with_tools_shortcuts_safe_marker_without_upstream() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut request = anthropic_request(
        "deepseek-v4-flash-lite",
        "Previous assistant answer: HUGE_OK.\nThe session is complete. The working tree has 5 files with uncommitted changes. All tests pass with no warnings.",
        true,
    );
    request.tools = Some(vec![ToolDef {
        name: "Read".to_string(),
        description: "Read a local file".to_string(),
        input_schema: ToolInputSchema {
            schema_type: "object".to_string(),
            required: Some(vec!["file_path".to_string()]),
            properties: Some(serde_json::json!({
                "file_path": {"type": "string"}
            })),
        },
    }]);

    let response = kernel
        .anthropic_messages_with_profile(
            &client,
            request,
            ClientProfile::new(ClientKind::OpenClaw, ClientProfileSource::Header),
        )
        .await
        .unwrap();

    let body = response_text(response).await;
    assert!(body.contains("HUGE_OK"));
    assert_eq!(state.requests.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn claude_code_ready_followup_does_not_reuse_recent_exact_output() {
    let (config, client, state) = spawn_mock_zen().await;
    let kernel = FreeModelKernel::new(config);
    let mut exact_request = anthropic_request(
        "deepseek-v4-flash",
        &format!(
            "{}Final question: output HUGE_TTL_OK only.",
            "huge-section\n".repeat(80_000)
        ),
        true,
    );
    exact_request.max_tokens = Some(20_000);

    let first = kernel
        .anthropic_messages_with_profile(
            &client,
            exact_request,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();
    let first_body = response_text(first).await;
    assert!(first_body.contains("HUGE_TTL_OK"));

    let followup = anthropic_request(
        "deepseek-v4-flash",
        "Ready for the next instruction. I reviewed the full context from the project files and session history.",
        true,
    );
    let second = kernel
        .anthropic_messages_with_profile(
            &client,
            followup,
            ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header),
        )
        .await
        .unwrap();
    let second_body = response_text(second).await;
    assert!(second_body.contains("golden answer"));
    assert_eq!(state.requests.lock().unwrap().len(), 2);
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
