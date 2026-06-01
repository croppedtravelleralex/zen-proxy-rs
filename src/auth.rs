use crate::config::Config;

pub fn request_api_key(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
        })
}

/// Check whether the request is authorized.
/// If require_api_key is false in config, all requests pass.
/// Otherwise the Authorization header must match the configured API key.
pub fn is_authorized(config: &Config, auth_header: Option<&str>) -> bool {
    if !config.require_api_key {
        return true;
    }
    let expected_bearer = format!("Bearer {}", config.api_key);
    auth_header == Some(&expected_bearer) || auth_header == Some(&config.api_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_config(require_api_key: bool, api_key: &str) -> Config {
        Config {
            host: "127.0.0.1".into(),
            port: 14118,
            zen_chat_url: "".into(),
            zen_api_key: "".into(),
            require_api_key,
            api_key: api_key.into(),
            timeout: Duration::from_secs(120),
            request_body_limit_bytes: 64 * 1024 * 1024,
            free_models: vec![],
            model_mappings: vec![],
        }
    }

    #[test]
    fn auth_bypass_when_not_required() {
        let cfg = make_config(false, "sk-dev");
        assert!(is_authorized(&cfg, None));
        assert!(is_authorized(&cfg, Some("garbage")));
    }

    #[test]
    fn auth_passes_with_correct_key() {
        let cfg = make_config(true, "sk-secret");
        assert!(is_authorized(&cfg, Some("Bearer sk-secret")));
    }

    #[test]
    fn auth_fails_with_wrong_key() {
        let cfg = make_config(true, "sk-secret");
        assert!(!is_authorized(&cfg, Some("Bearer wrong")));
    }

    #[test]
    fn auth_fails_with_missing_header() {
        let cfg = make_config(true, "sk-secret");
        assert!(!is_authorized(&cfg, None));
    }

    #[test]
    fn request_api_key_accepts_authorization_or_x_api_key() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-api-key", "sk-x".parse().unwrap());
        assert_eq!(request_api_key(&headers), Some("sk-x"));

        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer sk-auth".parse().unwrap(),
        );
        assert_eq!(request_api_key(&headers), Some("Bearer sk-auth"));
    }
}
