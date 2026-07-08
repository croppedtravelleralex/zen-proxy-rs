use axum::response::Response;
use reqwest::Client;

use crate::client_profile::ClientProfile;
use crate::config::{Config, ModelMapping};
use crate::error::AppError;
use crate::protocol::types::{AnthropicRequest, ChatRequest};

#[derive(Clone, Debug)]
pub struct KernelConfig {
    pub zen_chat_url: String,
    pub zen_api_key: String,
    pub extra_headers: Vec<(String, String)>,
    pub model_mappings: Vec<(String, String)>,
    pub true_first_token_frt: bool,
    pub claude_code_stream_initial_fetch_timeout_secs: u64,
    pub claude_code_stream_slow_guard_min_input_tokens: u64,
    pub claude_code_stream_no_forwardable_retry_secs: u64,
}

impl From<&Config> for KernelConfig {
    fn from(config: &Config) -> Self {
        Self::from_config_and_mappings(config, &config.model_mappings)
    }
}

impl KernelConfig {
    pub fn from_config_and_mappings(config: &Config, mappings: &[ModelMapping]) -> Self {
        Self {
            zen_chat_url: config.zen_chat_url.clone(),
            zen_api_key: config.zen_api_key.clone(),
            extra_headers: Vec::new(),
            model_mappings: mappings
                .iter()
                .map(|mapping| (mapping.public_name.clone(), mapping.upstream_name.clone()))
                .collect(),
            true_first_token_frt: config.true_first_token_frt,
            claude_code_stream_initial_fetch_timeout_secs: config
                .claude_code_stream_initial_fetch_timeout_secs,
            claude_code_stream_slow_guard_min_input_tokens: config
                .claude_code_stream_slow_guard_min_input_tokens,
            claude_code_stream_no_forwardable_retry_secs: config
                .claude_code_stream_no_forwardable_retry_secs,
        }
    }
}

#[derive(Clone, Debug)]
pub struct FreeModelKernel {
    config: KernelConfig,
}

impl FreeModelKernel {
    pub fn new(config: KernelConfig) -> Self {
        Self { config }
    }

    pub fn from_config(config: &Config) -> Self {
        Self::new(KernelConfig::from(config))
    }

    pub fn from_config_and_mappings(config: &Config, mappings: &[ModelMapping]) -> Self {
        Self::new(KernelConfig::from_config_and_mappings(config, mappings))
    }

    pub async fn openai_chat(
        &self,
        client: &Client,
        request: ChatRequest,
    ) -> Result<Response, AppError> {
        self.openai_chat_with_profile(client, request, ClientProfile::unknown())
            .await
    }

    pub async fn openai_chat_with_profile(
        &self,
        client: &Client,
        request: ChatRequest,
        profile: ClientProfile,
    ) -> Result<Response, AppError> {
        crate::proxy::openai::handle_openai_chat(client, &self.config, request, profile).await
    }

    pub async fn anthropic_messages(
        &self,
        client: &Client,
        request: AnthropicRequest,
    ) -> Result<Response, AppError> {
        self.anthropic_messages_with_profile(client, request, ClientProfile::unknown())
            .await
    }

    pub async fn anthropic_messages_with_profile(
        &self,
        client: &Client,
        request: AnthropicRequest,
        profile: ClientProfile,
    ) -> Result<Response, AppError> {
        crate::proxy::anthropic::handle_anthropic_messages(client, &self.config, request, profile)
            .await
    }
}
