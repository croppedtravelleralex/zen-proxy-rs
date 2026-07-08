use std::time::Duration;

use crate::model_catalog::{DEFAULT_ZEN_MODELS_URL, DEFAULT_ZEN_MODELS_USER_AGENT};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelMapping {
    pub public_name: String,
    pub upstream_name: String,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub zen_chat_url: String,
    pub zen_models_url: String,
    pub zen_models_user_agent: String,
    pub zen_api_key: String,
    pub require_api_key: bool,
    pub api_key: String,
    pub timeout: Duration,
    pub auto_discover_models: bool,
    pub model_discovery_timeout: Duration,
    pub model_discovery_cache_ttl: Duration,
    pub request_body_limit_bytes: usize,
    pub true_first_token_frt: bool,
    pub claude_code_stream_initial_fetch_timeout_secs: u64,
    pub claude_code_stream_slow_guard_min_input_tokens: u64,
    pub claude_code_stream_no_forwardable_retry_secs: u64,
    pub free_models: Vec<String>,
    pub model_mappings: Vec<ModelMapping>,
}

impl Config {
    pub fn from_env() -> Self {
        let newapi_base_url = std::env::var("FREE_MODEL_NEWAPI_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8081".into());
        let newapi_chat_url = format!(
            "{}/v1/chat/completions",
            newapi_base_url.trim_end_matches('/')
        );
        let flash_upstream = std::env::var("FREE_MODEL_DEEPSEEK_V4_FLASH_UPSTREAM")
            .unwrap_or_else(|_| "deepseek-v4-flash-free".into());
        let flash_lite_upstream = std::env::var("FREE_MODEL_DEEPSEEK_V4_FLASH_LITE_UPSTREAM")
            .unwrap_or_else(|_| "big-pickle".into());
        let mimo_upstream = std::env::var("FREE_MODEL_MIMO_V2_5_UPSTREAM")
            .unwrap_or_else(|_| "mimo-v2.5-free".into());
        let north_upstream = std::env::var("FREE_MODEL_NORTH_MINI_CODE_UPSTREAM")
            .unwrap_or_else(|_| "north-mini-code-free".into());
        let nemotron_upstream = std::env::var("FREE_MODEL_NEMOTRON_3_ULTRA_UPSTREAM")
            .unwrap_or_else(|_| "nemotron-3-ultra-free".into());
        let hy3_upstream =
            std::env::var("FREE_MODEL_HY3_UPSTREAM").unwrap_or_else(|_| "hy3-free".into());
        let minimax_upstream = std::env::var("FREE_MODEL_MINIMAX_M3_UPSTREAM")
            .unwrap_or_else(|_| "minimax-m3-free".into());
        let qwen_upstream = std::env::var("FREE_MODEL_QWEN3_6_PLUS_UPSTREAM")
            .unwrap_or_else(|_| "qwen3.6-plus-free".into());
        let zen_chat_url = std::env::var("FREE_MODEL_ZEN_CHAT_URL").unwrap_or(newapi_chat_url);
        let zen_models_url = std::env::var("FREE_MODEL_ZEN_MODELS_URL").unwrap_or_else(|_| {
            derive_zen_models_url(&zen_chat_url).unwrap_or_else(|| DEFAULT_ZEN_MODELS_URL.into())
        });
        let model_mappings = vec![
            ModelMapping {
                public_name: "deepseek-v4-flash".into(),
                upstream_name: flash_upstream,
            },
            ModelMapping {
                public_name: "big-pickle".into(),
                upstream_name: flash_lite_upstream,
            },
            ModelMapping {
                public_name: "mimo-v2.5".into(),
                upstream_name: mimo_upstream,
            },
            ModelMapping {
                public_name: "north-mini-code".into(),
                upstream_name: north_upstream,
            },
            ModelMapping {
                public_name: "nemotron-3-ultra".into(),
                upstream_name: nemotron_upstream,
            },
            ModelMapping {
                public_name: "hy3".into(),
                upstream_name: hy3_upstream,
            },
            ModelMapping {
                public_name: "minimax-m3".into(),
                upstream_name: minimax_upstream,
            },
            ModelMapping {
                public_name: "qwen3.6-plus".into(),
                upstream_name: qwen_upstream,
            },
        ];
        Self {
            host: std::env::var("FREE_MODEL_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
            port: std::env::var("FREE_MODEL_PORT")
                .unwrap_or_else(|_| "14118".into())
                .parse()
                .unwrap_or(14118),
            zen_chat_url,
            zen_models_url,
            zen_models_user_agent: std::env::var("FREE_MODEL_ZEN_MODELS_USER_AGENT")
                .unwrap_or_else(|_| DEFAULT_ZEN_MODELS_USER_AGENT.into()),
            zen_api_key: std::env::var("FREE_MODEL_ZEN_API_KEY")
                .or_else(|_| std::env::var("FREE_MODEL_NEWAPI_KEY"))
                .unwrap_or_else(|_| "sk-dev".into()),
            require_api_key: std::env::var("FREE_MODEL_REQUIRE_API_KEY")
                .map(|v| v != "0")
                .unwrap_or(true),
            api_key: std::env::var("FREE_MODEL_API_KEY").unwrap_or_else(|_| "sk-dev".into()),
            timeout: Duration::from_millis(
                std::env::var("FREE_MODEL_TIMEOUT_MS")
                    .unwrap_or_else(|_| "120000".into())
                    .parse()
                    .unwrap_or(120_000),
            ),
            auto_discover_models: env_flag("FREE_MODEL_AUTO_DISCOVER_MODELS", true),
            model_discovery_timeout: Duration::from_millis(env_u64(
                "FREE_MODEL_MODEL_DISCOVERY_TIMEOUT_MS",
                10_000,
            )),
            model_discovery_cache_ttl: Duration::from_secs(env_u64(
                "FREE_MODEL_MODEL_DISCOVERY_CACHE_TTL_SECS",
                300,
            )),
            request_body_limit_bytes: std::env::var("FREE_MODEL_REQUEST_BODY_LIMIT_MB")
                .unwrap_or_else(|_| "64".into())
                .parse::<usize>()
                .unwrap_or(64)
                .max(1)
                * 1024
                * 1024,
            true_first_token_frt: env_flag("FREE_MODEL_TRUE_FIRST_TOKEN_FRT", true),
            claude_code_stream_initial_fetch_timeout_secs: env_u64(
                "FREE_MODEL_CLAUDE_CODE_STREAM_INITIAL_FETCH_TIMEOUT_SECS",
                30,
            ),
            claude_code_stream_slow_guard_min_input_tokens: env_u64(
                "FREE_MODEL_CLAUDE_CODE_STREAM_SLOW_GUARD_MIN_INPUT_TOKENS",
                150_000,
            ),
            claude_code_stream_no_forwardable_retry_secs: env_u64(
                "FREE_MODEL_CLAUDE_CODE_STREAM_NO_FORWARDABLE_RETRY_SECS",
                45,
            )
            .max(1),
            free_models: model_mappings
                .iter()
                .map(|mapping| mapping.public_name.clone())
                .collect(),
            model_mappings,
        }
    }
}

fn derive_zen_models_url(zen_chat_url: &str) -> Option<String> {
    let url = zen_chat_url.trim().trim_end_matches('/');
    if !url.contains("opencode.ai/zen") {
        return None;
    }
    url.strip_suffix("/v1/chat/completions")
        .map(|base| format!("{base}/v1/models"))
        .or_else(|| {
            url.strip_suffix("/chat/completions")
                .map(|base| format!("{base}/models"))
        })
}

fn env_flag(name: &str, default: bool) -> bool {
    std::env::var(name)
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_static_mappings_include_hy3_fallback() {
        let config = Config::from_env();

        assert!(config
            .model_mappings
            .iter()
            .any(|mapping| mapping.public_name == "hy3"));
        assert!(config.free_models.iter().any(|model| model == "hy3"));
    }

    #[test]
    fn derives_models_url_only_for_opencode_zen_chat_url() {
        assert_eq!(
            derive_zen_models_url("https://opencode.ai/zen/v1/chat/completions").as_deref(),
            Some("https://opencode.ai/zen/v1/models")
        );
        assert_eq!(
            derive_zen_models_url("http://127.0.0.1:3000/v1/chat/completions"),
            None
        );
    }
}
