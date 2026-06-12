use serde::{Deserialize, Serialize};

use crate::config::DynamicModelPublicMode;
use crate::v4::model_discovery::{DiscoveredModel, DiscoveredModelState, ModelDiscoverySnapshot};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub upstream_id: String,
    pub compatibility_profile: ModelCompatibilityProfile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelResolution {
    pub public_model: String,
    pub upstream_model: String,
    pub compatibility_profile: ModelCompatibilityProfile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    UnknownModel(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCompatibilityProfile {
    StaticFlash,
    StaticFlashLite,
    DynamicGeneric,
    DynamicClaudeCodeCompatible,
    DynamicRestricted,
}

impl ModelCompatibilityProfile {
    pub fn for_static(public_model: &str) -> Option<Self> {
        match public_model {
            "deepseek-v4-flash" => Some(Self::StaticFlash),
            "deepseek-v4-flash-lite" => Some(Self::StaticFlashLite),
            _ => None,
        }
    }

    pub fn for_dynamic(model: &DiscoveredModel) -> Self {
        match model.state {
            DiscoveredModelState::Quarantined | DiscoveredModelState::Retired => {
                Self::DynamicRestricted
            }
            _ if model.claudecode_compatible => Self::DynamicClaudeCodeCompatible,
            _ => Self::DynamicGeneric,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::StaticFlash => "static_flash",
            Self::StaticFlashLite => "static_flash_lite",
            Self::DynamicGeneric => "dynamic_generic",
            Self::DynamicClaudeCodeCompatible => "dynamic_claudecode_compatible",
            Self::DynamicRestricted => "dynamic_restricted",
        }
    }
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

    fn is_reserved_public_or_upstream(model_id: &str) -> bool {
        Self::MODELS
            .iter()
            .any(|(public, upstream)| *public == model_id || *upstream == model_id)
    }
}

impl ModelRegistry for StaticModelRegistry {
    fn public_models(&self) -> Vec<ModelInfo> {
        Self::MODELS
            .iter()
            .map(|(public, upstream)| ModelInfo {
                id: (*public).to_string(),
                upstream_id: (*upstream).to_string(),
                compatibility_profile: ModelCompatibilityProfile::for_static(public)
                    .expect("static model must have a compatibility profile"),
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
                compatibility_profile: ModelCompatibilityProfile::for_static(public)
                    .expect("static model must have a compatibility profile"),
            })
            .ok_or_else(|| ModelError::UnknownModel(public_model.to_string()))
    }
}

#[derive(Debug, Clone)]
pub struct EffectiveModelRegistry {
    public_mode: DynamicModelPublicMode,
    discovery: ModelDiscoverySnapshot,
    dynamic_public_allowlist: Vec<String>,
}

impl EffectiveModelRegistry {
    pub fn new(public_mode: DynamicModelPublicMode, discovery: ModelDiscoverySnapshot) -> Self {
        Self::with_dynamic_public_allowlist(public_mode, discovery, Vec::new())
    }

    pub fn with_dynamic_public_allowlist(
        public_mode: DynamicModelPublicMode,
        discovery: ModelDiscoverySnapshot,
        dynamic_public_allowlist: Vec<String>,
    ) -> Self {
        Self {
            public_mode,
            discovery,
            dynamic_public_allowlist: dedupe_allowlist(dynamic_public_allowlist),
        }
    }

    pub fn is_dynamic_public(&self, model: &DiscoveredModel) -> bool {
        let Some(public_alias) = dynamic_public_alias(&model.id) else {
            return false;
        };
        if StaticModelRegistry::is_reserved_public_or_upstream(&model.id)
            || StaticModelRegistry::is_reserved_public_or_upstream(&model.upstream_id)
        {
            return false;
        }
        let mode_allows = match self.public_mode {
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
        };
        mode_allows && self.dynamic_public_allowlist_allows(model, &public_alias)
    }

    fn dynamic_public_allowlist_allows(&self, model: &DiscoveredModel, public_alias: &str) -> bool {
        self.dynamic_public_allowlist.is_empty()
            || self.dynamic_public_allowlist.iter().any(|allowed| {
                allowed == public_alias
                    || allowed == &model.id
                    || allowed == &model.upstream_id
                    || dynamic_public_alias(allowed).as_deref() == Some(public_alias)
            })
    }

    fn public_dynamic_models(&self) -> impl Iterator<Item = &DiscoveredModel> {
        self.discovery
            .models
            .iter()
            .filter(|model| self.is_dynamic_public(model))
    }

    fn resolve_dynamic_model(&self, public_model: &str) -> Option<&DiscoveredModel> {
        self.public_dynamic_models()
            .find(|model| dynamic_public_alias(&model.id).as_deref() == Some(public_model))
    }
}

impl ModelRegistry for EffectiveModelRegistry {
    fn public_models(&self) -> Vec<ModelInfo> {
        let mut models = StaticModelRegistry.public_models();
        for dynamic in self.public_dynamic_models() {
            let Some(public_id) = dynamic_public_alias(&dynamic.id) else {
                continue;
            };
            if models.iter().any(|model| model.id == public_id) {
                continue;
            }
            models.push(ModelInfo {
                id: public_id,
                upstream_id: dynamic.upstream_id.clone(),
                compatibility_profile: ModelCompatibilityProfile::for_dynamic(dynamic),
            });
        }
        models
    }

    fn resolve(&self, public_model: &str) -> Result<ModelResolution, ModelError> {
        if let Ok(static_model) = StaticModelRegistry.resolve(public_model) {
            return Ok(static_model);
        }
        self.resolve_dynamic_model(public_model)
            .map(|model| ModelResolution {
                public_model: dynamic_public_alias(&model.id)
                    .expect("public dynamic model must have a sanitized alias"),
                upstream_model: model.upstream_id.clone(),
                compatibility_profile: ModelCompatibilityProfile::for_dynamic(model),
            })
            .ok_or_else(|| ModelError::UnknownModel(public_model.to_string()))
    }
}

fn dynamic_public_alias(upstream_id: &str) -> Option<String> {
    upstream_id
        .strip_suffix("-free")
        .filter(|alias| !alias.is_empty())
        .map(str::to_string)
}

fn dedupe_allowlist(items: Vec<String>) -> Vec<String> {
    let mut deduped = Vec::new();
    for item in items.into_iter().map(|item| item.trim().to_string()) {
        if !item.is_empty() && !deduped.contains(&item) {
            deduped.push(item);
        }
    }
    deduped
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
                .resolve("deepseek-v4-flash")
                .unwrap()
                .compatibility_profile,
            ModelCompatibilityProfile::StaticFlash
        );
        assert_eq!(
            registry
                .resolve("deepseek-v4-flash-lite")
                .unwrap()
                .upstream_model,
            "big-pickle"
        );
        assert_eq!(
            registry
                .resolve("deepseek-v4-flash-lite")
                .unwrap()
                .compatibility_profile,
            ModelCompatibilityProfile::StaticFlashLite
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
                "new-active",
                "new-canary"
            ]
        );
        assert_eq!(
            registry.resolve("new-canary").unwrap().upstream_model,
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
                "new-active",
                "new-canary",
                "new-candidate"
            ]
        );
        assert!(registry.resolve("new-candidate").is_ok());
        assert_eq!(
            registry
                .resolve("new-candidate")
                .unwrap()
                .compatibility_profile,
            ModelCompatibilityProfile::DynamicGeneric
        );
        assert!(registry.resolve("paid-model").is_err());
    }

    #[test]
    fn effective_registry_exposes_earned_claudecode_profile_only_after_mark() {
        let discovery = DynamicModelRegistry::new(true, "url".into());
        discovery
            .update_from_opencode_json(r#"{"data":[{"id":"new-cc-free"}]}"#)
            .unwrap();
        discovery
            .set_model_state(
                "new-cc-free",
                DiscoveredModelState::Canary,
                "probe matrix passed",
            )
            .unwrap();
        let generic = EffectiveModelRegistry::new(
            DynamicModelPublicMode::CanaryOrActive,
            discovery.snapshot(),
        );
        assert_eq!(
            generic.resolve("new-cc").unwrap().compatibility_profile,
            ModelCompatibilityProfile::DynamicGeneric
        );

        discovery
            .mark_claudecode_compatible("new-cc-free", "http_bounded probe matrix passed")
            .unwrap();
        let compatible = EffectiveModelRegistry::new(
            DynamicModelPublicMode::CanaryOrActive,
            discovery.snapshot(),
        );
        assert_eq!(
            compatible.resolve("new-cc").unwrap().compatibility_profile,
            ModelCompatibilityProfile::DynamicClaudeCodeCompatible
        );
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
            vec!["deepseek-v4-flash", "deepseek-v4-flash-lite", "new-active"]
        );
        assert!(registry.resolve("new-active").is_ok());
        assert!(registry.resolve("new-canary-free").is_err());
    }

    #[test]
    fn effective_registry_desensitizes_free_suffix_and_deduplicates_static_upstreams() {
        let discovery = DynamicModelRegistry::new(true, "url".into());
        discovery
            .update_from_opencode_json(
                r#"{"data":[{"id":"deepseek-v4-flash-free"},{"id":"big-pickle"},{"id":"mimo-v2.5-free"},{"id":"paid-model"}]}"#,
            )
            .unwrap();
        let registry = EffectiveModelRegistry::new(
            DynamicModelPublicMode::CandidateCanaryOrActive,
            discovery.snapshot(),
        );
        let models = registry.public_models();
        let ids: Vec<String> = models.iter().map(|model| model.id.clone()).collect();
        assert_eq!(
            ids,
            vec!["deepseek-v4-flash", "deepseek-v4-flash-lite", "mimo-v2.5"]
        );
        assert_eq!(
            registry.resolve("mimo-v2.5").unwrap().upstream_model,
            "mimo-v2.5-free"
        );
        assert!(registry.resolve("mimo-v2.5-free").is_err());
        assert!(registry.resolve("deepseek-v4-flash-free").is_err());
        assert!(registry.resolve("big-pickle").is_err());
    }

    #[test]
    fn effective_registry_dynamic_allowlist_filters_public_models_and_resolve() {
        let discovery = DynamicModelRegistry::new(true, "url".into());
        discovery
            .update_from_opencode_json(
                r#"{"data":[{"id":"mimo-v2.5-free"},{"id":"nemotron-3-ultra-free"},{"id":"north-mini-code-free"}]}"#,
            )
            .unwrap();
        let registry = EffectiveModelRegistry::with_dynamic_public_allowlist(
            DynamicModelPublicMode::CandidateCanaryOrActive,
            discovery.snapshot(),
            vec!["mimo-v2.5".into(), "nemotron-3-ultra-free".into()],
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
                "mimo-v2.5",
                "nemotron-3-ultra"
            ]
        );
        assert_eq!(
            registry.resolve("nemotron-3-ultra").unwrap().upstream_model,
            "nemotron-3-ultra-free"
        );
        assert!(registry.resolve("north-mini-code").is_err());
    }

    #[test]
    fn effective_registry_empty_dynamic_allowlist_preserves_existing_behavior() {
        let discovery = DynamicModelRegistry::new(true, "url".into());
        discovery
            .update_from_opencode_json(
                r#"{"data":[{"id":"mimo-v2.5-free"},{"id":"nemotron-3-ultra-free"}]}"#,
            )
            .unwrap();
        let registry = EffectiveModelRegistry::with_dynamic_public_allowlist(
            DynamicModelPublicMode::CandidateCanaryOrActive,
            discovery.snapshot(),
            Vec::new(),
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
                "mimo-v2.5",
                "nemotron-3-ultra"
            ]
        );
    }
}
