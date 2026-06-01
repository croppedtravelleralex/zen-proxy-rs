use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn sha256_first8(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(input.as_bytes());
    hex::encode(&hash[..4])
}

fn start_server(port: u16) -> (Child, u16) {
    start_server_with_env(port, &[])
}

fn start_server_with_env(port: u16, envs: &[(&str, &str)]) -> (Child, u16) {
    let exe = option_env!("CARGO_BIN_EXE_zen-proxy-rs")
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            if cfg!(debug_assertions) {
                format!("{}/target/debug/zen-proxy-rs", env!("CARGO_MANIFEST_DIR"))
            } else {
                format!("{}/target/release/zen-proxy-rs", env!("CARGO_MANIFEST_DIR"))
            }
        });

    let mut command = Command::new(&exe);
    command
        .env("PORT", port.to_string())
        .env("BIND_ADDRESS", "127.0.0.1")
        .env("PROXY_TOKEN_MODE", "unlimited")
        .env("ADMIN_API_KEY", "test-key")
        .env("NODES_FILE", "/dev/null")
        .env("NODE_DB_PATH", format!("/tmp/zen-e2e-{}.json", port));
    for (key, value) in envs {
        command.env(key, value);
    }

    let child = command.spawn().expect("failed to start server");
    std::thread::sleep(Duration::from_secs(4));
    (child, port)
}

fn stop_server(mut child: Child, port: u16) {
    child.kill().ok();
    child.wait().ok();
    let _ = std::fs::remove_file(format!("/tmp/zen-e2e-{}.json", port));
}

fn start_mock_zen() -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::{Json, Router};

    let observed = Arc::new(Mutex::new(Vec::new()));
    let state = observed.clone();
    let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = std_listener.local_addr().unwrap();
    std_listener.set_nonblocking(true).unwrap();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            async fn handler(
                State(observed): State<Arc<Mutex<Vec<serde_json::Value>>>>,
                headers: axum::http::HeaderMap,
                Json(body): Json<serde_json::Value>,
            ) -> impl IntoResponse {
                observed.lock().unwrap().push(serde_json::json!({
                    "body": body,
                    "selected_node_id": headers
                        .get("x-zen-proxy-selected-node-id")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default(),
                    "selected_node_url": headers
                        .get("x-zen-proxy-selected-node-url")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                }));
                if body
                    .get("messages")
                    .and_then(|messages| messages.as_array())
                    .and_then(|messages| messages.last())
                    .and_then(|message| message.get("content"))
                    .and_then(|content| content.as_str())
                    == Some("rate-limit")
                {
                    return (
                        StatusCode::TOO_MANY_REQUESTS,
                        [("retry-after", "60")],
                        "FreeUsageLimitError",
                    )
                        .into_response();
                }
                let chunk = serde_json::json!({
                    "choices": [{"delta": {"content": "zen v4 ok"}}],
                    "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5}
                });
                let body = format!("data: {}\n\ndata: [DONE]\n\n", chunk);
                (
                    StatusCode::OK,
                    [
                        ("content-type", "text/event-stream"),
                        ("x-zen-observed-exit-ip", "direct"),
                    ],
                    body,
                )
                    .into_response()
            }

            let app = Router::new()
                .route("/zen/v1/chat/completions", post(handler))
                .with_state(state);
            let listener = tokio::net::TcpListener::from_std(std_listener).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });

    (format!("http://{addr}/zen"), observed)
}

#[cfg(test)]
mod e2e {
    use super::*;

    #[test]
    fn test_health() {
        let (child, port) = start_server(19781);
        let resp = reqwest::blocking::get(format!("http://127.0.0.1:{}/health", port))
            .expect("health endpoint");
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["status"], "ok");
        stop_server(child, port);
    }

    #[test]
    fn test_metrics() {
        let (child, port) =
            start_server_with_env(19782, &[("PREFERRED_PROXY_URLS", "http://127.0.0.1:7897")]);
        let resp = reqwest::blocking::get(format!("http://127.0.0.1:{}/metrics", port))
            .expect("metrics endpoint");
        assert_eq!(resp.status(), 200);
        let text = resp.text().unwrap();
        assert!(
            text.contains("zen_proxy_requests_total"),
            "metrics should contain counter"
        );
        assert!(
            text.contains("zen_proxy_pool_size{pool=\"dispatch\"} 1"),
            "metrics should contain live dispatch pool size: {text}"
        );
        stop_server(child, port);
    }

    #[test]
    fn test_index() {
        let (child, port) = start_server(19783);
        let resp =
            reqwest::blocking::get(format!("http://127.0.0.1:{}/", port)).expect("index endpoint");
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["service"], "zen-proxy-rs");
        stop_server(child, port);
    }

    #[test]
    fn test_admin_unauthorized() {
        let (child, port) = start_server(19784);
        let client = reqwest::blocking::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{}/admin/stats", port))
            .send()
            .expect("admin/stats endpoint");
        assert_eq!(resp.status(), 401, "no API key should be rejected");
        stop_server(child, port);
    }

    #[test]
    fn test_admin_authorized() {
        let (child, port) = start_server(19785);
        let client = reqwest::blocking::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{}/admin/stats", port))
            .header("x-api-key", "test-key")
            .send()
            .expect("admin/stats endpoint");
        assert_eq!(resp.status(), 200, "valid API key should be accepted");
        let body: serde_json::Value = resp.json().unwrap();
        assert!(body["success"].as_bool().unwrap_or(false));
        assert!(body["data"].is_object());
        stop_server(child, port);
    }

    #[test]
    fn test_models_endpoint() {
        let (child, port) = start_server(19786);
        let resp = reqwest::blocking::get(format!("http://127.0.0.1:{}/v1/models", port))
            .expect("models endpoint");
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["object"], "list");
        let ids: Vec<&str> = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|model| model["id"].as_str())
            .collect();
        assert_eq!(ids, vec!["deepseek-v4-flash", "deepseek-v4-pro"]);
        stop_server(child, port);
    }

    #[test]
    fn test_models_endpoint_v4_mode() {
        let (child, port) =
            start_server_with_env(19789, &[("ZEN_PROVIDER_MODE", "free_model_kernel")]);
        let resp = reqwest::blocking::get(format!("http://127.0.0.1:{}/v1/models", port))
            .expect("models endpoint");
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["object"], "list");
        let ids: Vec<&str> = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|model| model["id"].as_str())
            .collect();
        assert_eq!(ids, vec!["deepseek-v4-flash", "deepseek-v4-flash-lite"]);

        let detail = reqwest::blocking::get(format!(
            "http://127.0.0.1:{}/v1/models/deepseek-v4-flash",
            port
        ))
        .expect("model detail endpoint");
        assert_eq!(detail.status(), 200);
        let detail_body: serde_json::Value = detail.json().unwrap();
        assert_eq!(detail_body["id"], "deepseek-v4-flash");
        assert_eq!(detail_body["upstream_id"], "deepseek-v4-flash-free");

        let missing = reqwest::blocking::get(format!(
            "http://127.0.0.1:{}/v1/models/deepseek-v4-pro",
            port
        ))
        .expect("missing model detail endpoint");
        assert_eq!(missing.status(), 404);
        stop_server(child, port);
    }

    #[test]
    fn test_models_alias_endpoint_v4_mode() {
        let (child, port) =
            start_server_with_env(19797, &[("ZEN_PROVIDER_MODE", "free_model_kernel")]);
        let resp = reqwest::blocking::get(format!("http://127.0.0.1:{}/models", port))
            .expect("models alias endpoint");
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        let ids: Vec<&str> = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|model| model["id"].as_str())
            .collect();
        assert_eq!(ids, vec!["deepseek-v4-flash", "deepseek-v4-flash-lite"]);
        stop_server(child, port);
    }

    #[test]
    fn test_v4_openai_and_anthropic_use_free_model_kernel() {
        let (upstream_base, observed) = start_mock_zen();
        let (child, port) = start_server_with_env(
            19790,
            &[
                ("ZEN_PROVIDER_MODE", "free_model_kernel"),
                ("UPSTREAM_BASE", upstream_base.as_str()),
                ("POOL_MAX_RETRIES", "0"),
                ("ALLOW_DIRECT_FALLBACK", "true"),
            ],
        );
        let client = reqwest::blocking::Client::new();

        let openai_resp = client
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", port))
            .json(&serde_json::json!({
                "model": "deepseek-v4-flash",
                "messages": [{"role": "user", "content": "hello"}],
                "stream": false
            }))
            .send()
            .expect("v4 openai request");
        assert_eq!(openai_resp.status(), 200);
        let openai_body: serde_json::Value = openai_resp.json().unwrap();
        assert_eq!(openai_body["choices"][0]["message"]["content"], "zen v4 ok");

        let anthropic_resp = client
            .post(format!("http://127.0.0.1:{}/v1/messages", port))
            .json(&serde_json::json!({
                "model": "deepseek-v4-flash-lite",
                "messages": [{"role": "user", "content": "hello"}],
                "max_tokens": 64,
                "stream": false
            }))
            .send()
            .expect("v4 anthropic request");
        assert_eq!(anthropic_resp.status(), 200);
        let anthropic_body: serde_json::Value = anthropic_resp.json().unwrap();
        assert_eq!(anthropic_body["content"][0]["text"], "zen v4 ok");

        let anthropic_health_resp = client
            .post(format!("http://127.0.0.1:{}/v1/messages", port))
            .json(&serde_json::json!({
                "model": "deepseek-v4-flash-lite",
                "messages": [{"role": "user", "content": "hello"}],
                "stream": false
            }))
            .send()
            .expect("v4 anthropic health-style request");
        assert_eq!(anthropic_health_resp.status(), 200);

        let seen = observed.lock().unwrap();
        assert_eq!(seen[0]["body"]["model"], "deepseek-v4-flash-free");
        assert_eq!(seen[1]["body"]["model"], "big-pickle");
        assert_eq!(seen[2]["body"]["model"], "big-pickle");
        assert_eq!(seen[2]["body"]["max_tokens"], 1024);
        assert_eq!(seen[0]["selected_node_id"], "direct");
        assert_eq!(seen[0]["selected_node_url"], "direct");

        let requests_resp = client
            .get(format!("http://127.0.0.1:{}/admin/requests?limit=10", port))
            .header("x-api-key", "test-key")
            .send()
            .expect("admin requests endpoint");
        assert_eq!(requests_resp.status(), 200);
        let requests_body: serde_json::Value = requests_resp.json().unwrap();
        let items = requests_body["data"].as_array().unwrap();
        let openai_record = items
            .iter()
            .find(|item| item["public_model"] == "deepseek-v4-flash")
            .unwrap();
        assert!(openai_record["rid"].as_str().is_some());
        assert_eq!(openai_record["upstream_model"], "deepseek-v4-flash-free");
        assert_eq!(openai_record["selected_node_id"], "direct");
        assert_eq!(openai_record["selected_node_url_redacted"], "direct");
        assert_eq!(openai_record["observed_exit_ip"], "direct");
        assert_eq!(openai_record["outcome"], "success");
        assert_eq!(openai_record["prompt_tokens"], 2);
        assert_eq!(openai_record["completion_tokens"], 3);
        assert_eq!(openai_record["total_tokens"], 5);
        assert!(openai_record["bytes_received"].as_u64().unwrap_or(0) > 0);
        stop_server(child, port);
    }

    #[test]
    fn test_v4_proxy_api_key_accepts_x_api_key_header() {
        let (upstream_base, observed) = start_mock_zen();
        let (child, port) = start_server_with_env(
            19800,
            &[
                ("ZEN_PROVIDER_MODE", "free_model_kernel"),
                ("UPSTREAM_BASE", upstream_base.as_str()),
                ("POOL_MAX_RETRIES", "0"),
                ("ALLOW_DIRECT_FALLBACK", "true"),
                ("PROXY_API_KEY", "sk-dev"),
            ],
        );
        let client = reqwest::blocking::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", port))
            .header("x-api-key", "sk-dev")
            .json(&serde_json::json!({
                "model": "deepseek-v4-flash",
                "messages": [{"role": "user", "content": "hello"}],
                "stream": false
            }))
            .send()
            .expect("v4 openai request with x-api-key");
        assert_eq!(resp.status(), 200);
        assert_eq!(observed.lock().unwrap().len(), 1);
        stop_server(child, port);
    }

    #[test]
    fn test_v4_ingress_accepts_body_over_axum_default_limit() {
        let (child, port) = start_server_with_env(
            19802,
            &[
                ("ZEN_PROVIDER_MODE", "free_model_kernel"),
                ("REQUEST_BODY_LIMIT_MB", "8"),
            ],
        );
        let client = reqwest::blocking::Client::new();
        let body = serde_json::json!({
            "model": "not-a-v4-model",
            "messages": [{"role": "user", "content": "x".repeat(3 * 1024 * 1024)}],
            "stream": false
        })
        .to_string();
        let resp = client
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", port))
            .header("content-type", "application/json")
            .body(body)
            .send()
            .expect("large invalid v4 request");
        assert_eq!(resp.status(), 400);
        let text = resp.text().unwrap();
        assert!(
            text.contains("unsupported V4 model"),
            "large request should reach V4 handler, got {text}"
        );
        stop_server(child, port);
    }

    #[test]
    fn test_v4_compactor_trims_large_old_tool_result_before_upstream() {
        let (upstream_base, observed) = start_mock_zen();
        let (child, port) = start_server_with_env(
            19803,
            &[
                ("ZEN_PROVIDER_MODE", "free_model_kernel"),
                ("UPSTREAM_BASE", upstream_base.as_str()),
                ("POOL_MAX_RETRIES", "0"),
                ("ALLOW_DIRECT_FALLBACK", "true"),
                ("REQUEST_BODY_LIMIT_MB", "8"),
                ("ZEN_COMPACTOR_MODE", "enforce"),
                ("ZEN_ARTIFACT_CACHE_MODE", "off"),
                ("CONTEXT_COMPACT_BODY_MB", "1"),
                ("CONTEXT_TARGET_BODY_MB", "1"),
                ("CONTEXT_LARGE_CHUNK_BYTES", "1024"),
                ("CONTEXT_PRESERVE_RECENT_MESSAGES", "8"),
            ],
        );
        let client = reqwest::blocking::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", port))
            .json(&serde_json::json!({
                "model": "deepseek-v4-flash",
                "messages": [
                    {"role": "assistant", "content": null, "tool_calls": [{"id": "old-tool", "type": "function", "function": {"name": "Read", "arguments": "{}"}}]},
                    {"role": "tool", "content": "x".repeat(2 * 1024 * 1024), "tool_call_id": "old-tool"},
                    {"role": "assistant", "content": "recent assistant"},
                    {"role": "user", "content": "latest user"}
                ],
                "stream": false
            }))
            .send()
            .expect("v4 compacted openai request");
        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers()
                .get("x-zen-context-trimmed")
                .and_then(|value| value.to_str().ok()),
            Some("true")
        );
        assert_eq!(
            resp.headers()
                .get("x-zen-context-action")
                .and_then(|value| value.to_str().ok()),
            Some("compact")
        );

        let seen = observed.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let upstream_messages = seen[0]["body"]["messages"].as_array().unwrap();
        assert_eq!(upstream_messages.last().unwrap()["content"], "latest user");
        let compacted_tool = upstream_messages
            .iter()
            .find(|message| message["role"] == "tool")
            .and_then(|message| message["content"].as_str())
            .expect("paired tool result should remain protocol-shaped");
        assert!(compacted_tool.contains("ZenProxy context compactor"));
        assert!(compacted_tool.len() < 16 * 1024);
        stop_server(child, port);
    }

    #[test]
    fn test_v4_nonstream_guard_caps_large_prompt_before_upstream() {
        let (upstream_base, observed) = start_mock_zen();
        let (child, port) = start_server_with_env(
            19805,
            &[
                ("ZEN_PROVIDER_MODE", "free_model_kernel"),
                ("UPSTREAM_BASE", upstream_base.as_str()),
                ("POOL_MAX_RETRIES", "0"),
                ("ALLOW_DIRECT_FALLBACK", "true"),
                ("REQUEST_BODY_LIMIT_MB", "8"),
            ],
        );
        let client = reqwest::blocking::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", port))
            .json(&serde_json::json!({
                "model": "deepseek-v4-flash",
                "messages": [{"role": "user", "content": "x".repeat(220_000)}],
                "max_tokens": 4096,
                "stream": false
            }))
            .send()
            .expect("v4 nonstream guarded request");
        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers()
                .get("x-zen-nonstream-guard-action")
                .and_then(|value| value.to_str().ok()),
            Some("cap_long_prompt_2048")
        );

        let seen = observed.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["body"]["max_tokens"], 2048);
        stop_server(child, port);
    }

    #[test]
    fn test_v4_nonstream_guard_rejects_huge_prompt_long_output() {
        let (upstream_base, observed) = start_mock_zen();
        let (child, port) = start_server_with_env(
            19806,
            &[
                ("ZEN_PROVIDER_MODE", "free_model_kernel"),
                ("UPSTREAM_BASE", upstream_base.as_str()),
                ("POOL_MAX_RETRIES", "0"),
                ("ALLOW_DIRECT_FALLBACK", "true"),
                ("REQUEST_BODY_LIMIT_MB", "8"),
            ],
        );
        let client = reqwest::blocking::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", port))
            .json(&serde_json::json!({
                "model": "deepseek-v4-flash",
                "messages": [{"role": "user", "content": "x".repeat(440_000)}],
                "max_tokens": 20_000,
                "stream": false
            }))
            .send()
            .expect("v4 nonstream rejected request");
        assert_eq!(resp.status(), 422);
        assert!(resp.text().unwrap().contains("use stream=true"));
        assert!(observed.lock().unwrap().is_empty());
        stop_server(child, port);
    }

    #[test]
    fn test_v4_protocol_guard_repairs_openai_tool_history_before_upstream() {
        let (upstream_base, observed) = start_mock_zen();
        let (child, port) = start_server_with_env(
            19804,
            &[
                ("ZEN_PROVIDER_MODE", "free_model_kernel"),
                ("UPSTREAM_BASE", upstream_base.as_str()),
                ("POOL_MAX_RETRIES", "0"),
                ("ALLOW_DIRECT_FALLBACK", "true"),
                ("PROTOCOL_GUARD_MODE", "repair"),
            ],
        );
        let client = reqwest::blocking::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", port))
            .header("user-agent", "OpenClaw-test")
            .json(&serde_json::json!({
                "model": "deepseek-v4-flash",
                "messages": [
                    {"role":"assistant","content":null,"tool_calls":[{"id":"call_guard_1","type":"function","function":{"name":"Read","arguments":"{}"}}]},
                    {"role":"tool","content":"file contents"},
                    {"role":"user","content":"continue"}
                ],
                "stream": false
            }))
            .send()
            .expect("v4 protocol guard request");
        assert_eq!(resp.status(), 200);

        let seen = observed.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let upstream_messages = seen[0]["body"]["messages"].as_array().unwrap();
        let tool_message = upstream_messages
            .iter()
            .find(|message| message["role"] == "tool")
            .expect("tool message should remain protocol-shaped");
        assert_eq!(tool_message["tool_call_id"], "call_guard_1");
        drop(seen);

        let requests_resp = client
            .get(format!("http://127.0.0.1:{}/admin/requests?limit=10", port))
            .header("x-api-key", "test-key")
            .send()
            .expect("admin requests endpoint");
        assert_eq!(requests_resp.status(), 200);
        let requests_body: serde_json::Value = requests_resp.json().unwrap();
        let record = requests_body["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["protocol_guard"]["applied"] == true)
            .expect("protocol guard telemetry record");
        assert_eq!(record["protocol_guard"]["source_client"], "openclaw");
        assert_eq!(record["protocol_guard"]["missing_tool_call_id_count"], 1);
        assert_eq!(record["protocol_guard"]["post_valid"], true);
        stop_server(child, port);
    }

    #[test]
    fn test_v4_free_model_kernel_propagates_client_profile() {
        let (upstream_base, observed) = start_mock_zen();
        let (child, port) = start_server_with_env(
            19811,
            &[
                ("ZEN_PROVIDER_MODE", "free_model_kernel"),
                ("UPSTREAM_BASE", upstream_base.as_str()),
                ("POOL_MAX_RETRIES", "0"),
                ("ALLOW_DIRECT_FALLBACK", "true"),
            ],
        );
        let client = reqwest::blocking::Client::new();
        let tools = serde_json::json!([
            {"type":"function","function":{"name":"Task","parameters":{"type":"object","properties":{}}}}
        ]);

        let openclaw_resp = client
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", port))
            .header("x-fmc-client", "openclaw")
            .json(&serde_json::json!({
                "model": "deepseek-v4-flash",
                "messages": [{"role":"user","content":"use tool"}],
                "tools": tools.clone(),
                "stream": false
            }))
            .send()
            .expect("openclaw profile request");
        assert_eq!(openclaw_resp.status(), 200);

        let claude_resp = client
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", port))
            .header("x-fmc-client", "claude-code")
            .json(&serde_json::json!({
                "model": "deepseek-v4-flash",
                "messages": [{"role":"user","content":"use tool"}],
                "tools": tools.clone(),
                "stream": false
            }))
            .send()
            .expect("claude profile request");
        assert_eq!(claude_resp.status(), 200);

        let seen = observed.lock().unwrap();
        assert_eq!(
            seen[0]["body"]["thinking"],
            serde_json::json!({"type":"disabled"})
        );
        assert_ne!(
            seen[1]["body"]["thinking"],
            serde_json::json!({"type":"disabled"})
        );
        stop_server(child, port);
    }

    #[test]
    fn test_v4_stream_telemetry_records_bytes_and_usage() {
        let (upstream_base, _) = start_mock_zen();
        let (child, port) = start_server_with_env(
            19801,
            &[
                ("ZEN_PROVIDER_MODE", "free_model_kernel"),
                ("UPSTREAM_BASE", upstream_base.as_str()),
                ("POOL_MAX_RETRIES", "0"),
                ("ALLOW_DIRECT_FALLBACK", "true"),
            ],
        );
        let client = reqwest::blocking::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", port))
            .json(&serde_json::json!({
                "model": "deepseek-v4-flash-lite",
                "messages": [{"role": "user", "content": "hello"}],
                "stream": true
            }))
            .send()
            .expect("v4 openai stream request");
        let status = resp.status();
        let body = resp.text().unwrap();
        assert_eq!(status, 200, "stream response body: {body}");
        assert!(body.contains("data: [DONE]"));

        let requests_resp = client
            .get(format!("http://127.0.0.1:{}/admin/requests?limit=10", port))
            .header("x-api-key", "test-key")
            .send()
            .expect("admin requests endpoint");
        assert_eq!(requests_resp.status(), 200);
        let requests_body: serde_json::Value = requests_resp.json().unwrap();
        let stream_record = requests_body["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["is_streaming"] == true)
            .expect("stream telemetry record");
        assert_eq!(stream_record["prompt_tokens"], 2);
        assert_eq!(stream_record["completion_tokens"], 3);
        assert_eq!(stream_record["total_tokens"], 5);
        assert!(stream_record["bytes_received"].as_u64().unwrap_or(0) > 0);
        stop_server(child, port);
    }

    #[test]
    fn test_v4_upstream_429_returns_retry_after() {
        let (upstream_base, observed) = start_mock_zen();
        let (child, port) = start_server_with_env(
            19791,
            &[
                ("ZEN_PROVIDER_MODE", "free_model_kernel"),
                ("UPSTREAM_BASE", upstream_base.as_str()),
                ("POOL_MAX_RETRIES", "0"),
                ("ALLOW_DIRECT_FALLBACK", "true"),
            ],
        );
        let client = reqwest::blocking::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", port))
            .json(&serde_json::json!({
                "model": "deepseek-v4-flash",
                "messages": [{"role": "user", "content": "rate-limit"}],
                "stream": false
            }))
            .send()
            .expect("v4 openai rate-limit request");
        assert_eq!(resp.status(), 429);
        assert_eq!(
            resp.headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok()),
            Some("60")
        );
        let seen = observed.lock().unwrap();
        assert_eq!(seen[0]["selected_node_id"], "direct");
        assert_eq!(seen.len(), 1, "POOL_MAX_RETRIES=0 must not retry");
        drop(seen);
        let requests_resp = client
            .get(format!("http://127.0.0.1:{}/admin/requests?limit=10", port))
            .header("x-api-key", "test-key")
            .send()
            .expect("admin requests endpoint");
        assert_eq!(requests_resp.status(), 200);
        let requests_body: serde_json::Value = requests_resp.json().unwrap();
        let items = requests_body["data"].as_array().unwrap();
        let rate_limited_record = items
            .iter()
            .find(|item| item["status"] == 429)
            .expect("429 request telemetry");
        assert_eq!(rate_limited_record["outcome"], "rate_limited");
        assert_eq!(rate_limited_record["retry_count"], 0);
        stop_server(child, port);
    }

    #[test]
    fn test_v4_transport_failure_returns_bad_gateway() {
        let (child, port) = start_server_with_env(
            19792,
            &[
                ("ZEN_PROVIDER_MODE", "free_model_kernel"),
                ("UPSTREAM_BASE", "http://127.0.0.1:9/zen"),
                ("POOL_MAX_RETRIES", "0"),
                ("ALLOW_DIRECT_FALLBACK", "true"),
            ],
        );
        let client = reqwest::blocking::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", port))
            .json(&serde_json::json!({
                "model": "deepseek-v4-flash",
                "messages": [{"role": "user", "content": "hello"}],
                "stream": false
            }))
            .send()
            .expect("v4 openai transport-failure request");
        assert_eq!(resp.status(), 502);
        let requests_resp = client
            .get(format!("http://127.0.0.1:{}/admin/requests?limit=10", port))
            .header("x-api-key", "test-key")
            .send()
            .expect("admin requests endpoint");
        assert_eq!(requests_resp.status(), 200);
        let requests_body: serde_json::Value = requests_resp.json().unwrap();
        let items = requests_body["data"].as_array().unwrap();
        let failure_record = items
            .iter()
            .find(|item| item["status"] == 502)
            .expect("transport failure request telemetry");
        assert_eq!(failure_record["outcome"], "transport_error");
        assert_eq!(failure_record["retry_count"], 0);
        stop_server(child, port);
    }

    #[test]
    fn test_runtime_rollback_uses_same_binary_for_legacy_and_v4() {
        let (upstream_base, _) = start_mock_zen();
        let (legacy_child, legacy_port) = start_server(19793);
        let legacy_resp =
            reqwest::blocking::get(format!("http://127.0.0.1:{}/v1/models", legacy_port))
                .expect("legacy models endpoint");
        assert_eq!(legacy_resp.status(), 200);
        let legacy_body: serde_json::Value = legacy_resp.json().unwrap();
        let legacy_ids: Vec<&str> = legacy_body["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|model| model["id"].as_str())
            .collect();
        assert_eq!(legacy_ids, vec!["deepseek-v4-flash", "deepseek-v4-pro"]);
        stop_server(legacy_child, legacy_port);

        let (v4_child, v4_port) = start_server_with_env(
            19794,
            &[
                ("ZEN_PROVIDER_MODE", "free_model_kernel"),
                ("UPSTREAM_BASE", upstream_base.as_str()),
            ],
        );
        let v4_resp = reqwest::blocking::get(format!("http://127.0.0.1:{}/v1/models", v4_port))
            .expect("v4 models endpoint");
        assert_eq!(v4_resp.status(), 200);
        let v4_body: serde_json::Value = v4_resp.json().unwrap();
        let v4_ids: Vec<&str> = v4_body["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|model| model["id"].as_str())
            .collect();
        assert_eq!(v4_ids, vec!["deepseek-v4-flash", "deepseek-v4-flash-lite"]);
        stop_server(v4_child, v4_port);
    }

    #[test]
    fn test_admin_nodes_requires_auth() {
        let (child, port) = start_server(19787);
        let client = reqwest::blocking::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{}/admin/nodes", port))
            .send()
            .expect("admin/nodes endpoint");
        assert_eq!(resp.status(), 401, "no API key should be rejected");
        stop_server(child, port);
    }

    #[test]
    fn test_admin_nodes_returns_summary() {
        let (child, port) = start_server(19788);
        let client = reqwest::blocking::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{}/admin/nodes", port))
            .header("x-api-key", "test-key")
            .send()
            .expect("admin/nodes endpoint");
        assert_eq!(resp.status(), 200, "valid API key should be accepted");
        let body: serde_json::Value = resp.json().unwrap();
        assert!(body["success"].as_bool().unwrap_or(false));
        assert!(body["data"]["pools"]["total"].is_number());
        assert_eq!(body["data"]["allow_direct_fallback"], false);
        stop_server(child, port);
    }

    #[test]
    fn test_admin_ready_reports_not_ready_without_nodes() {
        let (child, port) = start_server(19796);
        let client = reqwest::blocking::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{}/admin/health/ready", port))
            .header("x-api-key", "test-key")
            .send()
            .expect("admin ready endpoint");
        assert_eq!(resp.status(), 503);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["data"]["status"], "not_ready");
        assert_eq!(body["data"]["details"]["direct_fallback_active"], false);
        stop_server(child, port);
    }

    #[test]
    fn test_admin_read_api_coverage() {
        let (child, port) = start_server(19798);
        let client = reqwest::blocking::Client::new();
        let paths = [
            "/admin/health",
            "/admin/health/live",
            "/admin/routes",
            "/admin/runtime",
            "/admin/models",
            "/admin/models/deepseek-v4-flash",
            "/admin/budget",
            "/admin/budget/nodes",
            "/admin/stats",
            "/admin/stats/models",
            "/admin/stats/nodes",
            "/admin/stats/pools",
            "/admin/stats/upstream",
            "/admin/pools",
            "/admin/pools/dispatch",
            "/admin/pools/active",
            "/admin/pools/ratelimited",
            "/admin/pools/dead",
            "/admin/fuse",
            "/admin/requests",
            "/admin/requests/recent",
            "/admin/requests/summary",
            "/admin/requests/timings",
            "/admin/requests/models",
            "/admin/requests/nodes",
            "/admin/events",
            "/admin/events/recent",
            "/admin/events/probes",
            "/admin/ledger",
            "/admin/ledger/models",
            "/admin/ledger/keys",
            "/admin/ledger/streams",
            "/admin/config",
            "/admin/config/validation",
            "/admin/system/uptime",
            "/admin/system/info",
            "/admin/requests/export?limit=5",
        ];

        for path in paths {
            let resp = client
                .get(format!("http://127.0.0.1:{port}{path}"))
                .header("x-api-key", "test-key")
                .send()
                .unwrap_or_else(|err| panic!("GET {path} failed: {err}"));
            assert_eq!(resp.status(), 200, "GET {path}");
        }

        let timings = client
            .get(format!("http://127.0.0.1:{}/admin/requests/timings", port))
            .header("x-api-key", "test-key")
            .send()
            .expect("request timings endpoint");
        assert_eq!(timings.status(), 200);
        let timings_body: serde_json::Value = timings.json().unwrap();
        let avg = &timings_body["data"]["avg"];
        assert!(
            avg.get("protocol_first_byte_ms").is_some(),
            "timings avg should expose protocol_first_byte_ms"
        );
        assert!(avg.get("first_content_token_ms").is_some());
        assert!(avg.get("first_tool_call_ms").is_some());

        let missing = client
            .get(format!(
                "http://127.0.0.1:{}/admin/requests/missing-rid",
                port
            ))
            .header("x-api-key", "test-key")
            .send()
            .expect("request detail missing endpoint");
        assert_eq!(missing.status(), 404);

        let unknown_pool = client
            .get(format!("http://127.0.0.1:{}/admin/pools/unknown", port))
            .header("x-api-key", "test-key")
            .send()
            .expect("unknown pool endpoint");
        assert_eq!(unknown_pool.status(), 404);

        let missing_model = client
            .get(format!(
                "http://127.0.0.1:{}/admin/models/not-a-model",
                port
            ))
            .header("x-api-key", "test-key")
            .send()
            .expect("missing admin model endpoint");
        assert_eq!(missing_model.status(), 404);

        let budget_nodes = client
            .get(format!("http://127.0.0.1:{}/admin/budget/nodes", port))
            .header("x-api-key", "test-key")
            .send()
            .expect("budget nodes endpoint");
        assert_eq!(budget_nodes.status(), 200);
        let body: serde_json::Value = budget_nodes.json().unwrap();
        assert!(body["data"]["nodes"].as_array().unwrap().is_empty());

        stop_server(child, port);
    }

    #[test]
    fn test_admin_write_api_coverage() {
        let (child, port) = start_server(19799);
        let client = reqwest::blocking::Client::new();

        let fuse_resp = client
            .post(format!("http://127.0.0.1:{}/admin/fuse", port))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({"open": true}))
            .send()
            .expect("fuse set endpoint");
        assert_eq!(fuse_resp.status(), 200);
        let fuse_body: serde_json::Value = fuse_resp.json().unwrap();
        assert_eq!(fuse_body["data"]["fuse"], true);

        let unfuse_resp = client
            .post(format!("http://127.0.0.1:{}/admin/fuse", port))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({"open": false}))
            .send()
            .expect("fuse unset endpoint");
        assert_eq!(unfuse_resp.status(), 200);

        let node_url = "http://127.0.0.1:9";
        let node_id = sha256_first8(node_url);
        let add_resp = client
            .post(format!("http://127.0.0.1:{}/admin/nodes", port))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({"url": node_url}))
            .send()
            .expect("node add endpoint");
        assert_eq!(add_resp.status(), 200);

        let nodes_resp = client
            .get(format!("http://127.0.0.1:{}/admin/nodes", port))
            .header("x-api-key", "test-key")
            .send()
            .expect("nodes endpoint");
        let nodes_body: serde_json::Value = nodes_resp.json().unwrap();
        assert_eq!(nodes_body["data"]["pools"]["dispatch"], 1);

        let budget_resp = client
            .get(format!(
                "http://127.0.0.1:{}/admin/nodes/{}/budget",
                port, node_id
            ))
            .header("x-api-key", "test-key")
            .send()
            .expect("node budget endpoint");
        assert_eq!(budget_resp.status(), 200);
        let budget_body: serde_json::Value = budget_resp.json().unwrap();
        assert_eq!(budget_body["data"]["node_id"], node_id);
        assert!(budget_body["data"]["local_budget"].is_object());

        let probe_missing_resp = client
            .post(format!(
                "http://127.0.0.1:{}/admin/nodes/missing-node/probe",
                port
            ))
            .header("x-api-key", "test-key")
            .send()
            .expect("node missing probe endpoint");
        assert_eq!(probe_missing_resp.status(), 404);

        let recover_resp = client
            .post(format!(
                "http://127.0.0.1:{}/admin/nodes/{}/recover",
                port, node_id
            ))
            .header("x-api-key", "test-key")
            .send()
            .expect("node recover endpoint");
        assert_eq!(recover_resp.status(), 200);

        let delete_resp = client
            .delete(format!("http://127.0.0.1:{}/admin/nodes/{}", port, node_id))
            .header("x-api-key", "test-key")
            .send()
            .expect("node delete endpoint");
        assert_eq!(delete_resp.status(), 200);

        let log_resp = client
            .post(format!(
                "http://127.0.0.1:{}/admin/system/log-level/info",
                port
            ))
            .header("x-api-key", "test-key")
            .send()
            .expect("log level endpoint");
        assert_eq!(log_resp.status(), 200);

        let reload_resp = client
            .post(format!("http://127.0.0.1:{}/admin/config/reload", port))
            .header("x-api-key", "test-key")
            .send()
            .expect("config reload endpoint");
        assert_eq!(reload_resp.status(), 200);

        let probe_resp = client
            .post(format!("http://127.0.0.1:{}/admin/probe/now", port))
            .header("x-api-key", "test-key")
            .send()
            .expect("probe now endpoint");
        assert_eq!(probe_resp.status(), 200);

        let missing_url_resp = client
            .post(format!("http://127.0.0.1:{}/admin/nodes", port))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({}))
            .send()
            .expect("node add missing url endpoint");
        assert_eq!(missing_url_resp.status(), 400);

        let invalid_log_resp = client
            .post(format!(
                "http://127.0.0.1:{}/admin/system/log-level/nope",
                port
            ))
            .header("x-api-key", "test-key")
            .send()
            .expect("invalid log level endpoint");
        assert_eq!(invalid_log_resp.status(), 400);

        stop_server(child, port);
    }

    #[test]
    fn test_v4_without_nodes_and_without_direct_fallback_returns_503() {
        let (upstream_base, observed) = start_mock_zen();
        let (child, port) = start_server_with_env(
            19795,
            &[
                ("ZEN_PROVIDER_MODE", "free_model_kernel"),
                ("UPSTREAM_BASE", upstream_base.as_str()),
                ("POOL_MAX_RETRIES", "0"),
            ],
        );
        let client = reqwest::blocking::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", port))
            .json(&serde_json::json!({
                "model": "deepseek-v4-flash",
                "messages": [{"role": "user", "content": "hello"}],
                "stream": false
            }))
            .send()
            .expect("v4 no-resource request");
        assert_eq!(resp.status(), 503);
        assert_eq!(observed.lock().unwrap().len(), 0);
        stop_server(child, port);
    }
}
