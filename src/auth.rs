use crate::config::Config;

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
}
