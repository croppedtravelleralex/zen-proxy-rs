use serde::{Deserialize, Serialize};

use crate::config::DynamicModelPublicMode;
use crate::v4::model_discovery::{DiscoveredModel, DiscoveredModelState, ModelDiscoverySnapshot};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub upstream_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelResolution {
    pub public_model: String,
    pub upstream_model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    UnknownModel(String),
}

pub trait ModelRegistry: Send + Sync {
    fn public_models(&self) -> Vec<ModelInfo>;
    fn resolve(&self, public_model: &str) -> Result<ModelResolution, ModelError>;
}

#[derive(Debug, Default)]
pub struct StaticModelRegistry;

impl StaticModelRegistry {
    const MODELS: &'static [(&'static str, &'static str)] = &[
        ("deepseek-v4-flash", "deepseek-v4-flash-free"),
        ("deepseek-v4-flash-lite", "big-pickle"),
    ];
}

impl ModelRegistry for StaticModelRegistry {
    fn public_models(&self) -> Vec<ModelInfo> {
        Self::MODELS
            .iter()
            .map(|(public, upstream)| ModelInfo {
                id: (*public).to_string(),
                upstream_id: (*upstream).to_string(),
            })
            .collect()
    }

    fn resolve(&self, public_model: &str) -> Result<ModelResolution, ModelError> {
        Self::MODELS
            .iter()
            .find(|(public, _)| *public == public_model)
            .map(|(public, upstream)| ModelResolution {
                public_model: (*public).to_string(),
                upstream_model: (*upstream).to_string(),
            })
            .ok_or_else(|| ModelError::UnknownModel(public_model.to_string()))
    }
}

#[derive(Debug, Clone)]
pub struct EffectiveModelRegistry {
    public_mode: DynamicModelPublicMode,
    discovery: ModelDiscoverySnapshot,
}

impl EffectiveModelRegistry {
    pub fn new(public_mode: DynamicModelPublicMode, discovery: ModelDiscoverySnapshot) -> Self {
        Self {
            public_mode,
            discovery,
        }
    }

    pub fn is_dynamic_public(&self, model: &DiscoveredModel) -> bool {
        match self.public_mode {
            DynamicModelPublicMode::StaticOnly => false,
            DynamicModelPublicMode::CandidateCanaryOrActive => {
                matches!(
                    model.state,
                    DiscoveredModelState::Candidate
                        | DiscoveredModelState::Canary
                        | DiscoveredModelState::Active
                )
            }
            DynamicModelPublicMode::CanaryOrActive => {
                matches!(
                    model.state,
                    DiscoveredModelState::Canary | DiscoveredModelState::Active
                )
            }
            DynamicModelPublicMode::ActiveOnly => {
                matches!(model.state, DiscoveredModelState::Active)
            }
        }
    }

    fn public_dynamic_models(&self) -> impl Iterator<Item = &DiscoveredModel> {
        self.discovery
            .models
            .iter()
            .filter(|model| self.is_dynamic_public(model))
    }
}

impl ModelRegistry for EffectiveModelRegistry {
    fn public_models(&self) -> Vec<ModelInfo> {
        let mut models = StaticModelRegistry.public_models();
        for dynamic in self.public_dynamic_models() {
            if models.iter().any(|model| model.id == dynamic.id) {
                continue;
            }
            models.push(ModelInfo {
                id: dynamic.id.clone(),
                upstream_id: dynamic.upstream_id.clone(),
            });
        }
        models
    }

    fn resolve(&self, public_model: &str) -> Result<ModelResolution, ModelError> {
        if let Ok(static_model) = StaticModelRegistry.resolve(public_model) {
            return Ok(static_model);
        }
        self.public_dynamic_models()
            .find(|model| model.id == public_model)
            .map(|model| ModelResolution {
                public_model: model.id.clone(),
                upstream_model: model.upstream_id.clone(),
            })
            .ok_or_else(|| ModelError::UnknownModel(public_model.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v4::model_discovery::DynamicModelRegistry;

    #[test]
    fn exposes_only_two_public_models() {
        let registry = StaticModelRegistry;
        let ids: Vec<String> = registry
            .public_models()
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert_eq!(ids, vec!["deepseek-v4-flash", "deepseek-v4-flash-lite"]);
    }

    #[test]
    fn resolves_public_models_to_v4_upstreams() {
        let registry = StaticModelRegistry;
        assert_eq!(
            registry
                .resolve("deepseek-v4-flash")
                .unwrap()
                .upstream_model,
            "deepseek-v4-flash-free"
        );
        assert_eq!(
            registry
                .resolve("deepseek-v4-flash-lite")
                .unwrap()
                .upstream_model,
            "big-pickle"
        );
    }

    #[test]
    fn rejects_unknown_models() {
        let registry = StaticModelRegistry;
        assert!(matches!(
            registry.resolve("deepseek-v4-pro"),
            Err(ModelError::UnknownModel(model)) if model == "deepseek-v4-pro"
        ));
    }

    fn discovered_registry_with_states() -> ModelDiscoverySnapshot {
        let registry = DynamicModelRegistry::new(true, "url".into());
        registry
            .update_from_opencode_json(
                r#"{"data":[{"id":"new-canary-free"},{"id":"new-active-free"},{"id":"new-candidate-free"},{"id":"paid-model"}]}"#,
            )
            .unwrap();
        registry.set_model_state(
            "new-canary-free",
            DiscoveredModelState::Canary,
            "test canary quorum",
        );
        registry.set_model_state(
            "new-active-free",
            DiscoveredModelState::Active,
            "test active quorum",
        );
        registry.snapshot()
    }

    #[test]
    fn effective_registry_defaults_to_static_only() {
        let registry = EffectiveModelRegistry::new(
            DynamicModelPublicMode::StaticOnly,
            discovered_registry_with_states(),
        );
        let ids: Vec<String> = registry
            .public_models()
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert_eq!(ids, vec!["deepseek-v4-flash", "deepseek-v4-flash-lite"]);
        assert!(matches!(
            registry.resolve("new-active-free"),
            Err(ModelError::UnknownModel(model)) if model == "new-active-free"
        ));
    }

    #[test]
    fn effective_registry_exposes_canary_and_active_only_when_configured() {
        let registry = EffectiveModelRegistry::new(
            DynamicModelPublicMode::CanaryOrActive,
            discovered_registry_with_states(),
        );
        let ids: Vec<String> = registry
            .public_models()
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert_eq!(
            ids,
            vec![
                "deepseek-v4-flash",
                "deepseek-v4-flash-lite",
                "new-active-free",
                "new-canary-free"
            ]
        );
        assert_eq!(
            registry.resolve("new-canary-free").unwrap().upstream_model,
            "new-canary-free"
        );
        assert!(matches!(
            registry.resolve("new-candidate-free"),
            Err(ModelError::UnknownModel(model)) if model == "new-candidate-free"
        ));
    }

    #[test]
    fn effective_registry_can_expose_candidates_for_isolated_test_channels() {
        let registry = EffectiveModelRegistry::new(
            DynamicModelPublicMode::CandidateCanaryOrActive,
            discovered_registry_with_states(),
        );
        let ids: Vec<String> = registry
            .public_models()
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert_eq!(
            ids,
            vec![
                "deepseek-v4-flash",
                "deepseek-v4-flash-lite",
                "new-active-free",
                "new-canary-free",
                "new-candidate-free"
            ]
        );
        assert!(registry.resolve("new-candidate-free").is_ok());
        assert!(registry.resolve("paid-model").is_err());
    }

    #[test]
    fn effective_registry_active_only_excludes_canary() {
        let registry = EffectiveModelRegistry::new(
            DynamicModelPublicMode::ActiveOnly,
            discovered_registry_with_states(),
        );
        let ids: Vec<String> = registry
            .public_models()
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert_eq!(
            ids,
            vec![
                "deepseek-v4-flash",
                "deepseek-v4-flash-lite",
                "new-active-free"
            ]
        );
        assert!(registry.resolve("new-active-free").is_ok());
        assert!(registry.resolve("new-canary-free").is_err());
    }
}
