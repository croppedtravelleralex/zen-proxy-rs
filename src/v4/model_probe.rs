use crate::v4::model_discovery::{
    is_free_candidate, DiscoveredModel, DiscoveredModelState, DynamicModelRegistry,
};
use serde_json::{json, Value};
use std::io::Read;
use std::time::Duration;

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
pub struct HttpProbeConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub timeout_secs: u64,
    pub max_response_bytes: usize,
}

impl HttpProbeConfig {
    fn normalized_base_url(&self) -> Result<String, ModelProbeFailure> {
        let base_url = self.base_url.trim().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err(ModelProbeFailure::soft(
                "probe_http_base_url_missing",
                "DYNAMIC_MODEL_PROBE_BASE_URL is required for http_bounded probes",
            ));
        }
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err(ModelProbeFailure::soft(
                "probe_http_base_url_invalid",
                "DYNAMIC_MODEL_PROBE_BASE_URL must start with http:// or https://",
            ));
        }
        Ok(base_url)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeHttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub timeout_secs: u64,
    pub max_response_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeHttpResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: String,
}

pub trait ProbeHttpTransport {
    fn execute(&self, request: &ProbeHttpRequest) -> Result<ProbeHttpResponse, ModelProbeFailure>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ReqwestBlockingProbeTransport;

impl ProbeHttpTransport for ReqwestBlockingProbeTransport {
    fn execute(&self, request: &ProbeHttpRequest) -> Result<ProbeHttpResponse, ModelProbeFailure> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(request.timeout_secs.max(1)))
            .build()
            .map_err(|err| {
                ModelProbeFailure::soft(
                    "probe_http_client_build_failed",
                    format!("failed to build probe http client: {err}"),
                )
            })?;
        let method = request.method.parse().map_err(|err| {
            ModelProbeFailure::soft(
                "probe_http_method_invalid",
                format!("invalid probe http method: {err}"),
            )
        })?;
        let mut builder = client.request(method, &request.url);
        for (key, value) in &request.headers {
            builder = builder.header(key, value);
        }
        let mut response = builder.body(request.body.clone()).send().map_err(|err| {
            ModelProbeFailure::soft(
                "probe_http_request_failed",
                format!("probe http request failed: {err}"),
            )
        })?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        let limit = request.max_response_bytes.max(1024);
        let mut body = String::new();
        response
            .by_ref()
            .take(limit.saturating_add(1) as u64)
            .read_to_string(&mut body)
            .map_err(|err| {
                ModelProbeFailure::soft(
                    "probe_http_response_read_failed",
                    format!("failed to read probe http response: {err}"),
                )
            })?;
        if body.len() > limit {
            return Err(ModelProbeFailure::soft(
                "probe_http_response_too_large",
                format!("probe response exceeded {limit} bytes"),
            ));
        }
        Ok(ProbeHttpResponse {
            status,
            content_type,
            body,
        })
    }
}

#[derive(Debug, Clone)]
pub struct BoundedHttpProbeAdapter<T> {
    config: HttpProbeConfig,
    transport: T,
}

impl<T> BoundedHttpProbeAdapter<T> {
    pub fn new(config: HttpProbeConfig, transport: T) -> Self {
        Self { config, transport }
    }
}

impl<T: ProbeHttpTransport> ModelProbeAdapter for BoundedHttpProbeAdapter<T> {
    fn run_probe(&self, model: &DiscoveredModel, probe_name: &str) -> ModelProbeOutcome {
        let request = match build_probe_request(model, probe_name, &self.config) {
            Ok(None) => return ModelProbeOutcome::Passed,
            Ok(Some(request)) => request,
            Err(failure) => return ModelProbeOutcome::Failed(failure.for_probe(probe_name)),
        };
        match self.transport.execute(&request) {
            Ok(response) => classify_probe_response(probe_name, &response),
            Err(failure) => ModelProbeOutcome::Failed(failure.for_probe(probe_name)),
        }
    }
}

fn build_probe_request(
    model: &DiscoveredModel,
    probe_name: &str,
    config: &HttpProbeConfig,
) -> Result<Option<ProbeHttpRequest>, ModelProbeFailure> {
    if probe_name == "metadata" {
        if is_free_candidate(&model.upstream_id) && is_free_candidate(&model.id) {
            return Ok(None);
        }
        return Err(ModelProbeFailure::hard_protocol(
            "probe_metadata_rejected",
            "model id is not a free-model candidate",
        ));
    }

    let base_url = config.normalized_base_url()?;
    let timeout_secs = config.timeout_secs.max(1);
    let max_response_bytes = config.max_response_bytes.max(1024);
    let mut headers = vec![
        ("content-type".to_string(), "application/json".to_string()),
        (
            "user-agent".to_string(),
            "zen-proxy-rs-model-probe/4.109".to_string(),
        ),
        ("x-zen-model-probe".to_string(), probe_name.to_string()),
    ];
    if let Some(api_key) = config.api_key.as_deref().filter(|key| !key.is_empty()) {
        headers.push(("authorization".to_string(), format!("Bearer {api_key}")));
        headers.push(("x-api-key".to_string(), api_key.to_string()));
    }

    let (path, body) = match probe_name {
        "openai_nonstream_minimal" => (
            "/v1/chat/completions",
            openai_minimal_body(&model.upstream_id, false),
        ),
        "openai_stream_minimal" => (
            "/v1/chat/completions",
            openai_minimal_body(&model.upstream_id, true),
        ),
        "anthropic_nonstream_minimal" => {
            headers.push(("anthropic-version".to_string(), "2023-06-01".to_string()));
            (
                "/v1/messages",
                anthropic_minimal_body(&model.upstream_id, false),
            )
        }
        "anthropic_stream_minimal" => {
            headers.push(("anthropic-version".to_string(), "2023-06-01".to_string()));
            (
                "/v1/messages",
                anthropic_minimal_body(&model.upstream_id, true),
            )
        }
        "tool_history_minimal" => {
            headers.push(("x-fmc-client".to_string(), "claude-code".to_string()));
            (
                "/v1/chat/completions",
                openai_tool_history_body(&model.upstream_id),
            )
        }
        "empty_output_guard" => (
            "/v1/chat/completions",
            openai_guard_body(
                &model.upstream_id,
                "Reply with exactly: probe-output-present",
                false,
            ),
        ),
        "format_guard" => (
            "/v1/chat/completions",
            openai_guard_body(
                &model.upstream_id,
                "Reply with valid JSON: {\"ok\":true}",
                false,
            ),
        ),
        other => {
            return Err(ModelProbeFailure::soft(
                "probe_unknown_name",
                format!("unknown probe name: {other}"),
            ));
        }
    };

    Ok(Some(ProbeHttpRequest {
        method: "POST".to_string(),
        url: format!("{base_url}{path}"),
        headers,
        body: body.to_string(),
        timeout_secs,
        max_response_bytes,
    }))
}

fn openai_minimal_body(model: &str, stream: bool) -> Value {
    openai_guard_body(model, "Reply with exactly: probe-ok", stream)
}

fn openai_guard_body(model: &str, prompt: &str, stream: bool) -> Value {
    json!({
        "model": model,
        "stream": stream,
        "max_tokens": 16,
        "temperature": 0,
        "messages": [{"role": "user", "content": prompt}]
    })
}

fn anthropic_minimal_body(model: &str, stream: bool) -> Value {
    json!({
        "model": model,
        "stream": stream,
        "max_tokens": 16,
        "temperature": 0,
        "messages": [{"role": "user", "content": "Reply with exactly: probe-ok"}]
    })
}

fn openai_tool_history_body(model: &str) -> Value {
    json!({
        "model": model,
        "stream": false,
        "max_tokens": 32,
        "temperature": 0,
        "messages": [
            {"role": "user", "content": "Use the existing tool result and answer with probe-ok."},
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_probe_1",
                    "type": "function",
                    "function": {
                        "name": "probe_lookup",
                        "arguments": "{\"value\":\"probe-ok\"}"
                    }
                }]
            },
            {
                "role": "tool",
                "tool_call_id": "call_probe_1",
                "content": "{\"value\":\"probe-ok\"}"
            },
            {"role": "user", "content": "Return the tool value only."}
        ],
        "tools": [{
            "type": "function",
            "function": {
                "name": "probe_lookup",
                "description": "Returns a fixed probe value.",
                "parameters": {
                    "type": "object",
                    "properties": {"value": {"type": "string"}},
                    "required": ["value"]
                }
            }
        }]
    })
}

fn classify_probe_response(probe_name: &str, response: &ProbeHttpResponse) -> ModelProbeOutcome {
    if !(200..300).contains(&response.status) {
        return ModelProbeOutcome::Failed(classify_http_error(response).for_probe(probe_name));
    }

    let outcome = match probe_name {
        "openai_stream_minimal" => classify_openai_stream_body(&response.body),
        "anthropic_stream_minimal" => classify_anthropic_stream_body(&response.body),
        "anthropic_nonstream_minimal" => classify_anthropic_json_body(&response.body),
        "openai_nonstream_minimal"
        | "tool_history_minimal"
        | "empty_output_guard"
        | "format_guard" => classify_openai_json_body(&response.body),
        _ => Err(ModelProbeFailure::soft(
            "probe_unknown_name",
            format!("unknown probe name: {probe_name}"),
        )),
    };
    match outcome {
        Ok(()) => ModelProbeOutcome::Passed,
        Err(failure) => ModelProbeOutcome::Failed(failure.for_probe(probe_name)),
    }
}

fn classify_http_error(response: &ProbeHttpResponse) -> ModelProbeFailure {
    let message = extract_error_message(&response.body).unwrap_or_else(|| {
        format!(
            "probe upstream returned HTTP {} with {} bytes",
            response.status,
            response.body.len()
        )
    });
    let lower = message.to_lowercase();
    if lower.contains("missing field tool_call_id")
        || lower.contains("missing field tool_use_id")
        || lower.contains("invalid assistant message")
        || lower.contains("reasoning_content")
        || lower.contains("deserialize")
        || lower.contains("failed to parse json")
    {
        return ModelProbeFailure::hard_protocol("probe_hard_protocol_error", message);
    }
    if lower.contains("no assistant content or tool call")
        || lower.contains("empty assistant")
        || lower.contains("empty output")
    {
        return ModelProbeFailure::soft("provider_empty_output", message);
    }
    if response.status == 400 || response.status == 422 {
        return ModelProbeFailure::hard_protocol("probe_invalid_request", message);
    }
    ModelProbeFailure::soft(format!("probe_http_status_{}", response.status), message)
}

fn extract_error_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| value.get("message").and_then(Value::as_str))
        .map(ToOwned::to_owned)
}

fn classify_openai_json_body(body: &str) -> Result<(), ModelProbeFailure> {
    let value: Value = serde_json::from_str(body).map_err(|err| {
        ModelProbeFailure::hard_protocol(
            "probe_invalid_openai_json",
            format!("invalid OpenAI JSON response: {err}"),
        )
    })?;
    let has_content = value
        .get("choices")
        .and_then(Value::as_array)
        .map(|choices| {
            choices.iter().any(|choice| {
                let message = choice.get("message").unwrap_or(choice);
                has_non_empty_string(message.pointer("/content"))
                    || message
                        .get("tool_calls")
                        .and_then(Value::as_array)
                        .map(|items| !items.is_empty())
                        .unwrap_or(false)
                    || has_non_empty_string(choice.pointer("/delta/content"))
                    || choice
                        .pointer("/delta/tool_calls")
                        .and_then(Value::as_array)
                        .map(|items| !items.is_empty())
                        .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    if has_content {
        Ok(())
    } else {
        Err(ModelProbeFailure::soft(
            "provider_empty_output",
            "OpenAI response had no assistant content or tool call",
        ))
    }
}

fn classify_anthropic_json_body(body: &str) -> Result<(), ModelProbeFailure> {
    let value: Value = serde_json::from_str(body).map_err(|err| {
        ModelProbeFailure::hard_protocol(
            "probe_invalid_anthropic_json",
            format!("invalid Anthropic JSON response: {err}"),
        )
    })?;
    let has_content = value
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items.iter().any(|item| {
                has_non_empty_string(item.get("text"))
                    || item.get("type").and_then(Value::as_str) == Some("tool_use")
            })
        })
        .unwrap_or(false);
    if has_content {
        Ok(())
    } else {
        Err(ModelProbeFailure::soft(
            "provider_empty_output",
            "Anthropic response had no content block",
        ))
    }
}

fn classify_openai_stream_body(body: &str) -> Result<(), ModelProbeFailure> {
    let mut saw_data = false;
    for line in body.lines() {
        let line = line.trim();
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            continue;
        }
        saw_data = true;
        let value: Value = serde_json::from_str(data).map_err(|err| {
            ModelProbeFailure::hard_protocol(
                "probe_invalid_openai_sse",
                format!("invalid OpenAI SSE JSON chunk: {err}"),
            )
        })?;
        if stream_value_has_openai_content(&value) {
            return Ok(());
        }
    }
    Err(ModelProbeFailure::soft(
        "provider_empty_output",
        if saw_data {
            "OpenAI stream ended without content or tool call"
        } else {
            "OpenAI stream had no SSE data chunks"
        },
    ))
}

fn classify_anthropic_stream_body(body: &str) -> Result<(), ModelProbeFailure> {
    let mut saw_data = false;
    for line in body.lines() {
        let line = line.trim();
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" || data.is_empty() {
            continue;
        }
        saw_data = true;
        let value: Value = serde_json::from_str(data).map_err(|err| {
            ModelProbeFailure::hard_protocol(
                "probe_invalid_anthropic_sse",
                format!("invalid Anthropic SSE JSON chunk: {err}"),
            )
        })?;
        if has_non_empty_string(value.pointer("/delta/text"))
            || value.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use")
        {
            return Ok(());
        }
    }
    Err(ModelProbeFailure::soft(
        "provider_empty_output",
        if saw_data {
            "Anthropic stream ended without content or tool call"
        } else {
            "Anthropic stream had no SSE data chunks"
        },
    ))
}

fn stream_value_has_openai_content(value: &Value) -> bool {
    value
        .get("choices")
        .and_then(Value::as_array)
        .map(|choices| {
            choices.iter().any(|choice| {
                has_non_empty_string(choice.pointer("/delta/content"))
                    || choice
                        .pointer("/delta/tool_calls")
                        .and_then(Value::as_array)
                        .map(|items| !items.is_empty())
                        .unwrap_or(false)
                    || has_non_empty_string(choice.pointer("/message/content"))
            })
        })
        .unwrap_or(false)
}

fn has_non_empty_string(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .map(|text| !text.is_empty())
        .unwrap_or(false)
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

    #[derive(Debug, Clone)]
    struct RecordingHttpTransport {
        response: ProbeHttpResponse,
        requests: std::sync::Arc<std::sync::Mutex<Vec<ProbeHttpRequest>>>,
    }

    impl RecordingHttpTransport {
        fn new(response: ProbeHttpResponse) -> Self {
            Self {
                response,
                requests: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }
    }

    impl ProbeHttpTransport for RecordingHttpTransport {
        fn execute(
            &self,
            request: &ProbeHttpRequest,
        ) -> Result<ProbeHttpResponse, ModelProbeFailure> {
            self.requests.lock().unwrap().push(request.clone());
            Ok(self.response.clone())
        }
    }

    fn http_probe_config() -> HttpProbeConfig {
        HttpProbeConfig {
            base_url: "http://127.0.0.1:4010".to_string(),
            api_key: Some("probe-key".to_string()),
            timeout_secs: 3,
            max_response_bytes: 4096,
        }
    }

    fn openai_content_response() -> ProbeHttpResponse {
        ProbeHttpResponse {
            status: 200,
            content_type: Some("application/json".to_string()),
            body: r#"{"choices":[{"message":{"role":"assistant","content":"probe-ok"}}]}"#
                .to_string(),
        }
    }

    #[test]
    fn http_adapter_builds_claudecode_tool_history_probe() {
        let registry = registry_with_models();
        let model = registry.get("good-free").unwrap();
        let transport = RecordingHttpTransport::new(openai_content_response());
        let requests = transport.requests.clone();
        let adapter = BoundedHttpProbeAdapter::new(http_probe_config(), transport);

        let outcome = adapter.run_probe(&model, "tool_history_minimal");
        assert_eq!(outcome, ModelProbeOutcome::Passed);

        let captured = requests.lock().unwrap();
        assert_eq!(captured.len(), 1);
        let request = &captured[0];
        assert_eq!(request.method, "POST");
        assert!(request.url.ends_with("/v1/chat/completions"));
        assert!(request
            .headers
            .iter()
            .any(|(key, value)| key == "authorization" && value == "Bearer probe-key"));
        assert!(request
            .headers
            .iter()
            .any(|(key, value)| key == "x-fmc-client" && value == "claude-code"));
        let body: Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(body["model"], "good-free");
        assert_eq!(body["messages"][2]["role"], "tool");
        assert_eq!(body["messages"][2]["tool_call_id"], "call_probe_1");
    }

    #[test]
    fn http_adapter_accepts_openai_stream_real_content() {
        let registry = registry_with_models();
        let model = registry.get("good-free").unwrap();
        let transport = RecordingHttpTransport::new(ProbeHttpResponse {
            status: 200,
            content_type: Some("text/event-stream".to_string()),
            body: "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"p\"}}]}\n\ndata: [DONE]\n\n".to_string(),
        });
        let adapter = BoundedHttpProbeAdapter::new(http_probe_config(), transport);

        assert_eq!(
            adapter.run_probe(&model, "openai_stream_minimal"),
            ModelProbeOutcome::Passed
        );
    }

    #[test]
    fn http_adapter_hard_fails_provider_protocol_errors() {
        let registry = registry_with_models();
        let model = registry.get("good-free").unwrap();
        let transport = RecordingHttpTransport::new(ProbeHttpResponse {
            status: 400,
            content_type: Some("application/json".to_string()),
            body: r#"{"error":{"message":"messages[2]: missing field tool_call_id"}}"#.to_string(),
        });
        let adapter = BoundedHttpProbeAdapter::new(http_probe_config(), transport);

        match adapter.run_probe(&model, "tool_history_minimal") {
            ModelProbeOutcome::Failed(failure) => {
                assert_eq!(failure.probe_name.as_deref(), Some("tool_history_minimal"));
                assert_eq!(failure.code, "probe_hard_protocol_error");
                assert!(failure.hard_protocol_failure);
            }
            ModelProbeOutcome::Passed => panic!("hard protocol error must fail"),
        }
    }

    #[test]
    fn http_adapter_requires_explicit_base_url() {
        let registry = registry_with_models();
        let model = registry.get("good-free").unwrap();
        let adapter = BoundedHttpProbeAdapter::new(
            HttpProbeConfig {
                base_url: String::new(),
                api_key: None,
                timeout_secs: 3,
                max_response_bytes: 4096,
            },
            RecordingHttpTransport::new(openai_content_response()),
        );

        match adapter.run_probe(&model, "openai_nonstream_minimal") {
            ModelProbeOutcome::Failed(failure) => {
                assert_eq!(
                    failure.probe_name.as_deref(),
                    Some("openai_nonstream_minimal")
                );
                assert_eq!(failure.code, "probe_http_base_url_missing");
                assert!(!failure.hard_protocol_failure);
            }
            ModelProbeOutcome::Passed => panic!("missing base URL must fail"),
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
