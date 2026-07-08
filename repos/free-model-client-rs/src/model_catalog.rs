use crate::config::{Config, ModelMapping};
use anyhow::{anyhow, Context};
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashSet;

pub const DEFAULT_ZEN_MODELS_URL: &str = "https://opencode.ai/zen/v1/models";
pub const DEFAULT_ZEN_MODELS_USER_AGENT: &str =
    "opencode/1.15.5 ai-sdk/provider-utils/4.0.23 runtime/bun/1.3.14";

#[derive(Debug, Deserialize)]
struct OpenAiModelsResponse {
    data: Vec<OpenAiModelEntry>,
}

#[derive(Debug, Deserialize)]
struct OpenAiModelEntry {
    id: String,
}

pub async fn discover_model_mappings(
    client: &Client,
    config: &Config,
) -> anyhow::Result<Vec<ModelMapping>> {
    let response = client
        .get(&config.zen_models_url)
        .header(reqwest::header::USER_AGENT, &config.zen_models_user_agent)
        .timeout(config.model_discovery_timeout)
        .send()
        .await
        .with_context(|| format!("requesting {}", config.zen_models_url))?
        .error_for_status()
        .with_context(|| format!("non-success status from {}", config.zen_models_url))?;
    let body = response
        .text()
        .await
        .context("reading Zen model catalog response body")?;
    let mappings = parse_zen_model_mappings(&body)?;
    if mappings.is_empty() {
        return Err(anyhow!("Zen model catalog returned no free models"));
    }
    Ok(mappings)
}

pub fn parse_zen_model_mappings(body: &str) -> anyhow::Result<Vec<ModelMapping>> {
    let payload: OpenAiModelsResponse =
        serde_json::from_str(body).context("parsing OpenAI-compatible model catalog")?;
    Ok(payload
        .data
        .into_iter()
        .filter_map(|entry| model_mapping_from_upstream_id(&entry.id))
        .collect())
}

pub fn merge_model_mappings(
    base: &[ModelMapping],
    discovered: &[ModelMapping],
) -> Vec<ModelMapping> {
    let mut seen_public = HashSet::<String>::new();
    let mut merged = Vec::with_capacity(base.len() + discovered.len());

    for mapping in base.iter().chain(discovered.iter()) {
        if mapping.public_name.trim().is_empty() || mapping.upstream_name.trim().is_empty() {
            continue;
        }
        let public_name = mapping.public_name.trim().to_ascii_lowercase();
        if seen_public.insert(public_name.clone()) {
            merged.push(ModelMapping {
                public_name,
                upstream_name: mapping.upstream_name.trim().to_string(),
            });
        }
    }

    merged
}

pub fn model_mapping_from_upstream_id(id: &str) -> Option<ModelMapping> {
    let upstream_name = normalize_upstream_model_id(id)?;
    if !is_free_zen_model_id(&upstream_name) {
        return None;
    }
    Some(ModelMapping {
        public_name: public_model_name(&upstream_name),
        upstream_name,
    })
}

pub fn public_model_name(upstream_name: &str) -> String {
    upstream_name
        .strip_suffix("-free")
        .unwrap_or(upstream_name)
        .to_ascii_lowercase()
}

fn normalize_upstream_model_id(id: &str) -> Option<String> {
    let normalized = id.trim().strip_prefix("opencode/").unwrap_or(id.trim());
    if normalized.is_empty() {
        return None;
    }
    Some(normalized.to_ascii_lowercase())
}

fn is_free_zen_model_id(id: &str) -> bool {
    id.ends_with("-free") || id == "big-pickle"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_opencode_zen_free_models() {
        let body = serde_json::json!({
            "object": "list",
            "data": [
                {"id": "gpt-5-nano", "object": "model", "owned_by": "opencode"},
                {"id": "big-pickle", "object": "model", "owned_by": "opencode"},
                {"id": "deepseek-v4-flash-free", "object": "model", "owned_by": "opencode"},
                {"id": "mimo-v2.5-free", "object": "model", "owned_by": "opencode"},
                {"id": "hy3-free", "object": "model", "owned_by": "opencode"},
                {"id": "nemotron-3-ultra-free", "object": "model", "owned_by": "opencode"},
                {"id": "north-mini-code-free", "object": "model", "owned_by": "opencode"},
                {"id": "paid-model", "object": "model", "owned_by": "opencode"}
            ]
        })
        .to_string();

        let mappings = parse_zen_model_mappings(&body).unwrap();

        assert!(mappings.contains(&ModelMapping {
            public_name: "hy3".to_string(),
            upstream_name: "hy3-free".to_string(),
        }));
        assert!(mappings.contains(&ModelMapping {
            public_name: "deepseek-v4-flash".to_string(),
            upstream_name: "deepseek-v4-flash-free".to_string(),
        }));
        assert!(mappings.contains(&ModelMapping {
            public_name: "mimo-v2.5".to_string(),
            upstream_name: "mimo-v2.5-free".to_string(),
        }));
        assert!(mappings.contains(&ModelMapping {
            public_name: "big-pickle".to_string(),
            upstream_name: "big-pickle".to_string(),
        }));
        assert!(!mappings
            .iter()
            .any(|mapping| mapping.public_name == "gpt-5-nano"));
        assert!(!mappings
            .iter()
            .any(|mapping| mapping.public_name == "paid-model"));
    }

    #[test]
    fn merge_keeps_explicit_static_mapping_precedence() {
        let static_mappings = vec![ModelMapping {
            public_name: "hy3".to_string(),
            upstream_name: "custom-hy3-free".to_string(),
        }];
        let discovered = vec![
            ModelMapping {
                public_name: "hy3".to_string(),
                upstream_name: "hy3-free".to_string(),
            },
            ModelMapping {
                public_name: "north-mini-code".to_string(),
                upstream_name: "north-mini-code-free".to_string(),
            },
        ];

        let merged = merge_model_mappings(&static_mappings, &discovered);

        assert_eq!(
            merged,
            vec![
                ModelMapping {
                    public_name: "hy3".to_string(),
                    upstream_name: "custom-hy3-free".to_string(),
                },
                ModelMapping {
                    public_name: "north-mini-code".to_string(),
                    upstream_name: "north-mini-code-free".to_string(),
                }
            ]
        );
    }
}
