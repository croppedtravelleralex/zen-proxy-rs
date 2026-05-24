use serde::{Deserialize, Serialize};

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
