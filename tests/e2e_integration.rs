use std::net::TcpListener;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SERVER_STARTUP_ATTEMPTS: usize = 8;
const SERVER_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

fn sha256_first8(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(input.as_bytes());
    hex::encode(&hash[..4])
}

fn start_server(_preferred_port: u16) -> (Child, u16) {
    start_server_with_env(_preferred_port, &[])
}

fn start_server_with_env(_preferred_port: u16, envs: &[(&str, &str)]) -> (Child, u16) {
    let mut last_error = String::new();
    for _ in 0..SERVER_STARTUP_ATTEMPTS {
        let port = pick_unused_port();
        let mut child = spawn_server(port, envs);
        match wait_for_server(&mut child, port) {
            Ok(()) => return (child, port),
            Err(err) => {
                last_error = err;
                child.kill().ok();
                child.wait().ok();
                let _ = std::fs::remove_file(node_db_path(port));
            }
        }
    }

    panic!("failed to start e2e server after {SERVER_STARTUP_ATTEMPTS} attempts: {last_error}");
}

fn pick_unused_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve e2e port");
    listener.local_addr().expect("reserved e2e port").port()
}

fn node_db_path(port: u16) -> String {
    format!("/tmp/zen-e2e-{port}.json")
}

fn spawn_server(port: u16, envs: &[(&str, &str)]) -> Child {
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
        .env("NODE_DB_PATH", node_db_path(port));
    for (key, value) in envs {
        command.env(key, value);
    }

    command.spawn().expect("failed to start server")
}

fn wait_for_server(child: &mut Child, port: u16) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(250))
        .build()
        .map_err(|err| format!("failed to build readiness client: {err}"))?;
    let url = format!("http://127.0.0.1:{port}/health");
    let deadline = Instant::now() + SERVER_STARTUP_TIMEOUT;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|err| format!("failed to poll server process: {err}"))?
        {
            return Err(format!(
                "server exited before readiness on port {port}: {status}"
            ));
        }

        let last_error = match client.get(&url).send() {
            Ok(resp) if resp.status().is_success() => return Ok(()),
            Ok(resp) => format!("readiness returned {}", resp.status()),
            Err(err) => err.to_string(),
        };

        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for server on port {port}; last readiness error: {last_error}"
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn stop_server(mut child: Child, port: u16) {
    child.kill().ok();
    child.wait().ok();
    let _ = std::fs::remove_file(node_db_path(port));
}

fn start_mock_zen() -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
    use axum::extract::DefaultBodyLimit;
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
                .with_state(state)
                .layer(DefaultBodyLimit::max(8 * 1024 * 1024));
            let listener = tokio::net::TcpListener::from_std(std_listener).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });

    (format!("http://{addr}/zen"), observed)
}

fn start_mock_models(body: serde_json::Value) -> String {
    use axum::routing::get;
    use axum::{Json, Router};

    let body = Arc::new(body);
    let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = std_listener.local_addr().unwrap();
    std_listener.set_nonblocking(true).unwrap();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let app = Router::new().route(
                "/v1/models",
                get({
                    let body = body.clone();
                    move || {
                        let body = body.clone();
                        async move { Json((*body).clone()) }
                    }
                }),
            );
            let listener = tokio::net::TcpListener::from_std(std_listener).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });

    format!("http://{addr}/v1/models")
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

        let client = reqwest::blocking::Client::new();
        for (probe_name, header, value) in [
            ("openai", "user-agent", "OpenAI/Python 1.0"),
            ("anthropic", "anthropic-client", "anthropic-sdk-rust/0.1"),
        ] {
            let probe_resp = client
                .get(format!("http://127.0.0.1:{}/v1/models", port))
                .header(header, value)
                .send()
                .unwrap_or_else(|err| panic!("{probe_name} model probe failed: {err}"));
            assert_eq!(probe_resp.status(), 200, "{probe_name} model probe");
            let probe_body: serde_json::Value = probe_resp.json().unwrap();
            let probe_ids: Vec<&str> = probe_body["data"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|model| model["id"].as_str())
                .collect();
            assert_eq!(
                probe_ids,
                vec!["deepseek-v4-flash", "deepseek-v4-flash-lite"],
                "{probe_name} model probe ids"
            );
        }
        stop_server(child, port);
    }

    #[test]
    fn test_dynamic_discovery_stays_admin_only() {
        let discovery_url = start_mock_models(serde_json::json!({
            "object": "list",
            "data": [
                {"id": "deepseek-v4-flash-free"},
                {"id": "new-opencode-free"},
                {"id": "paid-model"}
            ]
        }));
        let (child, port) = start_server_with_env(
            19790,
            &[
                ("ZEN_PROVIDER_MODE", "free_model_kernel"),
                ("DYNAMIC_MODEL_DISCOVERY_ENABLED", "true"),
                ("DYNAMIC_MODEL_DISCOVERY_URL", discovery_url.as_str()),
                ("DYNAMIC_MODEL_DISCOVERY_INTERVAL_SECS", "60"),
            ],
        );

        let public_resp = reqwest::blocking::get(format!("http://127.0.0.1:{}/v1/models", port))
            .expect("public models endpoint");
        assert_eq!(public_resp.status(), 200);
        let public_body: serde_json::Value = public_resp.json().unwrap();
        let public_ids: Vec<&str> = public_body["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|model| model["id"].as_str())
            .collect();
        assert_eq!(
            public_ids,
            vec!["deepseek-v4-flash", "deepseek-v4-flash-lite"]
        );

        let client = reqwest::blocking::Client::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        let admin_body = loop {
            let resp = client
                .get(format!("http://127.0.0.1:{}/admin/models", port))
                .header("x-api-key", "test-key")
                .send()
                .expect("admin models endpoint");
            assert_eq!(resp.status(), 200);
            let body: serde_json::Value = resp.json().unwrap();
            if body["data"]["dynamic_discovery"]["candidate_total"]
                .as_u64()
                .unwrap_or_default()
                >= 2
            {
                break body;
            }
            if Instant::now() >= deadline {
                panic!("dynamic discovery did not populate admin candidates: {body}");
            }
            std::thread::sleep(Duration::from_millis(100));
        };

        let discovery = &admin_body["data"]["dynamic_discovery"];
        assert_eq!(discovery["enabled"], true);
        assert_eq!(discovery["worker_running"], true);
        assert_eq!(discovery["candidate_total"], 2);
        assert_eq!(discovery["ignored_total"], 1);
        assert_eq!(discovery["missing_total"], 0);
        assert_eq!(admin_body["data"]["safety"]["candidates_are_public"], false);
        assert_eq!(admin_body["data"]["safety"]["auto_promote"], false);

        let detail = client
            .get(format!(
                "http://127.0.0.1:{}/admin/models/new-opencode-free",
                port
            ))
            .header("x-api-key", "test-key")
            .send()
            .expect("admin dynamic model detail");
        assert_eq!(detail.status(), 200);
        let detail_body: serde_json::Value = detail.json().unwrap();
        assert_eq!(detail_body["data"]["mode"], "dynamic_candidate");
        assert_eq!(detail_body["data"]["public"], false);
        assert_eq!(detail_body["data"]["probe_required"], true);
        assert_eq!(detail_body["data"]["auto_promoted"], false);

        let rejected = client
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", port))
            .json(&serde_json::json!({
                "model": "new-opencode-free",
                "messages": [{"role": "user", "content": "hello"}],
                "stream": false
            }))
            .send()
            .expect("candidate data-plane request");
        assert_eq!(rejected.status(), 400);
        let rejected_body: serde_json::Value = rejected.json().unwrap();
        assert!(rejected_body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("unsupported V4 model"));

        stop_server(child, port);
    }

    #[test]
    fn test_dynamic_canary_public_mode_exposes_promoted_models_only() {
        let discovery_url = start_mock_models(serde_json::json!({
            "object": "list",
            "data": [
                {"id": "new-opencode-free"},
                {"id": "paid-model"}
            ]
        }));
        let (child, port) = start_server_with_env(
            19791,
            &[
                ("ZEN_PROVIDER_MODE", "free_model_kernel"),
                ("DYNAMIC_MODEL_DISCOVERY_ENABLED", "true"),
                ("DYNAMIC_MODEL_DISCOVERY_URL", discovery_url.as_str()),
                ("DYNAMIC_MODEL_DISCOVERY_INTERVAL_SECS", "60"),
                ("DYNAMIC_MODEL_PUBLIC_MODE", "canary_or_active"),
            ],
        );

        let client = reqwest::blocking::Client::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let resp = client
                .get(format!("http://127.0.0.1:{}/admin/models", port))
                .header("x-api-key", "test-key")
                .send()
                .expect("admin models endpoint");
            assert_eq!(resp.status(), 200);
            let body: serde_json::Value = resp.json().unwrap();
            if body["data"]["dynamic_discovery"]["candidate_total"]
                .as_u64()
                .unwrap_or_default()
                >= 1
            {
                break;
            }
            if Instant::now() >= deadline {
                panic!("dynamic discovery did not populate candidates: {body}");
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        let before_resp = reqwest::blocking::get(format!("http://127.0.0.1:{}/v1/models", port))
            .expect("public models endpoint before promote");
        let before_body: serde_json::Value = before_resp.json().unwrap();
        let before_ids: Vec<&str> = before_body["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|model| model["id"].as_str())
            .collect();
        assert_eq!(
            before_ids,
            vec!["deepseek-v4-flash", "deepseek-v4-flash-lite"]
        );

        let promoted = client
            .post(format!(
                "http://127.0.0.1:{}/admin/models/new-opencode-free/promote",
                port
            ))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({"state": "canary"}))
            .send()
            .expect("promote dynamic model");
        assert_eq!(promoted.status(), 200);
        let promoted_body: serde_json::Value = promoted.json().unwrap();
        assert_eq!(promoted_body["data"]["state"], "canary");
        assert_eq!(promoted_body["data"]["public"], true);
        assert_eq!(promoted_body["data"]["routable"], true);

        let public_resp = reqwest::blocking::get(format!("http://127.0.0.1:{}/v1/models", port))
            .expect("public models endpoint after promote");
        assert_eq!(public_resp.status(), 200);
        let public_body: serde_json::Value = public_resp.json().unwrap();
        let public_ids: Vec<&str> = public_body["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|model| model["id"].as_str())
            .collect();
        assert_eq!(
            public_ids,
            vec![
                "deepseek-v4-flash",
                "deepseek-v4-flash-lite",
                "new-opencode-free"
            ]
        );

        let detail = reqwest::blocking::get(format!(
            "http://127.0.0.1:{}/v1/models/new-opencode-free",
            port
        ))
        .expect("dynamic public model detail");
        assert_eq!(detail.status(), 200);
        let detail_body: serde_json::Value = detail.json().unwrap();
        assert_eq!(detail_body["id"], "new-opencode-free");
        assert_eq!(detail_body["upstream_id"], "new-opencode-free");

        let paid_detail =
            reqwest::blocking::get(format!("http://127.0.0.1:{}/v1/models/paid-model", port))
                .expect("ignored model detail");
        assert_eq!(paid_detail.status(), 404);

        stop_server(child, port);
    }

    #[test]
    fn test_dynamic_active_only_mode_excludes_canary_until_active() {
        let discovery_url = start_mock_models(serde_json::json!({
            "object": "list",
            "data": [{"id": "new-active-only-free"}]
        }));
        let (child, port) = start_server_with_env(
            19792,
            &[
                ("ZEN_PROVIDER_MODE", "free_model_kernel"),
                ("DYNAMIC_MODEL_DISCOVERY_ENABLED", "true"),
                ("DYNAMIC_MODEL_DISCOVERY_URL", discovery_url.as_str()),
                ("DYNAMIC_MODEL_DISCOVERY_INTERVAL_SECS", "60"),
                ("DYNAMIC_MODEL_PUBLIC_MODE", "active_only"),
            ],
        );

        let client = reqwest::blocking::Client::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let resp = client
                .get(format!("http://127.0.0.1:{}/admin/models", port))
                .header("x-api-key", "test-key")
                .send()
                .expect("admin models endpoint");
            assert_eq!(resp.status(), 200);
            let body: serde_json::Value = resp.json().unwrap();
            if body["data"]["dynamic_discovery"]["candidate_total"]
                .as_u64()
                .unwrap_or_default()
                >= 1
            {
                break;
            }
            if Instant::now() >= deadline {
                panic!("dynamic discovery did not populate candidates: {body}");
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        let canary = client
            .post(format!(
                "http://127.0.0.1:{}/admin/models/new-active-only-free/promote",
                port
            ))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({"state": "canary"}))
            .send()
            .expect("canary promote dynamic model");
        assert_eq!(canary.status(), 200);

        let canary_detail = reqwest::blocking::get(format!(
            "http://127.0.0.1:{}/v1/models/new-active-only-free",
            port
        ))
        .expect("canary detail in active_only mode");
        assert_eq!(canary_detail.status(), 404);

        let active = client
            .post(format!(
                "http://127.0.0.1:{}/admin/models/new-active-only-free/promote",
                port
            ))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({"state": "active"}))
            .send()
            .expect("active promote dynamic model");
        assert_eq!(active.status(), 200);

        let active_detail = reqwest::blocking::get(format!(
            "http://127.0.0.1:{}/v1/models/new-active-only-free",
            port
        ))
        .expect("active detail in active_only mode");
        assert_eq!(active_detail.status(), 200);

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
                "model": "deepseek-v4-flash-lite",
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
        assert_eq!(seen[0]["body"]["model"], "big-pickle");
        assert_eq!(seen[1]["body"]["model"], "big-pickle");
        assert_eq!(seen[2]["body"]["model"], "big-pickle");
        assert!(seen[2]["body"].get("max_tokens").is_none());
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
            .find(|item| item["public_model"] == "deepseek-v4-flash-lite")
            .unwrap();
        assert!(openai_record["rid"].as_str().is_some());
        assert_eq!(openai_record["upstream_model"], "big-pickle");
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
                "model": "deepseek-v4-flash-lite",
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
        assert_eq!(seen[0]["body"]["model"], "big-pickle");
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
    fn test_v4_flash_input_wall_passes_large_old_tool_result_before_upstream() {
        let (upstream_base, observed) = start_mock_zen();
        let (child, port) = start_server_with_env(
            19807,
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
                ("CONTEXT_TOKEN_COMPACT", "100"),
                ("CONTEXT_TOKEN_TARGET", "100"),
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
                    {"role": "user", "content": "y".repeat(2 * 1024)}
                ],
                "stream": false
            }))
            .send()
            .expect("v4 flash pass-through openai request");
        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers()
                .get("x-zen-context-trimmed")
                .and_then(|value| value.to_str().ok()),
            Some("false")
        );
        assert_eq!(
            resp.headers()
                .get("x-zen-context-action")
                .and_then(|value| value.to_str().ok()),
            Some("warn")
        );

        let seen = observed.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["body"]["model"], "deepseek-v4-flash-free");
        let upstream_messages = seen[0]["body"]["messages"].as_array().unwrap();
        assert_eq!(
            upstream_messages.last().unwrap()["content"],
            "y".repeat(2 * 1024)
        );
        let tool_content = upstream_messages
            .iter()
            .find(|message| message["role"] == "tool")
            .and_then(|message| message["content"].as_str())
            .expect("paired tool result should remain protocol-shaped");
        assert!(!tool_content.contains("ZenProxy context compactor"));
        assert_eq!(tool_content.len(), 2 * 1024 * 1024);
        stop_server(child, port);
    }

    #[test]
    fn test_v4_nonstream_guard_preserves_large_prompt_output_before_upstream() {
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
            Some("pass")
        );

        let seen = observed.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["body"]["max_tokens"], 4096);
        stop_server(child, port);
    }

    #[test]
    fn test_v4_nonstream_guard_preserves_huge_prompt_long_output() {
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
            .expect("v4 nonstream preserved request");
        assert_eq!(resp.status(), 200);
        let seen = observed.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["body"]["max_tokens"], 20_000);
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
    fn test_v4_free_model_kernel_propagates_source_client_and_model_profile_policy() {
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

        let source_client_resp = client
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", port))
            .header("x-zen-source-client", "openclaw")
            .json(&serde_json::json!({
                "model": "deepseek-v4-flash-lite",
                "messages": [{"role":"user","content":"use tool"}],
                "tools": tools.clone(),
                "stream": false
            }))
            .send()
            .expect("source_client profile request");
        assert_eq!(source_client_resp.status(), 200);

        let flash_resp = client
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", port))
            .header("x-fmc-client", "openclaw")
            .json(&serde_json::json!({
                "model": "deepseek-v4-flash",
                "messages": [{"role":"user","content":"use tool"}],
                "tools": tools.clone(),
                "stream": false
            }))
            .send()
            .expect("flash model profile request");
        assert_eq!(flash_resp.status(), 200);

        let lite_claude_resp = client
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", port))
            .header("x-fmc-client", "claude-code")
            .json(&serde_json::json!({
                "model": "deepseek-v4-flash-lite",
                "messages": [{"role":"user","content":"use tool"}],
                "tools": tools.clone(),
                "stream": false
            }))
            .send()
            .expect("lite claude profile request");
        assert_eq!(lite_claude_resp.status(), 200);

        let seen = observed.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0]["body"]["model"], "big-pickle");
        assert_eq!(
            seen[0]["body"]["thinking"],
            serde_json::json!({"type":"disabled"})
        );
        assert_eq!(seen[1]["body"]["model"], "deepseek-v4-flash-free");
        assert!(seen[1]["body"]["thinking"].is_null());
        assert_eq!(seen[2]["body"]["model"], "big-pickle");
        assert!(seen[2]["body"]["thinking"].is_null());
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
