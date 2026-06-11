use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveredModelState {
    Candidate,
    ProbePending,
    Canary,
    Active,
    Ignored,
    Missing,
    Retired,
    Quarantined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredModel {
    pub id: String,
    pub upstream_id: String,
    pub state: DiscoveredModelState,
    pub reason: String,
    pub first_seen_unix: u64,
    pub last_seen_unix: u64,
    pub probe_required: bool,
    pub auto_promoted: bool,
    pub public: bool,
    pub routable: bool,
    #[serde(default)]
    pub last_probe_unix: Option<u64>,
    #[serde(default)]
    pub last_success_unix: Option<u64>,
    #[serde(default)]
    pub last_failure_unix: Option<u64>,
    #[serde(default)]
    pub last_failure_code: Option<String>,
    #[serde(default)]
    pub last_failure_message: Option<String>,
    #[serde(default)]
    pub probe_attempts_total: u64,
    #[serde(default)]
    pub probe_success_total: u64,
    #[serde(default)]
    pub probe_failure_total: u64,
    #[serde(default)]
    pub consecutive_probe_successes: u64,
    #[serde(default)]
    pub consecutive_probe_failures: u64,
    #[serde(default)]
    pub missing_rounds: u64,
    #[serde(default)]
    pub promotion_reason: Option<String>,
    #[serde(default)]
    pub rollback_reason: Option<String>,
    #[serde(default)]
    pub retirement_reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelDiscoverySnapshot {
    pub enabled: bool,
    pub source_url: String,
    pub last_attempt_unix: Option<u64>,
    pub last_success_unix: Option<u64>,
    pub last_error: Option<String>,
    pub worker_running: bool,
    pub discovered_total: usize,
    pub candidate_total: usize,
    pub ignored_total: usize,
    pub missing_total: usize,
    pub models: Vec<DiscoveredModel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct OpenCodeModelsResponse {
    data: Vec<OpenCodeModel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct OpenCodeModel {
    id: String,
}

#[derive(Debug, Default)]
pub struct DynamicModelRegistry {
    inner: RwLock<ModelDiscoverySnapshot>,
}

impl DynamicModelRegistry {
    pub fn new(enabled: bool, source_url: String) -> Self {
        Self {
            inner: RwLock::new(ModelDiscoverySnapshot {
                enabled,
                source_url,
                ..ModelDiscoverySnapshot::default()
            }),
        }
    }

    pub fn snapshot(&self) -> ModelDiscoverySnapshot {
        self.inner.read().unwrap().clone()
    }

    pub fn set_config(&self, enabled: bool, source_url: String) {
        let mut snapshot = self.inner.write().unwrap();
        snapshot.enabled = enabled;
        snapshot.source_url = source_url;
    }

    pub fn set_worker_running(&self, worker_running: bool) {
        self.inner.write().unwrap().worker_running = worker_running;
    }

    pub fn record_attempt(&self) {
        let mut snapshot = self.inner.write().unwrap();
        snapshot.last_attempt_unix = Some(now_unix());
    }

    pub fn record_error(&self, error: impl Into<String>) {
        let mut snapshot = self.inner.write().unwrap();
        snapshot.last_attempt_unix = Some(now_unix());
        snapshot.last_error = Some(error.into());
    }

    pub fn update_from_opencode_json(&self, body: &str) -> Result<ModelDiscoverySnapshot, String> {
        let response: OpenCodeModelsResponse =
            serde_json::from_str(body).map_err(|err| format!("invalid models json: {err}"))?;
        let now = now_unix();
        let mut seen_this_round = std::collections::BTreeSet::new();

        let mut merged: BTreeMap<String, DiscoveredModel> = self
            .inner
            .read()
            .unwrap()
            .models
            .iter()
            .cloned()
            .map(|model| (model.id.clone(), model))
            .collect();

        for model in response.data {
            seen_this_round.insert(model.id.clone());
            let (state, reason) = classify_model(&model.id);
            let entry = merged
                .entry(model.id.clone())
                .or_insert_with(|| DiscoveredModel {
                    id: model.id.clone(),
                    upstream_id: model.id.clone(),
                    state: state.clone(),
                    reason: reason.clone(),
                    first_seen_unix: now,
                    last_seen_unix: now,
                    probe_required: matches!(state, DiscoveredModelState::Candidate),
                    auto_promoted: false,
                    public: false,
                    routable: false,
                    last_probe_unix: None,
                    last_success_unix: None,
                    last_failure_unix: None,
                    last_failure_code: None,
                    last_failure_message: None,
                    probe_attempts_total: 0,
                    probe_success_total: 0,
                    probe_failure_total: 0,
                    consecutive_probe_successes: 0,
                    consecutive_probe_failures: 0,
                    missing_rounds: 0,
                    promotion_reason: None,
                    rollback_reason: None,
                    retirement_reason: None,
                });
            entry.state = state;
            entry.reason = reason;
            entry.last_seen_unix = now;
            entry.probe_required = matches!(entry.state, DiscoveredModelState::Candidate);
            entry.auto_promoted = false;
            entry.public = false;
            entry.routable = false;
            entry.missing_rounds = 0;
        }

        for model in merged.values_mut() {
            if !seen_this_round.contains(&model.id) {
                model.state = DiscoveredModelState::Missing;
                model.reason =
                    "previously discovered model is absent from the latest upstream list"
                        .to_string();
                model.probe_required = true;
                model.auto_promoted = false;
                model.public = false;
                model.routable = false;
                model.missing_rounds = model.missing_rounds.saturating_add(1);
            }
        }

        let mut models: Vec<_> = merged.into_values().collect();
        models.sort_by(|a, b| a.id.cmp(&b.id));

        let candidate_total = models
            .iter()
            .filter(|model| matches!(model.state, DiscoveredModelState::Candidate))
            .count();
        let ignored_total = models
            .iter()
            .filter(|model| matches!(model.state, DiscoveredModelState::Ignored))
            .count();
        let missing_total = models
            .iter()
            .filter(|model| matches!(model.state, DiscoveredModelState::Missing))
            .count();

        let mut snapshot = self.inner.write().unwrap();
        snapshot.last_attempt_unix = Some(now);
        snapshot.last_success_unix = Some(now);
        snapshot.last_error = None;
        snapshot.discovered_total = models.len();
        snapshot.candidate_total = candidate_total;
        snapshot.ignored_total = ignored_total;
        snapshot.missing_total = missing_total;
        snapshot.models = models;
        Ok(snapshot.clone())
    }

    pub fn get(&self, model_id: &str) -> Option<DiscoveredModel> {
        self.inner
            .read()
            .unwrap()
            .models
            .iter()
            .find(|model| model.id == model_id)
            .cloned()
    }

    pub fn set_model_state(
        &self,
        model_id: &str,
        state: DiscoveredModelState,
        reason: impl Into<String>,
    ) -> Option<DiscoveredModel> {
        let now = now_unix();
        let reason = reason.into();
        let mut snapshot = self.inner.write().unwrap();
        let index = snapshot
            .models
            .iter()
            .position(|model| model.id == model_id)?;
        {
            let model = &mut snapshot.models[index];
            let previous_state = model.state.clone();
            model.state = state;
            model.reason = reason.clone();
            model.last_seen_unix = now;
            model.probe_required = matches!(
                model.state,
                DiscoveredModelState::Candidate
                    | DiscoveredModelState::ProbePending
                    | DiscoveredModelState::Missing
            );
            model.auto_promoted = matches!(
                model.state,
                DiscoveredModelState::Canary | DiscoveredModelState::Active
            );
            model.public = matches!(
                model.state,
                DiscoveredModelState::Canary | DiscoveredModelState::Active
            );
            model.routable = model.public;
            match model.state {
                DiscoveredModelState::Canary | DiscoveredModelState::Active => {
                    model.promotion_reason = Some(reason);
                }
                DiscoveredModelState::Candidate
                    if matches!(
                        previous_state,
                        DiscoveredModelState::Canary
                            | DiscoveredModelState::Active
                            | DiscoveredModelState::Quarantined
                            | DiscoveredModelState::Retired
                    ) =>
                {
                    model.rollback_reason = Some(reason);
                }
                DiscoveredModelState::Retired => {
                    model.retirement_reason = Some(reason);
                }
                _ => {}
            }
        }
        recompute_counts(&mut snapshot);
        Some(snapshot.models[index].clone())
    }

    pub fn record_probe_start(&self, model_id: &str) -> Option<DiscoveredModel> {
        let now = now_unix();
        let mut snapshot = self.inner.write().unwrap();
        let index = snapshot
            .models
            .iter()
            .position(|model| model.id == model_id)?;
        {
            let model = &mut snapshot.models[index];
            model.state = DiscoveredModelState::ProbePending;
            model.reason = "model probe started; awaiting probe result".to_string();
            model.last_probe_unix = Some(now);
            model.last_seen_unix = now;
            model.probe_attempts_total = model.probe_attempts_total.saturating_add(1);
            model.probe_required = true;
            model.auto_promoted = false;
            model.public = false;
            model.routable = false;
        }
        recompute_counts(&mut snapshot);
        Some(snapshot.models[index].clone())
    }

    pub fn record_probe_success(
        &self,
        model_id: &str,
        probe_name: impl Into<String>,
    ) -> Option<DiscoveredModel> {
        let now = now_unix();
        let probe_name = probe_name.into();
        let mut snapshot = self.inner.write().unwrap();
        let index = snapshot
            .models
            .iter()
            .position(|model| model.id == model_id)?;
        {
            let model = &mut snapshot.models[index];
            model.reason = format!("probe passed: {probe_name}; promotion quorum still required");
            model.last_probe_unix = Some(now);
            model.last_success_unix = Some(now);
            model.last_seen_unix = now;
            model.last_failure_code = None;
            model.last_failure_message = None;
            model.probe_success_total = model.probe_success_total.saturating_add(1);
            model.consecutive_probe_successes = model.consecutive_probe_successes.saturating_add(1);
            model.consecutive_probe_failures = 0;
            model.probe_required = true;
            model.auto_promoted = false;
            model.public = false;
            model.routable = false;
        }
        recompute_counts(&mut snapshot);
        Some(snapshot.models[index].clone())
    }

    pub fn record_probe_failure(
        &self,
        model_id: &str,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Option<DiscoveredModel> {
        let now = now_unix();
        let code = code.into();
        let message = message.into();
        let mut snapshot = self.inner.write().unwrap();
        let index = snapshot
            .models
            .iter()
            .position(|model| model.id == model_id)?;
        {
            let model = &mut snapshot.models[index];
            model.reason = format!("probe failed: {code}");
            model.last_probe_unix = Some(now);
            model.last_failure_unix = Some(now);
            model.last_seen_unix = now;
            model.last_failure_code = Some(code);
            model.last_failure_message = Some(message);
            model.probe_failure_total = model.probe_failure_total.saturating_add(1);
            model.consecutive_probe_failures = model.consecutive_probe_failures.saturating_add(1);
            model.consecutive_probe_successes = 0;
            model.probe_required = true;
            model.auto_promoted = false;
            model.public = false;
            model.routable = false;
        }
        recompute_counts(&mut snapshot);
        Some(snapshot.models[index].clone())
    }
}

fn classify_model(id: &str) -> (DiscoveredModelState, String) {
    if is_free_candidate(id) {
        (
            DiscoveredModelState::Candidate,
            "free-looking opencode model; probe required before exposure".to_string(),
        )
    } else {
        (
            DiscoveredModelState::Ignored,
            "not a free-model candidate".to_string(),
        )
    }
}

pub fn is_free_candidate(id: &str) -> bool {
    id == "big-pickle" || id.ends_with("-free")
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn recompute_counts(snapshot: &mut ModelDiscoverySnapshot) {
    snapshot.discovered_total = snapshot.models.len();
    snapshot.candidate_total = snapshot
        .models
        .iter()
        .filter(|model| matches!(model.state, DiscoveredModelState::Candidate))
        .count();
    snapshot.ignored_total = snapshot
        .models
        .iter()
        .filter(|model| matches!(model.state, DiscoveredModelState::Ignored))
        .count();
    snapshot.missing_total = snapshot
        .models
        .iter()
        .filter(|model| matches!(model.state, DiscoveredModelState::Missing))
        .count();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_only_free_looking_models_as_candidates() {
        assert!(is_free_candidate("deepseek-v4-flash-free"));
        assert!(is_free_candidate("mimo-v2.5-free"));
        assert!(is_free_candidate("big-pickle"));
        assert!(!is_free_candidate("gpt-5.5"));
        assert!(!is_free_candidate("claude-sonnet-4-6"));
    }

    #[test]
    fn discovery_keeps_candidates_out_of_auto_promotion() {
        let registry = DynamicModelRegistry::new(true, "https://opencode.ai/zen/v1/models".into());
        let snapshot = registry
            .update_from_opencode_json(
                r#"{"object":"list","data":[{"id":"deepseek-v4-flash-free"},{"id":"gpt-5.5"},{"id":"big-pickle"}]}"#,
            )
            .unwrap();

        assert_eq!(snapshot.candidate_total, 2);
        assert_eq!(snapshot.ignored_total, 1);
        assert!(snapshot
            .models
            .iter()
            .filter(|model| matches!(model.state, DiscoveredModelState::Candidate))
            .all(|model| model.probe_required && !model.auto_promoted));
    }

    #[test]
    fn discovery_preserves_first_seen_and_updates_last_seen() {
        let registry = DynamicModelRegistry::new(true, "url".into());
        let first = registry
            .update_from_opencode_json(r#"{"data":[{"id":"mimo-v2.5-free"}]}"#)
            .unwrap();
        let first_seen = first.models[0].first_seen_unix;
        let second = registry
            .update_from_opencode_json(r#"{"data":[{"id":"mimo-v2.5-free"}]}"#)
            .unwrap();

        assert_eq!(second.models[0].first_seen_unix, first_seen);
        assert!(second.models[0].last_seen_unix >= first_seen);
    }

    #[test]
    fn discovery_marks_absent_models_as_missing() {
        let registry = DynamicModelRegistry::new(true, "url".into());
        registry
            .update_from_opencode_json(
                r#"{"data":[{"id":"mimo-v2.5-free"},{"id":"not-free-model"}]}"#,
            )
            .unwrap();
        let second = registry
            .update_from_opencode_json(r#"{"data":[{"id":"big-pickle"}]}"#)
            .unwrap();

        assert_eq!(second.candidate_total, 1);
        assert_eq!(second.missing_total, 2);
        assert!(second
            .models
            .iter()
            .filter(|model| matches!(model.state, DiscoveredModelState::Missing))
            .all(|model| model.probe_required && !model.auto_promoted));
    }

    #[test]
    fn discovery_counts_consecutive_missing_rounds() {
        let registry = DynamicModelRegistry::new(true, "url".into());
        registry
            .update_from_opencode_json(r#"{"data":[{"id":"mimo-v2.5-free"}]}"#)
            .unwrap();
        let missing_once = registry
            .update_from_opencode_json(r#"{"data":[]}"#)
            .unwrap();
        assert_eq!(missing_once.models[0].missing_rounds, 1);

        let missing_twice = registry
            .update_from_opencode_json(r#"{"data":[]}"#)
            .unwrap();
        assert_eq!(missing_twice.models[0].missing_rounds, 2);

        let recovered = registry
            .update_from_opencode_json(r#"{"data":[{"id":"mimo-v2.5-free"}]}"#)
            .unwrap();
        assert_eq!(recovered.models[0].missing_rounds, 0);
    }

    #[test]
    fn records_lifecycle_reasons_for_promotion_rollback_and_retirement() {
        let registry = DynamicModelRegistry::new(true, "url".into());
        registry
            .update_from_opencode_json(r#"{"data":[{"id":"mimo-v2.5-free"}]}"#)
            .unwrap();

        let canary = registry
            .set_model_state(
                "mimo-v2.5-free",
                DiscoveredModelState::Canary,
                "probe quorum met",
            )
            .unwrap();
        assert_eq!(canary.promotion_reason.as_deref(), Some("probe quorum met"));

        let candidate = registry
            .set_model_state(
                "mimo-v2.5-free",
                DiscoveredModelState::Candidate,
                "manual rollback after canary failure",
            )
            .unwrap();
        assert_eq!(
            candidate.rollback_reason.as_deref(),
            Some("manual rollback after canary failure")
        );

        let retired = registry
            .set_model_state(
                "mimo-v2.5-free",
                DiscoveredModelState::Retired,
                "missing beyond grace window",
            )
            .unwrap();
        assert_eq!(
            retired.retirement_reason.as_deref(),
            Some("missing beyond grace window")
        );
    }

    #[test]
    fn records_probe_attempt_success_and_failure_telemetry() {
        let registry = DynamicModelRegistry::new(true, "url".into());
        registry
            .update_from_opencode_json(r#"{"data":[{"id":"mimo-v2.5-free"}]}"#)
            .unwrap();

        let pending = registry
            .record_probe_start("mimo-v2.5-free")
            .expect("probe start record");
        assert_eq!(pending.probe_attempts_total, 1);
        assert!(pending.last_probe_unix.is_some());
        assert_eq!(pending.state, DiscoveredModelState::ProbePending);

        let success = registry
            .record_probe_success("mimo-v2.5-free", "openai_stream_minimal")
            .expect("probe success record");
        assert_eq!(success.probe_success_total, 1);
        assert_eq!(success.consecutive_probe_successes, 1);
        assert_eq!(success.consecutive_probe_failures, 0);
        assert!(success.last_success_unix.is_some());
        assert!(success.last_failure_code.is_none());
        assert!(success.last_failure_message.is_none());

        let failure = registry
            .record_probe_failure(
                "mimo-v2.5-free",
                "provider_empty_output",
                "upstream returned no assistant content or tool call",
            )
            .expect("probe failure record");
        assert_eq!(failure.probe_failure_total, 1);
        assert_eq!(failure.consecutive_probe_successes, 0);
        assert_eq!(failure.consecutive_probe_failures, 1);
        assert_eq!(
            failure.last_failure_code.as_deref(),
            Some("provider_empty_output")
        );
        assert_eq!(
            failure.last_failure_message.as_deref(),
            Some("upstream returned no assistant content or tool call")
        );
    }
}
