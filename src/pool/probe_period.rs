use std::time::Duration;

use crate::pool::*;

pub struct ProbePeriod;

impl ProbePeriod {
    pub async fn probe_node(
        client: &reqwest::Client,
        _node: &NodeRef,
        upstream_base: &str,
        timeout_secs: u64,
        _api_key: &str,
    ) -> bool {
        let probe_url = format!(
            "{}/v1/chat/completions",
            upstream_base.trim_end_matches('/')
        );
        let body = serde_json::json!({
            "model": "big-pickle",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": false,
        });

        for i in 0..3 {
            if i > 0 {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }

            let result = tokio::time::timeout(
                Duration::from_secs(timeout_secs),
                client.post(&probe_url).json(&body).send(),
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
