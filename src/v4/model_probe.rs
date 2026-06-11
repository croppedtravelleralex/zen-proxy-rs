use crate::v4::model_discovery::{DiscoveredModel, DiscoveredModelState, DynamicModelRegistry};

pub const REQUIRED_PROBE_NAMES: &[&str] = &[
    "metadata",
    "openai_nonstream_minimal",
    "openai_stream_minimal",
    "anthropic_nonstream_minimal",
    "anthropic_stream_minimal",
    "tool_history_minimal",
    "empty_output_guard",
    "format_guard",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelProbeConfig {
    pub success_quorum: u64,
    pub failure_quarantine_threshold: u64,
    pub required_probe_names: Vec<String>,
}

impl Default for ModelProbeConfig {
    fn default() -> Self {
        Self {
            success_quorum: 2,
            failure_quarantine_threshold: 3,
            required_probe_names: REQUIRED_PROBE_NAMES
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
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
    pub probe_name: Option<String>,
    pub code: String,
    pub message: String,
    pub hard_protocol_failure: bool,
}

impl ModelProbeFailure {
    pub fn soft(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            probe_name: None,
            code: code.into(),
            message: message.into(),
            hard_protocol_failure: false,
        }
    }

    pub fn hard_protocol(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            probe_name: None,
            code: code.into(),
            message: message.into(),
            hard_protocol_failure: true,
        }
    }

    pub fn for_probe(mut self, probe_name: impl Into<String>) -> Self {
        self.probe_name = Some(probe_name.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelProbeOutcome {
    Passed,
    Failed(ModelProbeFailure),
}

pub trait ModelProbeAdapter {
    fn run_probe(&self, model: &DiscoveredModel, probe_name: &str) -> ModelProbeOutcome;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct AllPassProbeAdapter;

impl ModelProbeAdapter for AllPassProbeAdapter {
    fn run_probe(&self, _model: &DiscoveredModel, _probe_name: &str) -> ModelProbeOutcome {
        ModelProbeOutcome::Passed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelProbeRunSummary {
    pub model_id: String,
    pub attempted_probe_names: Vec<String>,
    pub passed_probe_names: Vec<String>,
    pub failed_probe_name: Option<String>,
    pub final_state: DiscoveredModelState,
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
        if probed.consecutive_probe_successes >= self.config.success_quorum
            && self.required_probes_passed(&probed)
        {
            return registry
                .set_model_state(
                    model_id,
                    DiscoveredModelState::Canary,
                    format!(
                        "probe matrix passed: {} required probes, {} consecutive successes",
                        self.config.required_probe_names.len(),
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
            .record_probe_failure(
                model_id,
                failure.code.clone(),
                failure.message,
                failure.probe_name.clone(),
            )
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

    pub fn required_probe_names(&self) -> &[String] {
        &self.config.required_probe_names
    }

    pub fn required_probes_passed(&self, model: &DiscoveredModel) -> bool {
        self.config.required_probe_names.iter().all(|required| {
            model
                .passed_probe_names
                .iter()
                .any(|passed| passed == required)
        })
    }

    pub fn missing_required_probe_names(&self, model: &DiscoveredModel) -> Vec<String> {
        self.config
            .required_probe_names
            .iter()
            .filter(|required| {
                !model
                    .passed_probe_names
                    .iter()
                    .any(|passed| passed == *required)
            })
            .cloned()
            .collect()
    }

    pub fn run_required_probes<A: ModelProbeAdapter>(
        &self,
        registry: &DynamicModelRegistry,
        model_id: &str,
        adapter: &A,
    ) -> Result<ModelProbeRunSummary, ModelProbeError> {
        let mut attempted_probe_names = Vec::new();
        for probe_name in self.required_probe_names() {
            let started = self.start_probe(registry, model_id)?;
            attempted_probe_names.push(probe_name.clone());
            match adapter.run_probe(&started, probe_name) {
                ModelProbeOutcome::Passed => {
                    let model = self.record_success(registry, model_id, probe_name)?;
                    if matches!(
                        model.state,
                        DiscoveredModelState::Canary | DiscoveredModelState::Active
                    ) {
                        return Ok(ModelProbeRunSummary {
                            model_id: model.id,
                            attempted_probe_names,
                            passed_probe_names: model.passed_probe_names,
                            failed_probe_name: None,
                            final_state: model.state,
                        });
                    }
                }
                ModelProbeOutcome::Failed(failure) => {
                    let failed_probe_name = failure
                        .probe_name
                        .clone()
                        .or_else(|| Some(probe_name.clone()));
                    let model = self.record_failure(
                        registry,
                        model_id,
                        failure.for_probe(probe_name.clone()),
                    )?;
                    return Ok(ModelProbeRunSummary {
                        model_id: model.id,
                        attempted_probe_names,
                        passed_probe_names: model.passed_probe_names,
                        failed_probe_name,
                        final_state: model.state,
                    });
                }
            }
        }

        let model = registry
            .get(model_id)
            .ok_or_else(|| ModelProbeError::ModelNotFound(model_id.to_string()))?;
        Ok(ModelProbeRunSummary {
            model_id: model.id,
            attempted_probe_names,
            passed_probe_names: model.passed_probe_names,
            failed_probe_name: None,
            final_state: model.state,
        })
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
    fn two_successes_do_not_promote_without_full_required_probe_matrix() {
        let registry = registry_with_models();
        let engine = ModelProbeEngine::new(ModelProbeConfig::default());

        engine.start_probe(&registry, "good-free").unwrap();
        let first = engine
            .record_success(&registry, "good-free", "metadata")
            .unwrap();
        assert_eq!(first.state, DiscoveredModelState::ProbePending);
        assert_eq!(first.consecutive_probe_successes, 1);
        assert!(!first.public);

        engine.start_probe(&registry, "good-free").unwrap();
        let still_pending = engine
            .record_success(&registry, "good-free", "openai_nonstream_minimal")
            .unwrap();
        assert_eq!(still_pending.state, DiscoveredModelState::ProbePending);
        assert_eq!(still_pending.probe_success_total, 2);
        assert!(!still_pending.public);
        assert_eq!(
            engine.missing_required_probe_names(&still_pending),
            vec![
                "openai_stream_minimal".to_string(),
                "anthropic_nonstream_minimal".to_string(),
                "anthropic_stream_minimal".to_string(),
                "tool_history_minimal".to_string(),
                "empty_output_guard".to_string(),
                "format_guard".to_string()
            ]
        );
    }

    #[test]
    fn full_required_probe_matrix_promotes_candidate_to_canary() {
        let registry = registry_with_models();
        let engine = ModelProbeEngine::new(ModelProbeConfig::default());
        let mut last = None;

        for probe_name in REQUIRED_PROBE_NAMES {
            engine.start_probe(&registry, "good-free").unwrap();
            last = Some(
                engine
                    .record_success(&registry, "good-free", probe_name)
                    .unwrap(),
            );
        }

        let promoted = last.expect("last probe result");
        assert_eq!(promoted.state, DiscoveredModelState::Canary);
        assert_eq!(
            promoted.probe_success_total,
            REQUIRED_PROBE_NAMES.len() as u64
        );
        assert!(engine.required_probes_passed(&promoted));
        assert_eq!(
            promoted.promotion_reason.as_deref(),
            Some("probe matrix passed: 8 required probes, 8 consecutive successes")
        );
    }

    #[test]
    fn soft_failures_quarantine_only_after_threshold() {
        let registry = registry_with_models();
        let engine = ModelProbeEngine::new(ModelProbeConfig {
            success_quorum: 2,
            failure_quarantine_threshold: 2,
            ..ModelProbeConfig::default()
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

    #[derive(Debug, Default)]
    struct MockProbeAdapter {
        failures: Vec<(String, ModelProbeFailure)>,
    }

    impl MockProbeAdapter {
        fn failing(probe_name: &str, failure: ModelProbeFailure) -> Self {
            Self {
                failures: vec![(probe_name.to_string(), failure)],
            }
        }
    }

    impl ModelProbeAdapter for MockProbeAdapter {
        fn run_probe(&self, _model: &DiscoveredModel, probe_name: &str) -> ModelProbeOutcome {
            self.failures
                .iter()
                .find(|(name, _)| name == probe_name)
                .map(|(_, failure)| ModelProbeOutcome::Failed(failure.clone()))
                .unwrap_or(ModelProbeOutcome::Passed)
        }
    }

    #[test]
    fn runner_promotes_after_complete_mock_probe_matrix() {
        let registry = registry_with_models();
        let engine = ModelProbeEngine::new(ModelProbeConfig::default());
        let summary = engine
            .run_required_probes(&registry, "good-free", &MockProbeAdapter::default())
            .unwrap();

        assert_eq!(summary.final_state, DiscoveredModelState::Canary);
        assert_eq!(summary.failed_probe_name, None);
        assert_eq!(
            summary.attempted_probe_names.len(),
            REQUIRED_PROBE_NAMES.len()
        );
        assert_eq!(summary.passed_probe_names.len(), REQUIRED_PROBE_NAMES.len());
        let model = registry.get("good-free").unwrap();
        assert!(model.public);
        assert_eq!(
            model.probe_attempts_total,
            REQUIRED_PROBE_NAMES.len() as u64
        );
    }

    #[test]
    fn runner_stops_and_quarantines_on_hard_protocol_failure() {
        let registry = registry_with_models();
        let engine = ModelProbeEngine::new(ModelProbeConfig::default());
        let summary = engine
            .run_required_probes(
                &registry,
                "good-free",
                &MockProbeAdapter::failing(
                    "tool_history_minimal",
                    ModelProbeFailure::hard_protocol(
                        "provider_invalid_tool_history",
                        "missing tool_call_id",
                    ),
                ),
            )
            .unwrap();

        assert_eq!(summary.final_state, DiscoveredModelState::Quarantined);
        assert_eq!(
            summary.failed_probe_name.as_deref(),
            Some("tool_history_minimal")
        );
        assert_eq!(
            summary.attempted_probe_names,
            vec![
                "metadata".to_string(),
                "openai_nonstream_minimal".to_string(),
                "openai_stream_minimal".to_string(),
                "anthropic_nonstream_minimal".to_string(),
                "anthropic_stream_minimal".to_string(),
                "tool_history_minimal".to_string()
            ]
        );
        let model = registry.get("good-free").unwrap();
        assert_eq!(
            model.last_probe_name.as_deref(),
            Some("tool_history_minimal")
        );
        assert!(!model.public);
        assert!(!model.routable);
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
