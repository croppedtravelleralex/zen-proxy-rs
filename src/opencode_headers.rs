use axum::http::HeaderMap;
use reqwest::RequestBuilder;
use sha2::{Digest, Sha256};

use crate::config::Config;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpencodeHeaderHashes {
    pub user_agent_hash: String,
    pub client_hash: String,
    pub project_hash: String,
    pub session_hash: String,
    pub request_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpencodeHeaderSet {
    pub user_agent: String,
    pub client: String,
    pub project: String,
    pub session: String,
    pub request: String,
    pub hashes: OpencodeHeaderHashes,
}

pub fn build_opencode_headers(
    incoming_headers: &HeaderMap,
    config: &Config,
    client_id: &str,
    model: &str,
) -> Option<OpencodeHeaderSet> {
    if !config.opencode_headers_enabled {
        return None;
    }

    let request = uuid::Uuid::new_v4().to_string();
    let user_agent = format!("opencode/{}", config.opencode_user_agent_version);
    let client = config.opencode_client_name.clone();
    let project = short_hash(&format!("project:{}", config.opencode_project_seed));
    let session = build_session_id(incoming_headers, config, client_id, model);

    Some(OpencodeHeaderSet {
        hashes: OpencodeHeaderHashes {
            user_agent_hash: short_hash(&user_agent),
            client_hash: short_hash(&client),
            project_hash: short_hash(&project),
            session_hash: short_hash(&session),
            request_hash: short_hash(&request),
        },
        user_agent,
        client,
        project,
        session,
        request,
    })
}

pub fn apply_opencode_headers(
    builder: RequestBuilder,
    headers: &OpencodeHeaderSet,
) -> RequestBuilder {
    builder
        .header("User-Agent", &headers.user_agent)
        .header("x-opencode-client", &headers.client)
        .header("x-opencode-project", &headers.project)
        .header("x-opencode-session", &headers.session)
        .header("x-opencode-request", &headers.request)
}

fn build_session_id(
    incoming_headers: &HeaderMap,
    config: &Config,
    client_id: &str,
    model: &str,
) -> String {
    let user_agent = incoming_headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let bucket = chrono::Utc::now().timestamp() / config.opencode_session_ttl_secs.max(1) as i64;
    short_hash(&format!(
        "session:{}:{}:{}:{}:{}",
        config.opencode_project_seed, client_id, model, user_agent, bucket
    ))
}

pub fn short_hash(input: &str) -> String {
    let hash = Sha256::digest(input.as_bytes());
    hex::encode(&hash[..8])
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    fn enabled_config() -> Config {
        let mut config = Config::from_env();
        config.opencode_headers_enabled = true;
        config.opencode_user_agent_version = "test-version".to_string();
        config.opencode_client_name = "cli".to_string();
        config.opencode_project_seed = "project-seed".to_string();
        config.opencode_session_ttl_secs = 3600;
        config
    }

    #[test]
    fn returns_none_when_disabled() {
        let mut config = enabled_config();
        config.opencode_headers_enabled = false;
        let headers = HeaderMap::new();
        assert!(build_opencode_headers(&headers, &config, "client", "model").is_none());
    }

    #[test]
    fn builds_official_header_values_when_enabled() {
        let config = enabled_config();
        let headers = HeaderMap::new();
        let built = build_opencode_headers(&headers, &config, "client", "model").unwrap();
        assert_eq!(built.user_agent, "opencode/test-version");
        assert_eq!(built.client, "cli");
        assert!(!built.project.is_empty());
        assert!(!built.session.is_empty());
        assert!(!built.request.is_empty());
    }

    #[test]
    fn request_id_is_unique_per_call() {
        let config = enabled_config();
        let headers = HeaderMap::new();
        let first = build_opencode_headers(&headers, &config, "client", "model").unwrap();
        let second = build_opencode_headers(&headers, &config, "client", "model").unwrap();
        assert_ne!(first.request, second.request);
    }

    #[test]
    fn session_is_stable_for_same_client_and_model() {
        let config = enabled_config();
        let headers = HeaderMap::new();
        let first = build_opencode_headers(&headers, &config, "client", "model").unwrap();
        let second = build_opencode_headers(&headers, &config, "client", "model").unwrap();
        assert_eq!(first.session, second.session);
    }

    #[test]
    fn session_is_not_global_for_different_clients() {
        let config = enabled_config();
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::USER_AGENT,
            HeaderValue::from_static("agent-a"),
        );
        let first = build_opencode_headers(&headers, &config, "client-a", "model").unwrap();
        let second = build_opencode_headers(&headers, &config, "client-b", "model").unwrap();
        assert_ne!(first.session, second.session);
    }
}
