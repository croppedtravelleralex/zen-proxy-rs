use std::collections::HashMap;
use std::env;
use std::str::FromStr;
use std::time::Duration;

/// Token rate limiting modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenMode {
    /// Fixed token bucket rate.
    Fixed,
    /// Dynamically adjusted rate based on proxy health.
    Adaptive,
    /// No rate limiting.
    Unlimited,
}

/// Central configuration for the zen-proxy-rs service.
///
/// All fields have sensible defaults and can be overridden via environment
/// variables. The `reload()` method re-reads env vars at runtime, intended
/// for SIGHUP-based hot-reload.
#[derive(Debug, Clone)]
pub struct Config {
    /// TCP port to bind the HTTP server on.
    pub port: u16,
    /// IP address to bind the HTTP server to.
    pub bind_address: String,
    /// Base URL of the upstream OpenCode AI API.
    pub upstream_base: String,
    /// Chat completions endpoint path.
    pub chat_target: String,
    /// Model listing endpoint path.
    pub model_target: String,
    /// Token bucket mode: "fixed", "adaptive", or "unlimited".
    pub token_mode: String,
    /// Token replenish rate per second (fixed mode).
    pub token_rate: f64,
    /// Maximum token burst size.
    pub token_burst: f64,
    /// Minimum tokens/sec allowed in adaptive mode.
    pub adaptive_min_rate: f64,
    /// Maximum tokens/sec allowed in adaptive mode.
    pub adaptive_max_rate: f64,
    /// Sliding window size in seconds for adaptive rate calculation.
    pub adaptive_window: u64,
    /// Consecutive errors before a proxy node is blacklisted.
    pub proxy_error_threshold: u32,
    /// Seconds a blacklisted proxy stays in cooldown.
    pub proxy_cooldown_seconds: u64,
    /// Seconds between attempts to recover a blacklisted proxy.
    pub proxy_recovery_interval: u64,
    /// Maximum retry attempts across the proxy pool for a single request.
    pub pool_max_retries: u32,
    /// Maximum number of idle connections in the connection pool.
    pub pool_max_size: u32,
    /// Connection timeout in seconds.
    pub connect_timeout_secs: u64,
    /// Overall request timeout in seconds.
    pub request_timeout_secs: u64,
    /// Optional override model name sent to the upstream.
    pub model_override: Option<String>,
    /// Mapping from client-facing model names to upstream model names.
    pub model_mapping: HashMap<String, String>,
    /// When `true`, allow falling back to direct upstream connection when
    /// all proxy nodes are unavailable.
    pub allow_direct_fallback: bool,
    /// When `true`, enable benchmark / diagnostic endpoints and verbose
    /// per-request timing.
    pub benchmark_mode: bool,
    /// Log level filter (tracing directive, e.g. "info", "debug").
    pub log_level: String,
    /// Sticky-session TTL in seconds. A client is pinned to the same proxy
    /// for this duration to maintain session consistency.
    pub sticky_ttl_secs: f64,
}

impl Config {
    /// Build a `Config` from environment variables, applying defaults for
    /// any unset values.
    pub fn from_env() -> Self {
        Self {
            port: load_env_var("PORT", 4000u16),
            bind_address: load_env_var("BIND_ADDRESS", "0.0.0.0".to_string()),
            upstream_base: load_env_var(
                "UPSTREAM_BASE",
                "https://opencode.ai/zen".to_string(),
            ),
            chat_target: load_env_var(
                "CHAT_TARGET",
                "/v1/chat/completions".to_string(),
            ),
            model_target: load_env_var("MODEL_TARGET", "/v1/models".to_string()),
            token_mode: load_env_var("PROXY_TOKEN_MODE", "adaptive".to_string()),
            token_rate: load_env_var("PROXY_TOKEN_RATE", 100.0f64),
            token_burst: load_env_var("PROXY_TOKEN_BURST", 200.0f64),
            adaptive_min_rate: load_env_var("PROXY_ADAPTIVE_MIN_RATE", 100.0f64),
            adaptive_max_rate: load_env_var("PROXY_ADAPTIVE_MAX_RATE", 5000.0f64),
            adaptive_window: load_env_var("PROXY_ADAPTIVE_WINDOW", 30u64),
            proxy_error_threshold: load_env_var("PROXY_ERROR_THRESHOLD", 5u32),
            proxy_cooldown_seconds: load_env_var("PROXY_COOLDOWN_SECONDS", 60u64),
            proxy_recovery_interval: load_env_var("PROXY_RECOVERY_INTERVAL", 30u64),
            pool_max_retries: load_env_var("POOL_MAX_RETRIES", 3u32),
            pool_max_size: load_env_var("POOL_MAX_SIZE", 128u32),
            connect_timeout_secs: load_env_var("CONNECT_TIMEOUT_SECS", 5u64),
            request_timeout_secs: load_env_var("REQUEST_TIMEOUT_SECS", 120u64),
            model_override: match env::var("MODEL_OVERRIDE") {
                Ok(v) if !v.is_empty() => Some(v),
                _ => None,
            },
            model_mapping: Self::default_model_mapping(),
            allow_direct_fallback: load_env_var("ALLOW_DIRECT_FALLBACK", false),
            benchmark_mode: load_env_var("BENCHMARK_MODE", false),
            log_level: load_env_var("LOG_LEVEL", "info".to_string()),
            sticky_ttl_secs: load_env_var("STICKY_TTL_SECS", 180.0f64),
        }
    }

    /// Re-read all environment variables, updating the config in place.
    ///
    /// This is intended to be called on SIGHUP for live reconfiguration
    /// without a full process restart.
    pub fn reload(&mut self) {
        *self = Self::from_env();
    }

    /// Parse the `token_mode` string into a `TokenMode` enum.
    ///
    /// Returns `TokenMode::Adaptive` for unrecognised values to favour a safe
    /// default that gracefully self-regulates.
    pub fn parse_token_mode(&self) -> TokenMode {
        match self.token_mode.to_lowercase().as_str() {
            "fixed" => TokenMode::Fixed,
            "adaptive" => TokenMode::Adaptive,
            "unlimited" => TokenMode::Unlimited,
            other => {
                tracing::warn!(
                    "unknown token_mode \"{}\", falling back to adaptive",
                    other
                );
                TokenMode::Adaptive
            }
        }
    }

    /// Build the default model name mapping table.
    fn default_model_mapping() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert(
            "deepseek-v4-flash".to_string(),
            "big-pickle".to_string(),
        );
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

    // -- Convenience accessors ----------------------------------------------

    /// Resolved bind socket address (`bind_address:port`).
    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.bind_address, self.port)
    }

    /// Full upstream URL for chat completions.
    pub fn chat_url(&self) -> String {
        format!("{}{}", self.upstream_base, self.chat_target)
    }

    /// Full upstream URL for the model listing endpoint.
    pub fn model_url(&self) -> String {
        format!("{}{}", self.upstream_base, self.model_target)
    }

    /// Parsed connect timeout as a `Duration`.
    pub fn connect_timeout(&self) -> Duration {
        Duration::from_secs(self.connect_timeout_secs)
    }

    /// Parsed request timeout as a `Duration`.
    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs)
    }

    /// Parsed sticky-session TTL as a `Duration`.
    pub fn load_nodes(&self) -> Vec<String> {
        let path = env::var("NODES_FILE").unwrap_or_else(|_| "/etc/zen-proxy/nodes.json".into());
        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str::<Vec<String>>(&contents) {
                Ok(nodes) => {
                    tracing::info!(count = nodes.len(), file = %path, "loaded proxy nodes");
                    nodes
                }
                Err(e) => {
                    tracing::warn!(file = %path, error = %e, "failed to parse nodes file, using empty pool");
                    Vec::new()
                }
            },
            Err(_) => {
                tracing::warn!(file = %path, "nodes file not found, using empty pool (direct-only)");
                Vec::new()
            }
        }
    }

    pub fn sticky_ttl(&self) -> Duration {
        Duration::from_secs_f64(self.sticky_ttl_secs)
    }
}

/// Read an environment variable and parse it into the requested type,
/// falling back to `default` when the variable is unset, empty, or
/// contains an unparseable value.
///
/// Parsing failures are logged as warnings so misconfigured vars degrade
/// gracefully rather than panicking.
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
        for key in &["PORT", "PROXY_TOKEN_MODE", "MODEL_OVERRIDE"] {
            env::remove_var(key);
        }

        let cfg = Config::from_env();
        assert_eq!(cfg.port, 4000);
        assert_eq!(cfg.token_mode, "adaptive");
        assert!(cfg.model_override.is_none());
        assert_eq!(cfg.allow_direct_fallback, false);
        assert_eq!(cfg.benchmark_mode, false);
        assert_eq!(cfg.log_level, "info");
    }

    #[test]
    fn from_env_reads_env_overrides() {
        unsafe { env::set_var("PORT", "8080") };
        unsafe { env::set_var("PROXY_TOKEN_MODE", "fixed") };
        unsafe { env::set_var("LOG_LEVEL", "debug") };

        let cfg = Config::from_env();
        assert_eq!(cfg.port, 8080);
        assert_eq!(cfg.token_mode, "fixed");
        assert_eq!(cfg.log_level, "debug");

        env::remove_var("PORT");
        env::remove_var("PROXY_TOKEN_MODE");
        env::remove_var("LOG_LEVEL");
    }

    #[test]
    fn from_env_graceful_on_bad_values() {
        unsafe { env::set_var("PORT", "not-a-number") };

        let cfg = Config::from_env();
        assert_eq!(cfg.port, 4000);

        env::remove_var("PORT");
    }

    #[test]
    fn parse_token_mode_variants() {
        let mut cfg = Config::from_env();

        cfg.token_mode = "fixed".into();
        assert_eq!(cfg.parse_token_mode(), TokenMode::Fixed);

        cfg.token_mode = "FIXED".into();
        assert_eq!(cfg.parse_token_mode(), TokenMode::Fixed);

        cfg.token_mode = "adaptive".into();
        assert_eq!(cfg.parse_token_mode(), TokenMode::Adaptive);

        cfg.token_mode = "unlimited".into();
        assert_eq!(cfg.parse_token_mode(), TokenMode::Unlimited);

        cfg.token_mode = "garbage".into();
        assert_eq!(cfg.parse_token_mode(), TokenMode::Adaptive);
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
        assert_eq!(cfg.bind_addr(), "0.0.0.0:4000");
        assert!(cfg.chat_url().ends_with("/v1/chat/completions"));
        assert!(cfg.model_url().ends_with("/v1/models"));
        assert_eq!(cfg.connect_timeout(), Duration::from_secs(5));
        assert_eq!(cfg.request_timeout(), Duration::from_secs(120));
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
}
