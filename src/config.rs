use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub zen_chat_url: String,
    pub zen_api_key: String,
    pub require_api_key: bool,
    pub api_key: String,
    pub timeout: Duration,
    pub free_models: Vec<String>,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            host: std::env::var("FREE_MODEL_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
            port: std::env::var("FREE_MODEL_PORT")
                .unwrap_or_else(|_| "14118".into())
                .parse()
                .unwrap_or(14118),
            zen_chat_url: std::env::var("FREE_MODEL_ZEN_CHAT_URL")
                .unwrap_or_else(|_| "https://opencode.ai/zen/v1/chat/completions".into()),
            zen_api_key: std::env::var("FREE_MODEL_ZEN_API_KEY")
                .unwrap_or_else(|_| "public".into()),
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
            free_models: vec![
                "big-pickle".into(),
                "deepseek-v4-flash-free".into(),
                "nemotron-3-super-free".into(),
                "qwen3.6-plus-free".into(),
            ],
        }
    }
}
