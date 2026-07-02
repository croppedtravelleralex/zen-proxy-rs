use std::time::Duration;

use free_model_client_rs::kernel::{FreeModelKernel, KernelConfig};
use free_model_client_rs::protocol::types::{ChatRequest, Message};
use serde_json::Value;

use crate::pool::*;

pub struct ProbePeriod;

impl ProbePeriod {
    pub async fn probe_node(
        client: &reqwest::Client,
        _node: &NodeRef,
        upstream_base: &str,
        timeout_secs: u64,
        api_key: &str,
    ) -> bool {
        let zen_chat_url = format!(
            "{}/v1/chat/completions",
            upstream_base.trim_end_matches('/')
        );
        let kernel = FreeModelKernel::new(KernelConfig {
            zen_chat_url,
            zen_api_key: api_key.to_string(),
            extra_headers: vec![("x-zen-proxy-probe".to_string(), "dead".to_string())],
            model_mappings: Vec::new(),
            true_first_token_frt: true,
            claude_code_stream_initial_fetch_timeout_secs: 30,
            claude_code_stream_slow_guard_min_input_tokens: 150_000,
            claude_code_stream_no_forwardable_retry_secs: 45,
        });
        for i in 0..3 {
            if i > 0 {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            let request = ChatRequest {
                model: "deepseek-v4-flash-free".to_string(),
                messages: vec![Message {
                    role: "user".to_string(),
                    content: Value::String("Reply exactly: OK".to_string()),
                    tool_calls: None,
                    tool_call_id: None,
                }],
                stream: Some(false),
                max_tokens: Some(32),
                temperature: None,
                top_p: None,
                tools: None,
                tool_choice: None,
            };

            let result = tokio::time::timeout(
                Duration::from_secs(timeout_secs),
                kernel.openai_chat(client, request),
            )
            .await;

            match result {
                Ok(Ok(resp)) => {
                    if resp.status().is_success() {
                        return true;
                    }
                }
                _ => continue,
            }
        }

        false
    }
}
