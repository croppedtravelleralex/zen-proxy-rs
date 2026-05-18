use std::collections::HashMap;
use std::env;
use std::str::FromStr;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub bind_address: String,
    pub upstream_base: String,
    pub chat_target: String,
    pub model_target: String,
    pub admin_api_key: Option<String>,
    pub proxy_error_threshold: u32,
    pub proxy_cooldown_seconds: u64,
    pub proxy_recovery_interval: u64,
    pub pool_max_retries: u32,
    pub pool_max_size: u32,
    pub connect_timeout_secs: u64,
    pub request_timeout_secs: u64,
    pub probe_timeout_secs: u64,
    pub probe_connect_timeout_secs: u64,
    pub pool_warm_interval_secs: u64,
    pub probe_batch_size: usize,
    pub dispatch_capacity: usize,
    pub active_capacity: usize,
    pub ratelimited_capacity: usize,
    pub dead_capacity: usize,
    pub model_override: Option<String>,
    pub model_mapping: HashMap<String, String>,
    pub allow_direct_fallback: bool,
    pub benchmark_mode: bool,
    pub log_level: String,
    pub sticky_ttl_secs: f64,
    pub nodes_file: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            port: load_env_var("PORT", 4000u16),
            bind_address: load_env_var("BIND_ADDRESS", "127.0.0.1".to_string()),
            upstream_base: load_env_var("UPSTREAM_BASE", "https://opencode.ai/zen".to_string()),
            chat_target: load_env_var("CHAT_TARGET", "/v1/chat/completions".to_string()),
            model_target: load_env_var("MODEL_TARGET", "/v1/models".to_string()),
            admin_api_key: match env::var("ADMIN_API_KEY") {
                Ok(v) if !v.is_empty() => Some(v),
                _ => None,
            },
            proxy_error_threshold: load_env_var("PROXY_ERROR_THRESHOLD", 5u32),
            proxy_cooldown_seconds: load_env_var("PROXY_COOLDOWN_SECONDS", 60u64),
            proxy_recovery_interval: load_env_var("PROXY_RECOVERY_INTERVAL", 30u64),
            pool_max_retries: load_env_var("POOL_MAX_RETRIES", 3u32),
            pool_max_size: load_env_var("POOL_MAX_SIZE", 128u32),
            connect_timeout_secs: load_env_var("CONNECT_TIMEOUT_SECS", 5u64),
            request_timeout_secs: load_env_var("REQUEST_TIMEOUT_SECS", 120u64),
            probe_timeout_secs: load_env_var("PROBE_TIMEOUT_SECS", 30u64),
            probe_connect_timeout_secs: load_env_var("PROBE_CONNECT_TIMEOUT_SECS", 10u64),
            pool_warm_interval_secs: load_env_var("POOL_WARM_INTERVAL_SECS", 10u64),
            probe_batch_size: load_env_var("PROBE_BATCH_SIZE", 5usize),
            dispatch_capacity: load_env_var("DISPATCH_CAPACITY", 100usize),
            active_capacity: load_env_var("ACTIVE_CAPACITY", 100usize),
            ratelimited_capacity: load_env_var("RATELIMITED_CAPACITY", 100usize),
            dead_capacity: load_env_var("DEAD_CAPACITY", 100usize),
            model_override: match env::var("MODEL_OVERRIDE") {
                Ok(v) if !v.is_empty() => Some(v),
                _ => None,
            },
            model_mapping: Self::default_model_mapping(),
            allow_direct_fallback: load_env_var("ALLOW_DIRECT_FALLBACK", false),
            benchmark_mode: load_env_var("BENCHMARK_MODE", false),
            log_level: load_env_var("LOG_LEVEL", "info".to_string()),
            sticky_ttl_secs: load_env_var("STICKY_TTL_SECS", 180.0f64),
            nodes_file: env::var("NODES_FILE")
                .unwrap_or_else(|_| "/etc/zen-proxy/nodes.json".into()),
        }
    }

    pub fn reload(&mut self) {
        *self = Self::from_env();
    }

    fn default_model_mapping() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("deepseek-v4-flash".to_string(), "big-pickle".to_string());
        m.insert(
            "deepseek-v4-flash-lite".to_string(),
            "big-pickle-nothinking".to_string(),
        );
        m.insert(
            "deepseek-v4-pro".to_string(),
            "deepseek-v4-flash-free".to_string(),
        );
        m.insert(
            "deepseek-v4-pro-lite".to_string(),
            "deepseek-v4-flash-nothinking".to_string(),
        );
        m
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.bind_address, self.port)
    }

    pub fn chat_url(&self) -> String {
        format!("{}{}", self.upstream_base, self.chat_target)
    }

    pub fn model_url(&self) -> String {
        format!("{}{}", self.upstream_base, self.model_target)
    }

    pub fn connect_timeout(&self) -> Duration {
        Duration::from_secs(self.connect_timeout_secs)
    }

    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs)
    }

    pub fn probe_timeout(&self) -> Duration {
        Duration::from_secs(self.probe_timeout_secs)
    }

    pub fn probe_connect_timeout(&self) -> Duration {
        Duration::from_secs(self.probe_connect_timeout_secs)
    }

    pub fn pool_warm_interval(&self) -> Duration {
        Duration::from_secs(self.pool_warm_interval_secs)
    }

    pub fn sticky_ttl(&self) -> Duration {
        Duration::from_secs_f64(self.sticky_ttl_secs)
    }

    pub fn load_nodes(&self) -> Vec<String> {
        match std::fs::read_to_string(&self.nodes_file) {
            Ok(contents) => match serde_json::from_str::<Vec<String>>(&contents) {
                Ok(nodes) => {
                    tracing::info!(count = nodes.len(), file = %self.nodes_file, "loaded proxy nodes");
                    nodes
                }
                Err(e) => {
                    tracing::warn!(file = %self.nodes_file, error = %e, "failed to parse nodes file, using empty pool");
                    Vec::new()
                }
            },
            Err(_) => {
                tracing::warn!(file = %self.nodes_file, "nodes file not found, using empty pool (direct-only)");
                Vec::new()
            }
        }
    }
}

pub fn load_env_var<T: FromStr>(key: &str, default: T) -> T {
    match env::var(key) {
        Ok(raw) if !raw.is_empty() => match raw.parse::<T>() {
            Ok(val) => val,
            Err(_) => {
                tracing::warn!(
                    "env var {} has unparseable value \"{}\", using default",
                    key,
                    raw
                );
                default
            }
        },
        Ok(_) | Err(_) => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_uses_defaults_when_unset() {
        for key in &["PORT", "MODEL_OVERRIDE", "ADMIN_API_KEY"] {
            env::remove_var(key);
        }

        let cfg = Config::from_env();
        assert_eq!(cfg.port, 4000);
        assert!(cfg.admin_api_key.is_none());
        assert!(cfg.model_override.is_none());
        assert_eq!(cfg.allow_direct_fallback, false);
        assert_eq!(cfg.benchmark_mode, false);
        assert_eq!(cfg.log_level, "info");
        assert_eq!(cfg.probe_timeout_secs, 30);
        assert_eq!(cfg.probe_batch_size, 5);
        assert_eq!(cfg.dispatch_capacity, 100);
    }

    #[test]
    fn from_env_reads_env_overrides() {
        unsafe { env::set_var("PORT", "8080") };
        unsafe { env::set_var("LOG_LEVEL", "debug") };
        unsafe { env::set_var("PROBE_BATCH_SIZE", "10") };

        let cfg = Config::from_env();
        assert_eq!(cfg.port, 8080);
        assert_eq!(cfg.log_level, "debug");
        assert_eq!(cfg.probe_batch_size, 10);

        env::remove_var("PORT");
        env::remove_var("LOG_LEVEL");
        env::remove_var("PROBE_BATCH_SIZE");
    }

    #[test]
    fn from_env_graceful_on_bad_values() {
        unsafe { env::set_var("PORT", "not-a-number") };

        let cfg = Config::from_env();
        assert_eq!(cfg.port, 4000);

        env::remove_var("PORT");
    }

    #[test]
    fn model_mapping_is_pre_populated() {
        let cfg = Config::from_env();
        assert_eq!(
            cfg.model_mapping.get("deepseek-v4-flash").unwrap(),
            "big-pickle"
        );
        assert_eq!(
            cfg.model_mapping.get("deepseek-v4-flash-lite").unwrap(),
            "big-pickle-nothinking"
        );
        assert_eq!(
            cfg.model_mapping.get("deepseek-v4-pro").unwrap(),
            "deepseek-v4-flash-free"
        );
        assert_eq!(
            cfg.model_mapping.get("deepseek-v4-pro-lite").unwrap(),
            "deepseek-v4-flash-nothinking"
        );
        assert_eq!(cfg.model_mapping.len(), 4);
    }

    #[test]
    fn reload_re_reads_env() {
        let mut cfg = Config::from_env();

        unsafe { env::set_var("PORT", "9999") };
        cfg.reload();
        assert_eq!(cfg.port, 9999);

        env::remove_var("PORT");
    }

    #[test]
    fn load_env_var_returns_default_on_empty_var() {
        unsafe { env::set_var("PORT", "") };
        let port: u16 = load_env_var("PORT", 4000u16);
        assert_eq!(port, 4000);
        env::remove_var("PORT");
    }

    #[test]
    fn convenience_accessors() {
        let cfg = Config::from_env();
        assert_eq!(cfg.bind_addr(), "127.0.0.1:4000");
        assert!(cfg.chat_url().ends_with("/v1/chat/completions"));
        assert!(cfg.model_url().ends_with("/v1/models"));
        assert_eq!(cfg.connect_timeout(), Duration::from_secs(5));
        assert_eq!(cfg.request_timeout(), Duration::from_secs(120));
        assert_eq!(cfg.probe_timeout(), Duration::from_secs(30));
        assert_eq!(cfg.probe_connect_timeout(), Duration::from_secs(10));
        assert_eq!(cfg.pool_warm_interval(), Duration::from_secs(10));
        assert_eq!(cfg.sticky_ttl(), Duration::from_secs_f64(180.0));
    }

    #[test]
    fn model_override_none_when_unset() {
        env::remove_var("MODEL_OVERRIDE");
        let cfg = Config::from_env();
        assert!(cfg.model_override.is_none());
    }

    #[test]
    fn model_override_some_when_set() {
        unsafe { env::set_var("MODEL_OVERRIDE", "custom-model") };
        let cfg = Config::from_env();
        assert_eq!(cfg.model_override.as_deref(), Some("custom-model"));
        env::remove_var("MODEL_OVERRIDE");
    }

    #[test]
    fn model_override_none_when_empty() {
        unsafe { env::set_var("MODEL_OVERRIDE", "") };
        let cfg = Config::from_env();
        assert!(cfg.model_override.is_none());
        env::remove_var("MODEL_OVERRIDE");
    }

    #[test]
    fn admin_api_key_none_when_unset() {
        env::remove_var("ADMIN_API_KEY");
        let cfg = Config::from_env();
        assert!(cfg.admin_api_key.is_none());
    }

    #[test]
    fn admin_api_key_some_when_set() {
        unsafe { env::set_var("ADMIN_API_KEY", "secret-key-123") };
        let cfg = Config::from_env();
        assert_eq!(cfg.admin_api_key.as_deref(), Some("secret-key-123"));
        env::remove_var("ADMIN_API_KEY");
    }

    #[test]
    fn nodes_file_default() {
        env::remove_var("NODES_FILE");
        let cfg = Config::from_env();
        assert_eq!(cfg.nodes_file, "/etc/zen-proxy/nodes.json");
    }
}
