use std::process::{Command, Child};
use std::time::Duration;

fn start_server(port: u16) -> (Child, u16) {
    let exe = if cfg!(debug_assertions) {
        format!("{}/target/debug/zen-proxy-rs", env!("CARGO_MANIFEST_DIR"))
    } else {
        format!("{}/target/release/zen-proxy-rs", env!("CARGO_MANIFEST_DIR"))
    };

    let child = Command::new(&exe)
        .env("PORT", port.to_string())
        .env("BIND_ADDRESS", "127.0.0.1")
        .env("PROXY_TOKEN_MODE", "unlimited")
        .env("ADMIN_API_KEY", "test-key")
        .env("NODES_FILE", "/dev/null")
        .env("NODE_DB_PATH", format!("/tmp/zen-e2e-{}.json", port))
        .spawn()
        .expect("failed to start server");
    std::thread::sleep(Duration::from_secs(4));
    (child, port)
}

fn stop_server(mut child: Child, port: u16) {
    child.kill().ok();
    child.wait().ok();
    let _ = std::fs::remove_file(format!("/tmp/zen-e2e-{}.json", port));
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
        assert!(text.contains("zen_proxy_requests_total"), "metrics should contain counter");
        stop_server(child, port);
    }

    #[test]
    fn test_index() {
        let (child, port) = start_server(19783);
        let resp = reqwest::blocking::get(format!("http://127.0.0.1:{}/", port))
            .expect("index endpoint");
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
        assert_eq!(body["status"], "ok");
        assert!(body["stats"].is_object());
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
        stop_server(child, port);
    }
}
