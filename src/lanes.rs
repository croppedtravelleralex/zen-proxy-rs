use axum::body::Bytes;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneKind {
    ShortNonStream,
    NormalStream,
    LargeContext,
    HugeContext,
    LongNonStream,
    LongOutput,
    ToolHeavy,
}

#[derive(Debug)]
struct LaneState {
    max: usize,
    in_flight: AtomicUsize,
}

impl LaneState {
    fn new(max: usize) -> Self {
        Self {
            max: max.max(1),
            in_flight: AtomicUsize::new(0),
        }
    }

    fn try_acquire(self: &Arc<Self>, kind: LaneKind) -> Option<LanePermit> {
        let mut current = self.in_flight.load(Ordering::Acquire);
        loop {
            if current >= self.max {
                return None;
            }
            match self.in_flight.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(LanePermit {
                        inner: Arc::new(LanePermitInner {
                            state: self.clone(),
                        }),
                        kind,
                    })
                }
                Err(next) => current = next,
            }
        }
    }

    fn release(&self) {
        self.in_flight.fetch_sub(1, Ordering::AcqRel);
    }

    fn snapshot(&self) -> LaneSnapshot {
        LaneSnapshot {
            max: self.max,
            in_flight: self.in_flight.load(Ordering::Acquire),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct LaneSnapshot {
    pub max: usize,
    pub in_flight: usize,
}

#[derive(Debug, Serialize)]
pub struct LaneLimiterSnapshot {
    pub enabled: bool,
    pub short_nonstream: LaneSnapshot,
    pub normal_stream: LaneSnapshot,
    pub large_context: LaneSnapshot,
    pub huge_context: LaneSnapshot,
    pub long_nonstream: LaneSnapshot,
    pub long_output: LaneSnapshot,
    pub tool_heavy: LaneSnapshot,
}

#[derive(Debug)]
pub struct LaneLimiter {
    enabled: bool,
    short_nonstream: Arc<LaneState>,
    normal_stream: Arc<LaneState>,
    large_context: Arc<LaneState>,
    huge_context: Arc<LaneState>,
    long_nonstream: Arc<LaneState>,
    long_output: Arc<LaneState>,
    tool_heavy: Arc<LaneState>,
}

#[derive(Debug, Clone)]
pub struct LanePermit {
    inner: Arc<LanePermitInner>,
    kind: LaneKind,
}

impl LanePermit {
    pub fn kind(&self) -> LaneKind {
        self.kind
    }
}

#[derive(Debug)]
struct LanePermitInner {
    state: Arc<LaneState>,
}

impl Drop for LanePermitInner {
    fn drop(&mut self) {
        self.state.release();
    }
}

impl LaneLimiter {
    pub fn from_config(config: &Config) -> Self {
        let short_nonstream = if config.v43_lanes_enabled {
            config.v43_short_nonstream_concurrency
        } else {
            config.v1_max_concurrent_requests
        };
        Self {
            enabled: config.v43_lanes_enabled,
            short_nonstream: Arc::new(LaneState::new(short_nonstream)),
            normal_stream: Arc::new(LaneState::new(config.v43_stream_concurrency)),
            large_context: Arc::new(LaneState::new(config.v43_large_context_concurrency)),
            huge_context: Arc::new(LaneState::new(config.v43_huge_context_concurrency)),
            long_nonstream: Arc::new(LaneState::new(config.v46_long_nonstream_concurrency)),
            long_output: Arc::new(LaneState::new(config.v46_long_output_concurrency)),
            tool_heavy: Arc::new(LaneState::new(config.v46_tool_heavy_concurrency)),
        }
    }

    pub async fn acquire(
        &self,
        config: &Config,
        path: &str,
        body: &Bytes,
    ) -> Result<LanePermit, Response> {
        let kind = if self.enabled {
            classify_lane(config, path, body)
        } else {
            LaneKind::ShortNonStream
        };
        let state = self.state_for(kind);
        let mut waited_ms = 0u64;
        loop {
            if let Some(permit) = state.try_acquire(kind) {
                return Ok(permit);
            }
            if waited_ms >= config.v43_lane_wait_timeout_ms {
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    axum::Json(serde_json::json!({
                        "error": {
                            "message": "zenproxy lane is saturated",
                            "lane": kind,
                            "retry_after_ms": 250
                        }
                    })),
                )
                    .into_response());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            waited_ms = waited_ms.saturating_add(10);
        }
    }

    pub fn attach(&self, response: &mut Response, permit: LanePermit) {
        response.headers_mut().insert(
            "x-zen-lane",
            axum::http::HeaderValue::from_static(lane_name(permit.kind())),
        );
        response.extensions_mut().insert(permit);
    }

    pub fn snapshot(&self) -> LaneLimiterSnapshot {
        LaneLimiterSnapshot {
            enabled: self.enabled,
            short_nonstream: self.short_nonstream.snapshot(),
            normal_stream: self.normal_stream.snapshot(),
            large_context: self.large_context.snapshot(),
            huge_context: self.huge_context.snapshot(),
            long_nonstream: self.long_nonstream.snapshot(),
            long_output: self.long_output.snapshot(),
            tool_heavy: self.tool_heavy.snapshot(),
        }
    }

    fn state_for(&self, kind: LaneKind) -> Arc<LaneState> {
        match kind {
            LaneKind::ShortNonStream => self.short_nonstream.clone(),
            LaneKind::NormalStream => self.normal_stream.clone(),
            LaneKind::LargeContext => self.large_context.clone(),
            LaneKind::HugeContext => self.huge_context.clone(),
            LaneKind::LongNonStream => self.long_nonstream.clone(),
            LaneKind::LongOutput => self.long_output.clone(),
            LaneKind::ToolHeavy => self.tool_heavy.clone(),
        }
    }
}

fn classify_lane(config: &Config, path: &str, body: &Bytes) -> LaneKind {
    let body_mb = body.len().div_ceil(1024 * 1024);
    let estimated_tokens = estimate_request_tokens(body);
    if body_mb >= config.v43_huge_context_body_mb.max(1)
        || estimated_tokens >= config.v45_huge_context_tokens.max(1)
    {
        return LaneKind::HugeContext;
    }
    if body_mb >= config.v43_large_context_body_mb.max(1)
        || estimated_tokens >= config.v45_large_context_tokens.max(1)
    {
        return LaneKind::LargeContext;
    }
    let profile = request_profile(body);
    if !matches!(path, "chat/completions" | "messages") {
        return LaneKind::ShortNonStream;
    }
    if profile.tool_heavy {
        return LaneKind::ToolHeavy;
    }
    if !profile.streaming && profile.max_tokens >= config.v46_long_output_tokens.max(1) {
        return LaneKind::LongOutput;
    }
    if !profile.streaming && estimated_tokens >= config.v46_long_nonstream_tokens.max(1) {
        return LaneKind::LongNonStream;
    }
    if profile.streaming {
        LaneKind::NormalStream
    } else {
        LaneKind::ShortNonStream
    }
}

#[derive(Debug, Default)]
struct RequestProfile {
    streaming: bool,
    max_tokens: u64,
    tool_heavy: bool,
}

fn request_profile(body: &Bytes) -> RequestProfile {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return RequestProfile::default();
    };
    let streaming = value
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_tokens = value
        .get("max_tokens")
        .or_else(|| value.get("max_completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let tools_count = value
        .get("tools")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let tool_markers = count_tool_markers(&value);
    RequestProfile {
        streaming,
        max_tokens,
        tool_heavy: tools_count >= 16 || tool_markers >= 12,
    }
}

fn estimate_request_tokens(body: &Bytes) -> u64 {
    if body.is_empty() {
        return 0;
    }
    serde_json::from_slice::<Value>(body)
        .ok()
        .map(|value| estimate_value_tokens(&value))
        .unwrap_or_else(|| (body.len() as u64 / 4).max(1))
}

fn estimate_value_tokens(value: &Value) -> u64 {
    match value {
        Value::String(s) => (s.len() as u64 / 4).max(1),
        Value::Array(items) => items.iter().map(estimate_value_tokens).sum(),
        Value::Object(map) => map
            .iter()
            .filter(|(key, _)| {
                matches!(
                    key.as_str(),
                    "content" | "messages" | "system" | "prompt" | "tools" | "tool_calls"
                )
            })
            .map(|(_, value)| estimate_value_tokens(value))
            .sum(),
        _ => 0,
    }
}

fn count_tool_markers(value: &Value) -> usize {
    match value {
        Value::Array(items) => items.iter().map(count_tool_markers).sum(),
        Value::Object(map) => {
            let here = usize::from(
                map.get("role").and_then(Value::as_str) == Some("tool")
                    || map.contains_key("tool_calls")
                    || map.contains_key("tool_call_id")
                    || map.get("type").and_then(Value::as_str) == Some("tool_result")
                    || map.get("type").and_then(Value::as_str) == Some("tool_use"),
            );
            here + map.values().map(count_tool_markers).sum::<usize>()
        }
        _ => 0,
    }
}

fn lane_name(kind: LaneKind) -> &'static str {
    match kind {
        LaneKind::ShortNonStream => "short_nonstream",
        LaneKind::NormalStream => "normal_stream",
        LaneKind::LargeContext => "large_context",
        LaneKind::HugeContext => "huge_context",
        LaneKind::LongNonStream => "long_nonstream",
        LaneKind::LongOutput => "long_output",
        LaneKind::ToolHeavy => "tool_heavy",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Bytes;

    fn cfg_with_lanes() -> Config {
        let mut cfg = Config::from_env();
        cfg.v43_lanes_enabled = true;
        cfg.v43_large_context_body_mb = 8;
        cfg.v43_huge_context_body_mb = 32;
        cfg.v45_large_context_tokens = 200_000;
        cfg.v45_huge_context_tokens = 500_000;
        cfg.v46_long_nonstream_tokens = 10_000;
        cfg.v46_long_output_tokens = 4_096;
        cfg.v46_tool_heavy_concurrency = 4;
        cfg
    }

    #[test]
    fn token_threshold_routes_large_context_before_body_mb_threshold() {
        let cfg = cfg_with_lanes();
        let content = "x".repeat(820_000);
        let body = Bytes::from(
            serde_json::json!({
                "model": "deepseek-v4-flash",
                "stream": true,
                "messages": [{"role": "user", "content": content}]
            })
            .to_string(),
        );

        assert_eq!(
            classify_lane(&cfg, "chat/completions", &body),
            LaneKind::LargeContext
        );
    }

    #[test]
    fn token_threshold_routes_huge_context_before_body_mb_threshold() {
        let cfg = cfg_with_lanes();
        let content = "x".repeat(2_100_000);
        let body = Bytes::from(
            serde_json::json!({
                "model": "deepseek-v4-flash",
                "stream": true,
                "messages": [{"role": "user", "content": content}]
            })
            .to_string(),
        );

        assert_eq!(
            classify_lane(&cfg, "chat/completions", &body),
            LaneKind::HugeContext
        );
    }

    #[test]
    fn routes_long_nonstream_to_isolated_lane() {
        let cfg = cfg_with_lanes();
        let body = Bytes::from(
            serde_json::json!({
                "model": "deepseek-v4-flash",
                "stream": false,
                "messages": [{"role": "user", "content": "x".repeat(45_000)}]
            })
            .to_string(),
        );

        assert_eq!(
            classify_lane(&cfg, "chat/completions", &body),
            LaneKind::LongNonStream
        );
    }

    #[test]
    fn routes_long_output_to_isolated_lane() {
        let cfg = cfg_with_lanes();
        let body = Bytes::from(
            serde_json::json!({
                "model": "deepseek-v4-flash",
                "stream": false,
                "max_tokens": 8192,
                "messages": [{"role": "user", "content": "hello"}]
            })
            .to_string(),
        );

        assert_eq!(
            classify_lane(&cfg, "chat/completions", &body),
            LaneKind::LongOutput
        );
    }

    #[test]
    fn routes_tool_heavy_to_isolated_lane() {
        let cfg = cfg_with_lanes();
        let messages = (0..12)
            .flat_map(|idx| {
                [
                    serde_json::json!({"role":"assistant","content":null,"tool_calls":[{"id":format!("call_{idx}"),"type":"function","function":{"name":"Read","arguments":"{}"}}]}),
                    serde_json::json!({"role":"tool","tool_call_id":format!("call_{idx}"),"content":"ok"}),
                ]
            })
            .collect::<Vec<_>>();
        let body = Bytes::from(
            serde_json::json!({
                "model": "deepseek-v4-flash",
                "stream": true,
                "messages": messages
            })
            .to_string(),
        );

        assert_eq!(
            classify_lane(&cfg, "chat/completions", &body),
            LaneKind::ToolHeavy
        );
    }

    #[test]
    fn lane_snapshot_exposes_v46_lanes() {
        let cfg = cfg_with_lanes();
        let limiter = LaneLimiter::from_config(&cfg);
        let snapshot = limiter.snapshot();

        assert_eq!(
            snapshot.long_nonstream.max,
            cfg.v46_long_nonstream_concurrency
        );
        assert_eq!(snapshot.long_output.max, cfg.v46_long_output_concurrency);
        assert_eq!(snapshot.tool_heavy.max, cfg.v46_tool_heavy_concurrency);
    }
}
