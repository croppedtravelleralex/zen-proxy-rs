use crate::config::DynamicModelProbeAdapterMode;
use crate::state::AppState;
use crate::v4::model_discovery::DiscoveredModelState;
use crate::v4::model_probe::{
    AllPassProbeAdapter, BoundedHttpProbeAdapter, HttpProbeConfig, ModelProbeConfig,
    ModelProbeEngine, ModelProbeError, ModelProbeRunSummary, ReqwestBlockingProbeTransport,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DynamicModelProbeRunError {
    AdapterDisabled,
    HttpBaseUrlMissing,
    Model(ModelProbeError),
    WorkerJoin(String),
}

impl From<ModelProbeError> for DynamicModelProbeRunError {
    fn from(value: ModelProbeError) -> Self {
        Self::Model(value)
    }
}

pub async fn run_dynamic_model_probe_once(
    state: &AppState,
    model_id: &str,
) -> Result<ModelProbeRunSummary, DynamicModelProbeRunError> {
    let (
        adapter_mode,
        success_quorum,
        failure_quarantine_threshold,
        timeout_secs,
        base_url,
        api_key,
        max_response_bytes,
    ) = {
        let cfg = state.config.read().unwrap();
        (
            cfg.dynamic_model_probe_adapter_mode,
            cfg.dynamic_model_probe_success_quorum,
            cfg.dynamic_model_probe_failure_quarantine_threshold,
            cfg.dynamic_model_probe_timeout_secs,
            cfg.dynamic_model_probe_base_url.clone(),
            cfg.dynamic_model_probe_api_key.clone(),
            cfg.dynamic_model_probe_max_response_bytes,
        )
    };

    let probe_config = ModelProbeConfig {
        success_quorum,
        failure_quarantine_threshold,
        ..ModelProbeConfig::default()
    };

    match adapter_mode {
        DynamicModelProbeAdapterMode::Disabled => Err(DynamicModelProbeRunError::AdapterDisabled),
        DynamicModelProbeAdapterMode::HarnessAllPass => {
            let engine = ModelProbeEngine::new(probe_config);
            let adapter = AllPassProbeAdapter;
            engine
                .run_required_probes(&state.dynamic_models, model_id, &adapter)
                .map_err(Into::into)
        }
        DynamicModelProbeAdapterMode::HttpBounded => {
            if base_url.trim().is_empty() {
                return Err(DynamicModelProbeRunError::HttpBaseUrlMissing);
            }
            let registry = state.dynamic_models.clone();
            let model_id = model_id.to_string();
            let http_config = HttpProbeConfig {
                base_url,
                api_key,
                timeout_secs,
                max_response_bytes,
            };
            tokio::task::spawn_blocking(move || {
                let engine = ModelProbeEngine::new(probe_config);
                let adapter =
                    BoundedHttpProbeAdapter::new(http_config, ReqwestBlockingProbeTransport);
                engine.run_required_probes(&registry, &model_id, &adapter)
            })
            .await
            .map_err(|err| DynamicModelProbeRunError::WorkerJoin(err.to_string()))?
            .map_err(Into::into)
        }
    }
}

pub fn probe_error_http_status(error: &DynamicModelProbeRunError) -> axum::http::StatusCode {
    match error {
        DynamicModelProbeRunError::AdapterDisabled
        | DynamicModelProbeRunError::HttpBaseUrlMissing => {
            axum::http::StatusCode::UNPROCESSABLE_ENTITY
        }
        DynamicModelProbeRunError::Model(ModelProbeError::ModelNotFound(_)) => {
            axum::http::StatusCode::NOT_FOUND
        }
        DynamicModelProbeRunError::Model(ModelProbeError::ModelNotProbeable { .. }) => {
            axum::http::StatusCode::CONFLICT
        }
        DynamicModelProbeRunError::WorkerJoin(_) => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub fn probe_error_message(error: DynamicModelProbeRunError) -> String {
    match error {
        DynamicModelProbeRunError::AdapterDisabled => {
            "dynamic model probe adapter is disabled".to_string()
        }
        DynamicModelProbeRunError::HttpBaseUrlMissing => {
            "DYNAMIC_MODEL_PROBE_BASE_URL is required for http_bounded manual probes".to_string()
        }
        DynamicModelProbeRunError::Model(ModelProbeError::ModelNotFound(model_id)) => {
            format!("dynamic model not found: {model_id}")
        }
        DynamicModelProbeRunError::Model(ModelProbeError::ModelNotProbeable {
            model_id,
            state,
        }) => {
            let state = state_name(&state);
            format!("dynamic model is not probeable in state {state}: {model_id}")
        }
        DynamicModelProbeRunError::WorkerJoin(message) => {
            format!("dynamic model probe worker failed: {message}")
        }
    }
}

fn state_name(state: &DiscoveredModelState) -> &'static str {
    match state {
        DiscoveredModelState::Candidate => "candidate",
        DiscoveredModelState::ProbePending => "probe_pending",
        DiscoveredModelState::Canary => "canary",
        DiscoveredModelState::Active => "active",
        DiscoveredModelState::Ignored => "ignored",
        DiscoveredModelState::Missing => "missing",
        DiscoveredModelState::Retired => "retired",
        DiscoveredModelState::Quarantined => "quarantined",
    }
}
