use std::time::Duration;

#[derive(Clone, Debug)]
pub struct ModelMapping {
    pub public_name: String,
    pub upstream_name: String,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub zen_chat_url: String,
    pub zen_api_key: String,
    pub require_api_key: bool,
    pub api_key: String,
    pub timeout: Duration,
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
        let model_mappings = vec![
            ModelMapping {
                public_name: "deepseek-v4-flash".into(),
                upstream_name: flash_upstream,
            },
            ModelMapping {
                public_name: "deepseek-v4-flash-lite".into(),
                upstream_name: flash_lite_upstream,
            },
        ];
        Self {
            host: std::env::var("FREE_MODEL_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
            port: std::env::var("FREE_MODEL_PORT")
                .unwrap_or_else(|_| "14118".into())
                .parse()
                .unwrap_or(14118),
            zen_chat_url: std::env::var("FREE_MODEL_ZEN_CHAT_URL").unwrap_or(newapi_chat_url),
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
