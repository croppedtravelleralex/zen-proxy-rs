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
}

#[derive(Debug)]
pub struct LaneLimiter {
    enabled: bool,
    short_nonstream: Arc<LaneState>,
    normal_stream: Arc<LaneState>,
    large_context: Arc<LaneState>,
    huge_context: Arc<LaneState>,
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
        }
    }

    fn state_for(&self, kind: LaneKind) -> Arc<LaneState> {
        match kind {
            LaneKind::ShortNonStream => self.short_nonstream.clone(),
            LaneKind::NormalStream => self.normal_stream.clone(),
            LaneKind::LargeContext => self.large_context.clone(),
            LaneKind::HugeContext => self.huge_context.clone(),
        }
    }
}

fn classify_lane(config: &Config, path: &str, body: &Bytes) -> LaneKind {
    let body_mb = body.len().div_ceil(1024 * 1024);
    if body_mb >= config.v43_huge_context_body_mb.max(1) {
        return LaneKind::HugeContext;
    }
    if body_mb >= config.v43_large_context_body_mb.max(1) {
        return LaneKind::LargeContext;
    }
    if matches!(path, "chat/completions" | "messages") && request_is_streaming(body) {
        LaneKind::NormalStream
    } else {
        LaneKind::ShortNonStream
    }
}

fn request_is_streaming(body: &Bytes) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.get("stream").and_then(Value::as_bool))
        .unwrap_or(false)
}

fn lane_name(kind: LaneKind) -> &'static str {
    match kind {
        LaneKind::ShortNonStream => "short_nonstream",
        LaneKind::NormalStream => "normal_stream",
        LaneKind::LargeContext => "large_context",
        LaneKind::HugeContext => "huge_context",
    }
}
