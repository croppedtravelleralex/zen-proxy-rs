use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn start_server(port: u16) -> (Child, u16) {
    start_server_with_env(port, &[])
}

fn start_server_with_env(port: u16, envs: &[(&str, &str)]) -> (Child, u16) {
    let exe = if cfg!(debug_assertions) {
        format!("{}/target/debug/zen-proxy-rs", env!("CARGO_MANIFEST_DIR"))
    } else {
        format!("{}/target/release/zen-proxy-rs", env!("CARGO_MANIFEST_DIR"))
    };

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
                Json(body): Json<serde_json::Value>,
            ) -> impl IntoResponse {
                observed.lock().unwrap().push(body.clone());
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
                    [("content-type", "text/event-stream")],
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
        let (child, port) = start_server(19782);
        let resp = reqwest::blocking::get(format!("http://127.0.0.1:{}/metrics", port))
            .expect("metrics endpoint");
        assert_eq!(resp.status(), 200);
        let text = resp.text().unwrap();
        assert!(
            text.contains("zen_proxy_requests_total"),
            "metrics should contain counter"
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

        let seen = observed.lock().unwrap();
        assert_eq!(seen[0]["model"], "deepseek-v4-flash-free");
        assert_eq!(seen[1]["model"], "big-pickle");
        stop_server(child, port);
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
        assert!(body["data"]["total_requests"].is_number());
        stop_server(child, port);
    }
}
