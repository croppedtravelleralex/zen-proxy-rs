use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveredModelState {
    Candidate,
    Ignored,
    Missing,
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
                });
            entry.state = state;
            entry.reason = reason;
            entry.last_seen_unix = now;
            entry.probe_required = matches!(entry.state, DiscoveredModelState::Candidate);
            entry.auto_promoted = false;
        }

        for model in merged.values_mut() {
            if !seen_this_round.contains(&model.id) {
                model.state = DiscoveredModelState::Missing;
                model.reason =
                    "previously discovered model is absent from the latest upstream list"
                        .to_string();
                model.probe_required = true;
                model.auto_promoted = false;
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
}
