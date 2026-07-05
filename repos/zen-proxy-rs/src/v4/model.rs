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
    StaticMimo,
    DynamicGeneric,
    DynamicClaudeCodeCompatible,
    DynamicRestricted,
}

impl ModelCompatibilityProfile {
    pub fn for_static(public_model: &str) -> Option<Self> {
        match public_model {
            "deepseek" => Some(Self::StaticFlash),
            "deepseek-v4-flash" => Some(Self::StaticFlash),
            "bigpickle" => Some(Self::StaticFlashLite),
            "big-pickle" => Some(Self::StaticFlashLite),
            "mimo" => Some(Self::StaticMimo),
            "mimo-v2.5" => Some(Self::StaticMimo),
            "claude-haiku-4-5" => Some(Self::StaticFlash),
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
            Self::StaticMimo => "static_mimo",
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
        ("big-pickle", "big-pickle"),
        ("mimo-v2.5", "mimo-v2.5-free"),
    ];
    const REQUEST_ALIASES: &'static [(&'static str, &'static str, &'static str)] = &[
        ("deepseek", "deepseek", "deepseek-v4-flash-free"),
        ("bigpickle", "bigpickle", "big-pickle"),
        ("mimo", "mimo", "mimo-v2.5-free"),
    ];
    const HIDDEN_HELPER_MODELS: &'static [(&'static str, &'static str)] =
        &[("claude-haiku-4-5", "deepseek-v4-flash-free")];

    fn is_reserved_public_or_upstream(model_id: &str) -> bool {
        Self::MODELS
            .iter()
            .chain(Self::HIDDEN_HELPER_MODELS.iter())
            .map(|(public, upstream)| (*public, *upstream))
            .chain(
                Self::REQUEST_ALIASES
                    .iter()
                    .map(|(alias, _, upstream)| (*alias, *upstream)),
            )
            .any(|(public, upstream)| public == model_id || upstream == model_id)
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
            .chain(Self::HIDDEN_HELPER_MODELS.iter())
            .find(|(public, _)| *public == public_model)
            .map(|(public, upstream)| ModelResolution {
                public_model: (*public).to_string(),
                upstream_model: (*upstream).to_string(),
                compatibility_profile: ModelCompatibilityProfile::for_static(public)
                    .expect("static model must have a compatibility profile"),
            })
            .or_else(|| {
                Self::REQUEST_ALIASES
                    .iter()
                    .find(|(alias, _, _)| *alias == public_model)
                    .map(|(_, public, upstream)| ModelResolution {
                        public_model: (*public).to_string(),
                        upstream_model: (*upstream).to_string(),
                        compatibility_profile: ModelCompatibilityProfile::for_static(public)
                            .expect("static request alias must have a compatibility profile"),
                    })
            })
            .ok_or_else(|| ModelError::UnknownModel(public_model.to_string()))
    }
}

#[derive(Debug, Clone)]
pub struct EffectiveModelRegistry {
    public_mode: DynamicModelPublicMode,
    discovery: ModelDiscoverySnapshot,
    dynamic_public_allowlist: Vec<String>,
    dynamic_claudecode_compat_allowlist: Vec<String>,
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
        Self::with_dynamic_allowlists(public_mode, discovery, dynamic_public_allowlist, Vec::new())
    }

    pub fn with_dynamic_allowlists(
        public_mode: DynamicModelPublicMode,
        discovery: ModelDiscoverySnapshot,
        dynamic_public_allowlist: Vec<String>,
        dynamic_claudecode_compat_allowlist: Vec<String>,
    ) -> Self {
        Self {
            public_mode,
            discovery,
            dynamic_public_allowlist: dedupe_allowlist(dynamic_public_allowlist),
            dynamic_claudecode_compat_allowlist: dedupe_allowlist(
                dynamic_claudecode_compat_allowlist,
            ),
        }
    }

    pub fn is_dynamic_public(&self, model: &DiscoveredModel) -> bool {
        let Some(public_alias) = dynamic_public_alias(&model.id) else {
            return false;
        };
        self.is_dynamic_routable(model)
            && self.dynamic_public_allowlist_allows(model, &public_alias)
    }

    pub fn is_dynamic_routable(&self, model: &DiscoveredModel) -> bool {
        if dynamic_public_alias(&model.id).is_none() {
            return false;
        }
        if StaticModelRegistry::is_reserved_public_or_upstream(&model.id)
            || StaticModelRegistry::is_reserved_public_or_upstream(&model.upstream_id)
        {
            return false;
        }
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

    fn dynamic_public_allowlist_allows(&self, model: &DiscoveredModel, public_alias: &str) -> bool {
        allowlist_allows(&self.dynamic_public_allowlist, model, public_alias)
    }

    fn public_dynamic_models(&self) -> impl Iterator<Item = &DiscoveredModel> {
        self.discovery
            .models
            .iter()
            .filter(|model| self.is_dynamic_public(model))
    }

    fn resolve_dynamic_model(&self, public_model: &str) -> Option<&DiscoveredModel> {
        self.discovery
            .models
            .iter()
            .filter(|model| self.is_dynamic_routable(model))
            .find(|model| dynamic_public_alias(&model.id).as_deref() == Some(public_model))
    }

    fn compatibility_profile_for_dynamic(
        &self,
        model: &DiscoveredModel,
        public_alias: &str,
    ) -> ModelCompatibilityProfile {
        let base = ModelCompatibilityProfile::for_dynamic(model);
        if matches!(base, ModelCompatibilityProfile::DynamicGeneric)
            && explicit_allowlist_allows(
                &self.dynamic_claudecode_compat_allowlist,
                model,
                public_alias,
            )
        {
            ModelCompatibilityProfile::DynamicClaudeCodeCompatible
        } else {
            base
        }
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
            let compatibility_profile = self.compatibility_profile_for_dynamic(dynamic, &public_id);
            models.push(ModelInfo {
                id: public_id,
                upstream_id: dynamic.upstream_id.clone(),
                compatibility_profile,
            });
        }
        models
    }

    fn resolve(&self, public_model: &str) -> Result<ModelResolution, ModelError> {
        if let Ok(static_model) = StaticModelRegistry.resolve(public_model) {
            return Ok(static_model);
        }
        self.resolve_dynamic_model(public_model)
            .map(|model| {
                let public_alias = dynamic_public_alias(&model.id)
                    .expect("public dynamic model must have a sanitized alias");
                ModelResolution {
                    compatibility_profile: self
                        .compatibility_profile_for_dynamic(model, &public_alias),
                    public_model: public_alias,
                    upstream_model: model.upstream_id.clone(),
                }
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

fn allowlist_allows(allowlist: &[String], model: &DiscoveredModel, public_alias: &str) -> bool {
    allowlist.is_empty() || explicit_allowlist_allows(allowlist, model, public_alias)
}

fn explicit_allowlist_allows(
    allowlist: &[String],
    model: &DiscoveredModel,
    public_alias: &str,
) -> bool {
    !allowlist.is_empty()
        && allowlist.iter().any(|allowed| {
            allowed == public_alias
                || allowed == &model.id
                || allowed == &model.upstream_id
                || dynamic_public_alias(allowed).as_deref() == Some(public_alias)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v4::model_discovery::DynamicModelRegistry;

    #[test]
    fn exposes_static_public_models() {
        let registry = StaticModelRegistry;
        let ids: Vec<String> = registry
            .public_models()
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert_eq!(ids, vec!["deepseek-v4-flash", "big-pickle", "mimo-v2.5"]);
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
            registry.resolve("big-pickle").unwrap().upstream_model,
            "big-pickle"
        );
        assert_eq!(
            registry
                .resolve("big-pickle")
                .unwrap()
                .compatibility_profile,
            ModelCompatibilityProfile::StaticFlashLite
        );
        assert_eq!(
            registry.resolve("mimo-v2.5").unwrap().upstream_model,
            "mimo-v2.5-free"
        );
        assert_eq!(
            registry.resolve("mimo-v2.5").unwrap().compatibility_profile,
            ModelCompatibilityProfile::StaticMimo
        );
    }

    #[test]
    fn resolves_short_request_aliases_without_public_listing() {
        let registry = StaticModelRegistry;
        let ids: Vec<String> = registry
            .public_models()
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert!(!ids.contains(&"deepseek".to_string()));
        assert!(!ids.contains(&"bigpickle".to_string()));
        assert!(!ids.contains(&"mimo".to_string()));

        let deepseek = registry.resolve("deepseek").unwrap();
        assert_eq!(deepseek.public_model, "deepseek");
        assert_eq!(deepseek.upstream_model, "deepseek-v4-flash-free");
        assert_eq!(
            deepseek.compatibility_profile,
            ModelCompatibilityProfile::StaticFlash
        );

        let bigpickle = registry.resolve("bigpickle").unwrap();
        assert_eq!(bigpickle.public_model, "bigpickle");
        assert_eq!(bigpickle.upstream_model, "big-pickle");
        assert_eq!(
            bigpickle.compatibility_profile,
            ModelCompatibilityProfile::StaticFlashLite
        );

        let mimo = registry.resolve("mimo").unwrap();
        assert_eq!(mimo.public_model, "mimo");
        assert_eq!(mimo.upstream_model, "mimo-v2.5-free");
        assert_eq!(
            mimo.compatibility_profile,
            ModelCompatibilityProfile::StaticMimo
        );
    }

    #[test]
    fn resolves_claude_code_webfetch_helper_without_public_listing() {
        let registry = StaticModelRegistry;
        let ids: Vec<String> = registry
            .public_models()
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert!(!ids.contains(&"claude-haiku-4-5".to_string()));

        let helper = registry.resolve("claude-haiku-4-5").unwrap();
        assert_eq!(helper.public_model, "claude-haiku-4-5");
        assert_eq!(helper.upstream_model, "deepseek-v4-flash-free");
        assert_eq!(
            helper.compatibility_profile,
            ModelCompatibilityProfile::StaticFlash
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
        assert_eq!(ids, vec!["deepseek-v4-flash", "big-pickle", "mimo-v2.5"]);
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
                "big-pickle",
                "mimo-v2.5",
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
                "big-pickle",
                "mimo-v2.5",
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
    fn effective_registry_claudecode_allowlist_grants_dynamic_trial_profile() {
        let discovery = DynamicModelRegistry::new(true, "url".into());
        discovery
            .update_from_opencode_json(
                r#"{"data":[{"id":"mimo-v2.5-free"},{"id":"north-mini-code-free"},{"id":"big-pickle"}]}"#,
            )
            .unwrap();
        let registry = EffectiveModelRegistry::with_dynamic_allowlists(
            DynamicModelPublicMode::CandidateCanaryOrActive,
            discovery.snapshot(),
            vec!["mimo-v2.5".into(), "north-mini-code".into()],
            vec!["north-mini-code".into()],
        );

        assert_eq!(
            registry.resolve("mimo-v2.5").unwrap().compatibility_profile,
            ModelCompatibilityProfile::StaticMimo
        );
        assert_eq!(
            registry
                .resolve("north-mini-code")
                .unwrap()
                .compatibility_profile,
            ModelCompatibilityProfile::DynamicClaudeCodeCompatible
        );
        assert_eq!(
            registry
                .resolve("big-pickle")
                .unwrap()
                .compatibility_profile,
            ModelCompatibilityProfile::StaticFlashLite
        );
    }

    #[test]
    fn effective_registry_claudecode_allowlist_accepts_upstream_id() {
        let discovery = DynamicModelRegistry::new(true, "url".into());
        discovery
            .update_from_opencode_json(r#"{"data":[{"id":"north-mini-code-free"}]}"#)
            .unwrap();
        let registry = EffectiveModelRegistry::with_dynamic_allowlists(
            DynamicModelPublicMode::CandidateCanaryOrActive,
            discovery.snapshot(),
            Vec::new(),
            vec!["north-mini-code-free".into()],
        );

        assert_eq!(
            registry
                .resolve("north-mini-code")
                .unwrap()
                .compatibility_profile,
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
            vec!["deepseek-v4-flash", "big-pickle", "mimo-v2.5", "new-active"]
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
        assert_eq!(ids, vec!["deepseek-v4-flash", "big-pickle", "mimo-v2.5"]);
        assert_eq!(
            registry.resolve("mimo-v2.5").unwrap().upstream_model,
            "mimo-v2.5-free"
        );
        assert!(registry.resolve("mimo-v2.5-free").is_err());
        assert!(registry.resolve("deepseek-v4-flash-free").is_err());
        assert_eq!(
            registry.resolve("big-pickle").unwrap().upstream_model,
            "big-pickle"
        );
        assert!(registry.resolve("deepseek-v4-flash-lite").is_err());
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
                "big-pickle",
                "mimo-v2.5",
                "nemotron-3-ultra"
            ]
        );
        assert_eq!(
            registry.resolve("nemotron-3-ultra").unwrap().upstream_model,
            "nemotron-3-ultra-free"
        );
        assert_eq!(
            registry.resolve("north-mini-code").unwrap().upstream_model,
            "north-mini-code-free"
        );
    }

    #[test]
    fn effective_registry_keeps_unlisted_free_models_routable() {
        let discovery = DynamicModelRegistry::new(true, "url".into());
        discovery
            .update_from_opencode_json(
                r#"{"data":[{"id":"mimo-v2.5-free"},{"id":"minimax-m3-free"},{"id":"qwen3.6-plus-free"},{"id":"paid-model"}]}"#,
            )
            .unwrap();
        let registry = EffectiveModelRegistry::with_dynamic_allowlists(
            DynamicModelPublicMode::CandidateCanaryOrActive,
            discovery.snapshot(),
            vec!["mimo-v2.5".into()],
            vec!["mimo-v2.5".into()],
        );

        let ids: Vec<String> = registry
            .public_models()
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert_eq!(ids, vec!["deepseek-v4-flash", "big-pickle", "mimo-v2.5"]);

        let minimax = registry.resolve("minimax-m3").unwrap();
        assert_eq!(minimax.upstream_model, "minimax-m3-free");
        assert_eq!(
            minimax.compatibility_profile,
            ModelCompatibilityProfile::DynamicGeneric
        );
        assert_eq!(
            registry.resolve("qwen3.6-plus").unwrap().upstream_model,
            "qwen3.6-plus-free"
        );
        assert!(registry.resolve("paid-model").is_err());
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
                "big-pickle",
                "mimo-v2.5",
                "nemotron-3-ultra"
            ]
        );
    }
}
