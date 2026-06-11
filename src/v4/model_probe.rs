use crate::v4::model_discovery::{DiscoveredModel, DiscoveredModelState, DynamicModelRegistry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelProbeConfig {
    pub success_quorum: u64,
    pub failure_quarantine_threshold: u64,
}

impl Default for ModelProbeConfig {
    fn default() -> Self {
        Self {
            success_quorum: 2,
            failure_quarantine_threshold: 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelProbeError {
    ModelNotFound(String),
    ModelNotProbeable {
        model_id: String,
        state: DiscoveredModelState,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelProbeFailure {
    pub code: String,
    pub message: String,
    pub hard_protocol_failure: bool,
}

impl ModelProbeFailure {
    pub fn soft(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            hard_protocol_failure: false,
        }
    }

    pub fn hard_protocol(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            hard_protocol_failure: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelProbeEngine {
    config: ModelProbeConfig,
}

impl ModelProbeEngine {
    pub fn new(config: ModelProbeConfig) -> Self {
        Self { config }
    }

    pub fn start_probe(
        &self,
        registry: &DynamicModelRegistry,
        model_id: &str,
    ) -> Result<DiscoveredModel, ModelProbeError> {
        self.ensure_probeable(registry, model_id)?;
        registry
            .record_probe_start(model_id)
            .ok_or_else(|| ModelProbeError::ModelNotFound(model_id.to_string()))
    }

    pub fn record_success(
        &self,
        registry: &DynamicModelRegistry,
        model_id: &str,
        probe_name: &str,
    ) -> Result<DiscoveredModel, ModelProbeError> {
        self.ensure_probeable(registry, model_id)?;
        let probed = registry
            .record_probe_success(model_id, probe_name)
            .ok_or_else(|| ModelProbeError::ModelNotFound(model_id.to_string()))?;
        if probed.consecutive_probe_successes >= self.config.success_quorum {
            return registry
                .set_model_state(
                    model_id,
                    DiscoveredModelState::Canary,
                    format!(
                        "probe success quorum met: {} consecutive successes",
                        probed.consecutive_probe_successes
                    ),
                )
                .ok_or_else(|| ModelProbeError::ModelNotFound(model_id.to_string()));
        }
        Ok(probed)
    }

    pub fn record_failure(
        &self,
        registry: &DynamicModelRegistry,
        model_id: &str,
        failure: ModelProbeFailure,
    ) -> Result<DiscoveredModel, ModelProbeError> {
        self.ensure_probeable(registry, model_id)?;
        let probed = registry
            .record_probe_failure(model_id, failure.code.clone(), failure.message)
            .ok_or_else(|| ModelProbeError::ModelNotFound(model_id.to_string()))?;
        if failure.hard_protocol_failure
            || probed.consecutive_probe_failures >= self.config.failure_quarantine_threshold
        {
            return registry
                .set_model_state(
                    model_id,
                    DiscoveredModelState::Quarantined,
                    format!(
                        "probe quarantine: code={}, consecutive_failures={}",
                        failure.code, probed.consecutive_probe_failures
                    ),
                )
                .ok_or_else(|| ModelProbeError::ModelNotFound(model_id.to_string()));
        }
        Ok(probed)
    }

    fn ensure_probeable(
        &self,
        registry: &DynamicModelRegistry,
        model_id: &str,
    ) -> Result<(), ModelProbeError> {
        let model = registry
            .get(model_id)
            .ok_or_else(|| ModelProbeError::ModelNotFound(model_id.to_string()))?;
        match model.state {
            DiscoveredModelState::Candidate | DiscoveredModelState::ProbePending => Ok(()),
            state => Err(ModelProbeError::ModelNotProbeable {
                model_id: model_id.to_string(),
                state,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry_with_models() -> DynamicModelRegistry {
        let registry = DynamicModelRegistry::new(true, "url".into());
        registry
            .update_from_opencode_json(
                r#"{"data":[{"id":"good-free"},{"id":"paid-model"},{"id":"missing-free"}]}"#,
            )
            .unwrap();
        registry
            .update_from_opencode_json(r#"{"data":[{"id":"good-free"},{"id":"paid-model"}]}"#)
            .unwrap();
        registry
    }

    #[test]
    fn success_quorum_promotes_candidate_to_canary() {
        let registry = registry_with_models();
        let engine = ModelProbeEngine::new(ModelProbeConfig::default());

        engine.start_probe(&registry, "good-free").unwrap();
        let first = engine
            .record_success(&registry, "good-free", "openai_stream_minimal")
            .unwrap();
        assert_eq!(first.state, DiscoveredModelState::ProbePending);
        assert_eq!(first.consecutive_probe_successes, 1);
        assert!(!first.public);

        engine.start_probe(&registry, "good-free").unwrap();
        let promoted = engine
            .record_success(&registry, "good-free", "anthropic_stream_minimal")
            .unwrap();
        assert_eq!(promoted.state, DiscoveredModelState::Canary);
        assert_eq!(promoted.probe_success_total, 2);
        assert_eq!(
            promoted.promotion_reason.as_deref(),
            Some("probe success quorum met: 2 consecutive successes")
        );
    }

    #[test]
    fn soft_failures_quarantine_only_after_threshold() {
        let registry = registry_with_models();
        let engine = ModelProbeEngine::new(ModelProbeConfig {
            success_quorum: 2,
            failure_quarantine_threshold: 2,
        });

        engine.start_probe(&registry, "good-free").unwrap();
        let first = engine
            .record_failure(
                &registry,
                "good-free",
                ModelProbeFailure::soft("provider_empty_output", "empty assistant output"),
            )
            .unwrap();
        assert_eq!(first.state, DiscoveredModelState::ProbePending);
        assert_eq!(first.consecutive_probe_failures, 1);
        assert!(!first.public);

        engine.start_probe(&registry, "good-free").unwrap();
        let quarantined = engine
            .record_failure(
                &registry,
                "good-free",
                ModelProbeFailure::soft("provider_empty_output", "empty assistant output"),
            )
            .unwrap();
        assert_eq!(quarantined.state, DiscoveredModelState::Quarantined);
        assert_eq!(quarantined.probe_failure_total, 2);
        assert!(!quarantined.public);
        assert!(!quarantined.routable);
    }

    #[test]
    fn hard_protocol_failure_quarantines_immediately() {
        let registry = registry_with_models();
        let engine = ModelProbeEngine::new(ModelProbeConfig::default());

        engine.start_probe(&registry, "good-free").unwrap();
        let quarantined = engine
            .record_failure(
                &registry,
                "good-free",
                ModelProbeFailure::hard_protocol(
                    "provider_invalid_tool_history",
                    "missing tool_call_id",
                ),
            )
            .unwrap();
        assert_eq!(quarantined.state, DiscoveredModelState::Quarantined);
        assert_eq!(quarantined.probe_failure_total, 1);
    }

    #[test]
    fn ignored_missing_and_unknown_models_are_not_probeable() {
        let registry = registry_with_models();
        let engine = ModelProbeEngine::new(ModelProbeConfig::default());

        assert!(matches!(
            engine.start_probe(&registry, "paid-model"),
            Err(ModelProbeError::ModelNotProbeable {
                state: DiscoveredModelState::Ignored,
                ..
            })
        ));
        assert!(matches!(
            engine.start_probe(&registry, "missing-free"),
            Err(ModelProbeError::ModelNotProbeable {
                state: DiscoveredModelState::Missing,
                ..
            })
        ));
        assert!(matches!(
            engine.start_probe(&registry, "unknown-free"),
            Err(ModelProbeError::ModelNotFound(model)) if model == "unknown-free"
        ));
    }
}
