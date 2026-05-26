use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::RwLock;

use crate::pool::global_budget::GlobalBudgetRegistry;
use crate::pool::*;
use serde_json::{json, Value};

const SCORE_SCALE: u64 = 100;
const INITIAL_BASE_SCORE: u64 = 20 * SCORE_SCALE;
const SUCCESS_SCORE_STEP: u64 = 25 * SCORE_SCALE;
const FAILURE_SCORE_PENALTY: u64 = 60 * SCORE_SCALE;
const DEFAULT_MAX_CALLS_PER_WINDOW: u64 = 100;
const DEFAULT_MAX_TOKENS_PER_WINDOW: u64 = 10_000_000;
const DEFAULT_MAX_KB_PER_WINDOW: u64 = 64 * 1024;
const DEFAULT_COOLDOWN_SECS: i64 = 60;
const DEFAULT_DISPATCH_SHARDS: usize = 16;

#[derive(Debug, Clone)]
pub struct AimdConfig {
    pub min_concurrent: u32,
    pub max_concurrent: u32,
    pub success_step: u32,
    pub failure_percent: u32,
    pub slow_latency_ms: u64,
}

impl Default for AimdConfig {
    fn default() -> Self {
        Self {
            min_concurrent: 1,
            max_concurrent: 16,
            success_step: 1,
            failure_percent: 50,
            slow_latency_ms: 30_000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NodeBudgetLimits {
    pub max_calls_per_window: u64,
    pub max_tokens_per_window: u64,
    pub max_kb_per_window: u64,
    pub cooldown_secs: i64,
}

impl Default for NodeBudgetLimits {
    fn default() -> Self {
        Self {
            max_calls_per_window: DEFAULT_MAX_CALLS_PER_WINDOW,
            max_tokens_per_window: DEFAULT_MAX_TOKENS_PER_WINDOW,
            max_kb_per_window: DEFAULT_MAX_KB_PER_WINDOW,
            cooldown_secs: DEFAULT_COOLDOWN_SECS,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NodeBudgetSnapshot {
    pub node_id: String,
    pub node_state: String,
    pub calls_in_window: u64,
    pub tokens_in_window: u64,
    pub kb_in_window: u64,
    pub concurrent_now: u32,
    pub max_concurrent: u32,
    pub cooldown_until: Option<i64>,
    pub budget_hit_reason: Option<String>,
}

#[derive(Debug, Clone)]
struct NodeBudget {
    calls_in_window: u64,
    tokens_in_window: u64,
    kb_in_window: u64,
    max_calls_per_window: u64,
    max_tokens_per_window: u64,
    max_kb_per_window: u64,
    cooldown_secs: i64,
    cooldown_until: Option<i64>,
    budget_hit_reason: Option<String>,
}

impl From<NodeBudgetLimits> for NodeBudget {
    fn from(limits: NodeBudgetLimits) -> Self {
        Self {
            calls_in_window: 0,
            tokens_in_window: 0,
            kb_in_window: 0,
            max_calls_per_window: limits.max_calls_per_window,
            max_tokens_per_window: limits.max_tokens_per_window,
            max_kb_per_window: limits.max_kb_per_window,
            cooldown_secs: limits.cooldown_secs,
            cooldown_until: None,
            budget_hit_reason: None,
        }
    }
}

impl NodeBudget {
    fn can_admit(
        &self,
        meta: &RequestMeta,
        now: i64,
        concurrent_now: u32,
        max_concurrent: u32,
    ) -> Result<(), String> {
        if let Some(until) = self.cooldown_until {
            if until > now {
                return Err("cooldown".to_string());
            }
        }
        if concurrent_now >= max_concurrent {
            return Err("max_concurrent".to_string());
        }
        if self.calls_in_window.saturating_add(1) > self.max_calls_per_window {
            return Err("max_calls".to_string());
        }
        if self
            .tokens_in_window
            .saturating_add(meta.estimated_input_tokens())
            > self.max_tokens_per_window
        {
            return Err("max_tokens".to_string());
        }
        if self.kb_in_window.saturating_add(meta.request_kb()) > self.max_kb_per_window {
            return Err("max_kb".to_string());
        }
        Ok(())
    }

    fn admit(&mut self, meta: &RequestMeta) {
        self.calls_in_window = self.calls_in_window.saturating_add(1);
        self.tokens_in_window = self
            .tokens_in_window
            .saturating_add(meta.estimated_input_tokens());
        self.kb_in_window = self.kb_in_window.saturating_add(meta.request_kb());
        self.budget_hit_reason = None;
    }

    fn rollback_admit(&mut self, meta: &RequestMeta) {
        self.calls_in_window = self.calls_in_window.saturating_sub(1);
        self.tokens_in_window = self
            .tokens_in_window
            .saturating_sub(meta.estimated_input_tokens());
        self.kb_in_window = self.kb_in_window.saturating_sub(meta.request_kb());
    }

    fn cooldown(&mut self, now: i64, reason: impl Into<String>) {
        self.cooldown_until = Some(now + self.cooldown_secs);
        self.budget_hit_reason = Some(reason.into());
    }

    fn clear_expired_cooldown(&mut self, now: i64) {
        if self.cooldown_until.is_some_and(|until| until <= now) {
            self.cooldown_until = None;
            self.budget_hit_reason = None;
        }
    }
}

struct PoolNode {
    node: NodeRef,
    base_score: AtomicU64,
    consecutive_successes: AtomicU32,
    recent_results: RwLock<VecDeque<bool>>,
    avg_latency_ms: AtomicU64,
    idle_since: AtomicI64,
    max_concurrent: AtomicU32,
    active_leases: AtomicU32,
    budget: RwLock<NodeBudget>,
}

impl PoolNode {
    fn new(node: NodeRef, limits: NodeBudgetLimits, aimd: &AimdConfig) -> Self {
        Self {
            node,
            base_score: AtomicU64::new(INITIAL_BASE_SCORE),
            consecutive_successes: AtomicU32::new(0),
            recent_results: RwLock::new(VecDeque::with_capacity(20)),
            avg_latency_ms: AtomicU64::new(0),
            idle_since: AtomicI64::new(chrono::Utc::now().timestamp()),
            max_concurrent: AtomicU32::new(5u32.clamp(
                aimd.min_concurrent.max(1),
                aimd.max_concurrent.max(aimd.min_concurrent.max(1)),
            )),
            active_leases: AtomicU32::new(0),
            budget: RwLock::new(NodeBudget::from(limits)),
        }
    }

    fn score(&self) -> f64 {
        self.score_for(None)
    }

    fn score_for(&self, meta: Option<&RequestMeta>) -> f64 {
        let large_stream = meta.is_some_and(|m| m.stream && m.body_size >= 128 * 1024);
        let very_large_stream = meta.is_some_and(|m| m.stream && m.body_size >= 512 * 1024);
        let health_weight = if very_large_stream {
            0.30
        } else if large_stream {
            0.35
        } else {
            0.50
        };
        let success_weight = 0.20;
        let idle_weight = if large_stream { 0.10 } else { 0.15 };
        let latency_weight = if very_large_stream {
            0.30
        } else if large_stream {
            0.25
        } else {
            0.10
        };
        let momentum_weight = if large_stream { 0.10 } else { 0.05 };

        let base_pct = self.base_score.load(Ordering::Relaxed) as f64 / SCORE_SCALE as f64;
        let health = (base_pct / 100.0).clamp(0.0, 1.0) * health_weight;

        let recent = self.recent_results.read().unwrap();
        let total = recent.len();
        let successes = recent.iter().filter(|&&r| r).count();
        let success_rate = if total > 0 {
            successes as f64 / total as f64 * success_weight
        } else {
            0.0
        };
        drop(recent);

        let now = chrono::Utc::now().timestamp();
        let idle_secs = now - self.idle_since.load(Ordering::Relaxed);
        let idle_factor = (idle_secs as f64 / 60.0).min(1.0) * idle_weight;

        let avg_lat = self.avg_latency_ms.load(Ordering::Relaxed) as f64;
        let latency_factor = (1.0 - (avg_lat / 5000.0).min(1.0)).max(0.0) * latency_weight;

        let consec = self.consecutive_successes.load(Ordering::Relaxed) as f64;
        let momentum = (consec / 10.0).min(1.0) * momentum_weight;

        health + success_rate + idle_factor + latency_factor + momentum
    }

    fn record_result(&self, success: bool, latency_ms: u64, aimd: &AimdConfig) {
        {
            let mut recent = self.recent_results.write().unwrap();
            recent.push_back(success);
            while recent.len() > 20 {
                recent.pop_front();
            }
        }

        self.record_latency(latency_ms);

        if success {
            self.raise_base_score();
            let prev = self.consecutive_successes.fetch_add(1, Ordering::Relaxed);
            if prev.saturating_add(1) % 3 == 0 {
                let _ =
                    self.max_concurrent
                        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                            Some(
                                value
                                    .saturating_add(aimd.success_step.max(1))
                                    .min(aimd.max_concurrent.max(aimd.min_concurrent)),
                            )
                        });
            }
            if latency_ms >= aimd.slow_latency_ms && aimd.slow_latency_ms > 0 {
                self.reduce_concurrency(aimd);
            }
        } else {
            self.consecutive_successes.store(0, Ordering::Relaxed);
            self.lower_base_score();
            self.reduce_concurrency(aimd);
        }
    }

    fn record_latency(&self, latency_ms: u64) {
        if latency_ms == 0 {
            return;
        }
        let _ = self
            .avg_latency_ms
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                Some(if current == 0 {
                    latency_ms
                } else {
                    current.saturating_mul(3).saturating_add(latency_ms) / 4
                })
            });
    }

    fn raise_base_score(&self) {
        let _ = self
            .base_score
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                Some(
                    value
                        .saturating_add(SUCCESS_SCORE_STEP)
                        .min(100 * SCORE_SCALE),
                )
            });
    }

    fn lower_base_score(&self) {
        let _ = self
            .base_score
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                Some(value.saturating_sub(FAILURE_SCORE_PENALTY))
            });
    }

    fn reduce_concurrency(&self, aimd: &AimdConfig) {
        let failure_percent = aimd.failure_percent.clamp(1, 99);
        let _ = self
            .max_concurrent
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                let reduced = value.saturating_mul(failure_percent) / 100;
                Some(reduced.max(aimd.min_concurrent.max(1)))
            });
    }

    fn try_admit(&self, meta: &RequestMeta, now: i64) -> bool {
        let concurrent_now = self.active_leases.load(Ordering::Relaxed);
        let max_concurrent = self.max_concurrent.load(Ordering::Relaxed);
        let mut budget = self.budget.write().unwrap();
        budget.clear_expired_cooldown(now);
        match budget.can_admit(meta, now, concurrent_now, max_concurrent) {
            Ok(()) => {
                budget.admit(meta);
                self.active_leases.fetch_add(1, Ordering::SeqCst);
                true
            }
            Err(reason) => {
                if matches!(reason.as_str(), "max_calls" | "max_tokens" | "max_kb") {
                    budget.cooldown(now, reason);
                } else {
                    budget.budget_hit_reason = Some(reason);
                }
                false
            }
        }
    }

    fn release_lease(&self) {
        let _ = self
            .active_leases
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                Some(value.saturating_sub(1))
            });
    }

    fn rollback_local_admit(&self, meta: &RequestMeta) {
        self.release_lease();
        self.budget.write().unwrap().rollback_admit(meta);
    }

    fn snapshot(&self) -> NodeBudgetSnapshot {
        let budget = self.budget.read().unwrap();
        let now = chrono::Utc::now().timestamp();
        let cooldown_active = budget.cooldown_until.is_some_and(|until| until > now);
        NodeBudgetSnapshot {
            node_id: self.node.id.clone(),
            node_state: if cooldown_active {
                "cooldown".to_string()
            } else {
                "dispatch".to_string()
            },
            calls_in_window: budget.calls_in_window,
            tokens_in_window: budget.tokens_in_window,
            kb_in_window: budget.kb_in_window,
            concurrent_now: self.active_leases.load(Ordering::Relaxed),
            max_concurrent: self.max_concurrent.load(Ordering::Relaxed),
            cooldown_until: budget.cooldown_until,
            budget_hit_reason: budget.budget_hit_reason.clone(),
        }
    }

    fn detail(&self, global_budget: Option<&GlobalBudgetRegistry>) -> Value {
        let snapshot = self.snapshot();
        let global = global_budget
            .map(|registry| registry.snapshot(&self.node.id))
            .unwrap_or_default();
        json!({
            "node_id": snapshot.node_id,
            "node_url_redacted": crate::ledger::LedgerEvent::redact_node_url(&self.node.url),
            "state": snapshot.node_state,
            "score": self.score(),
            "base_score": self.base_score.load(Ordering::Relaxed) as f64 / SCORE_SCALE as f64,
            "consecutive_successes": self.consecutive_successes.load(Ordering::Relaxed),
            "recent_success_rate": self.recent_success_rate(),
            "avg_latency_ms": self.avg_latency_ms.load(Ordering::Relaxed),
            "idle_secs": chrono::Utc::now().timestamp().saturating_sub(self.idle_since.load(Ordering::Relaxed)),
            "local_budget": {
                "calls_in_window": snapshot.calls_in_window,
                "tokens_in_window": snapshot.tokens_in_window,
                "kb_in_window": snapshot.kb_in_window,
                "concurrent_now": snapshot.concurrent_now,
                "max_concurrent": snapshot.max_concurrent,
                "cooldown_until": snapshot.cooldown_until,
                "budget_hit_reason": snapshot.budget_hit_reason,
            },
            "global_budget": global,
        })
    }

    fn recent_success_rate(&self) -> f64 {
        let recent = self.recent_results.read().unwrap();
        if recent.is_empty() {
            return 0.0;
        }
        let successes = recent.iter().filter(|&&value| value).count();
        successes as f64 / recent.len() as f64
    }
}

struct DispatchShard {
    nodes: RwLock<Vec<PoolNode>>,
}

impl DispatchShard {
    fn new() -> Self {
        Self {
            nodes: RwLock::new(Vec::new()),
        }
    }
}

pub struct DispatchPool {
    shards: Vec<DispatchShard>,
    idle_since: AtomicI64,
    budget_limits: NodeBudgetLimits,
    global_budget: Option<GlobalBudgetRegistry>,
    global_budget_fail_open: bool,
    aimd: AimdConfig,
}

impl DispatchPool {
    pub fn new() -> Self {
        Self::new_with_limits(NodeBudgetLimits::default())
    }

    pub fn new_with_limits(budget_limits: NodeBudgetLimits) -> Self {
        Self::new_with_options(
            budget_limits,
            AimdConfig::default(),
            DEFAULT_DISPATCH_SHARDS,
        )
    }

    pub fn new_with_options(
        budget_limits: NodeBudgetLimits,
        aimd: AimdConfig,
        shard_count: usize,
    ) -> Self {
        let shard_count = shard_count.clamp(1, 128);
        Self {
            shards: (0..shard_count).map(|_| DispatchShard::new()).collect(),
            idle_since: AtomicI64::new(chrono::Utc::now().timestamp()),
            budget_limits,
            global_budget: None,
            global_budget_fail_open: true,
            aimd,
        }
    }

    pub fn with_global_budget(mut self, global_budget: GlobalBudgetRegistry) -> Self {
        self.global_budget = Some(global_budget);
        self
    }

    pub fn with_global_budget_fail_open(mut self, fail_open: bool) -> Self {
        self.global_budget_fail_open = fail_open;
        self
    }

    pub fn budget_snapshots(&self) -> Vec<NodeBudgetSnapshot> {
        let mut snapshots = Vec::new();
        for shard in &self.shards {
            snapshots.extend(shard.nodes.read().unwrap().iter().map(PoolNode::snapshot));
        }
        snapshots
    }

    pub fn budget_counts(&self) -> (usize, usize, usize) {
        let snapshots = self.budget_snapshots();
        let cooldown_size = snapshots
            .iter()
            .filter(|snapshot| snapshot.node_state == "cooldown")
            .count();
        let budget_limited_size = snapshots
            .iter()
            .filter(|snapshot| snapshot.budget_hit_reason.is_some())
            .count();
        let leased_count = snapshots
            .iter()
            .map(|snapshot| snapshot.concurrent_now as usize)
            .sum();
        (cooldown_size, budget_limited_size, leased_count)
    }

    fn global_admit(&self, node: &PoolNode, meta: &RequestMeta) -> bool {
        let Some(registry) = &self.global_budget else {
            return true;
        };
        match registry.try_acquire(&node.node.id, meta) {
            Ok(_) => true,
            Err(reason) => {
                let mut budget = node.budget.write().unwrap();
                if Self::is_global_budget_rejection(&reason) {
                    budget.cooldown(chrono::Utc::now().timestamp(), format!("global_{reason}"));
                } else {
                    budget.budget_hit_reason = Some(format!("global_{reason}"));
                    if self.global_budget_fail_open {
                        tracing::warn!(
                            node_id = %node.node.id,
                            reason = %reason,
                            "global budget unavailable; failing open to local budget"
                        );
                        return true;
                    }
                }
                false
            }
        }
    }

    fn is_global_budget_rejection(reason: &str) -> bool {
        matches!(
            reason,
            "max_calls" | "max_tokens" | "max_kb" | "cooldown" | "max_concurrent"
        )
    }

    fn request_exceeds_single_node_budget(&self, meta: &RequestMeta) -> bool {
        meta.request_kb() > self.budget_limits.max_kb_per_window
            || self.budget_limits.max_calls_per_window == 0
    }

    fn try_sampled_acquire(
        &self,
        nodes: &[PoolNode],
        meta: &RequestMeta,
        now: i64,
    ) -> Option<NodeRef> {
        let sample_count = nodes.len().min(8);
        if sample_count == 0 {
            return None;
        }

        let mut best_idx = None;
        let mut best_score = f64::MIN;
        for _ in 0..sample_count {
            let idx = fastrand::usize(..nodes.len());
            let node = &nodes[idx];
            let concurrent_now = node.active_leases.load(Ordering::Relaxed);
            let max_concurrent = node.max_concurrent.load(Ordering::Relaxed);
            if node
                .budget
                .read()
                .unwrap()
                .can_admit(meta, now, concurrent_now, max_concurrent)
                .is_err()
            {
                continue;
            }
            let score = node.score_for(Some(meta));
            if score > best_score {
                best_score = score;
                best_idx = Some(idx);
            }
        }

        let node = &nodes[best_idx?];
        if node.try_admit(meta, now) {
            if self.global_admit(node, meta) {
                return Some(node.node.clone());
            }
            node.rollback_local_admit(meta);
        }
        None
    }

    fn shard_index_for_id(&self, node_id: &str) -> usize {
        let mut hash = 0usize;
        for byte in node_id.as_bytes() {
            hash = hash.wrapping_mul(31).wrapping_add(*byte as usize);
        }
        hash % self.shards.len().max(1)
    }

    fn acquire_from_shard(
        &self,
        shard_idx: usize,
        meta: &RequestMeta,
        now: i64,
    ) -> Option<NodeRef> {
        let nodes = self.shards[shard_idx].nodes.read().unwrap();
        if nodes.is_empty() {
            return None;
        }

        if let Some(node) = self.try_sampled_acquire(&nodes, meta, now) {
            return Some(node);
        }

        let eligible: Vec<&PoolNode> = nodes
            .iter()
            .filter(|node| {
                let concurrent_now = node.active_leases.load(Ordering::Relaxed);
                let max_concurrent = node.max_concurrent.load(Ordering::Relaxed);
                node.budget
                    .read()
                    .unwrap()
                    .can_admit(meta, now, concurrent_now, max_concurrent)
                    .is_ok()
            })
            .collect();
        if eligible.is_empty() {
            for node in nodes.iter() {
                let _ = node.try_admit(meta, now);
            }
            return None;
        }

        let total: f64 = eligible.iter().map(|n| n.score_for(Some(meta))).sum();
        if total <= 0.0 {
            return None;
        }

        let threshold = fastrand::f64() * total;
        let mut cumulative = 0.0;
        for n in eligible {
            cumulative += n.score_for(Some(meta));
            if cumulative >= threshold {
                if n.try_admit(meta, now) {
                    if self.global_admit(n, meta) {
                        return Some(n.node.clone());
                    }
                    n.rollback_local_admit(meta);
                }
                continue;
            }
        }

        None
    }

    fn acquire_best_global(&self, meta: &RequestMeta, now: i64) -> Option<NodeRef> {
        let mut candidates = Vec::new();
        for shard in &self.shards {
            let nodes = shard.nodes.read().unwrap();
            for node in nodes.iter() {
                let concurrent_now = node.active_leases.load(Ordering::Relaxed);
                let max_concurrent = node.max_concurrent.load(Ordering::Relaxed);
                if node
                    .budget
                    .read()
                    .unwrap()
                    .can_admit(meta, now, concurrent_now, max_concurrent)
                    .is_ok()
                {
                    candidates.push((node.score_for(Some(meta)), node.node.id.clone()));
                }
            }
        }
        candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        for (_, node_id) in candidates.into_iter().take(8) {
            if let Ok(node) = self.try_acquire_sticky(meta, &node_id) {
                return Some(node);
            }
        }
        None
    }

    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }
}

impl Default for DispatchPool {
    fn default() -> Self {
        Self::new()
    }
}

impl Pool for DispatchPool {
    fn acquire(&self) -> Option<NodeRef> {
        self.acquire_for(&RequestMeta {
            model: String::new(),
            stream: false,
            body_size: 1,
        })
    }

    fn acquire_for(&self, meta: &RequestMeta) -> Option<NodeRef> {
        if self.request_exceeds_single_node_budget(meta) {
            return None;
        }

        let now = chrono::Utc::now().timestamp();
        let start = fastrand::usize(..self.shards.len());
        for offset in 0..self.shards.len() {
            let idx = (start + offset) % self.shards.len();
            if let Some(node) = self.acquire_from_shard(idx, meta, now) {
                return Some(node);
            }
        }

        self.acquire_best_global(meta, now)
    }

    fn try_acquire_sticky(
        &self,
        _meta: &RequestMeta,
        node_id: &NodeId,
    ) -> Result<NodeRef, DispatchError> {
        if self.request_exceeds_single_node_budget(_meta) {
            return Err(DispatchError::RequestTooLarge);
        }

        let shard_idx = self.shard_index_for_id(node_id);
        let nodes = self.shards[shard_idx].nodes.read().unwrap();
        let now = chrono::Utc::now().timestamp();
        let node = nodes
            .iter()
            .find(|n| n.node.id == *node_id)
            .ok_or(DispatchError::NoResource)?;
        if node.try_admit(_meta, now) {
            if self.global_admit(node, _meta) {
                return Ok(node.node.clone());
            }
            node.rollback_local_admit(_meta);
        }
        Err(DispatchError::NoResource)
    }

    fn preflight(&self, meta: &RequestMeta) -> Result<(), DispatchError> {
        if self.request_exceeds_single_node_budget(meta) {
            Err(DispatchError::RequestTooLarge)
        } else {
            Ok(())
        }
    }

    fn release_with_latency(&self, node_id: &NodeId, result: &ResultKind, latency_ms: u64) {
        let shard_idx = self.shard_index_for_id(node_id);
        let mut nodes = self.shards[shard_idx].nodes.write().unwrap();
        if let Some(pn) = nodes.iter_mut().find(|n| n.node.id == *node_id) {
            pn.release_lease();
            if let Some(registry) = &self.global_budget {
                registry.release_one(node_id);
            }
            match result {
                ResultKind::Success(_) => {
                    pn.record_result(true, latency_ms, &self.aimd);
                    pn.idle_since
                        .store(chrono::Utc::now().timestamp(), Ordering::Relaxed);
                }
                ResultKind::RateLimited => {
                    pn.record_result(false, latency_ms, &self.aimd);
                }
                ResultKind::EmptyOutput => {
                    pn.record_result(false, latency_ms, &self.aimd);
                }
                ResultKind::SoftFailure { .. } => {
                    pn.record_result(false, latency_ms, &self.aimd);
                }
                ResultKind::Error { .. } => {
                    pn.record_result(false, latency_ms, &self.aimd);
                }
            }
        }
    }

    fn record_latency_hint(&self, node_id: &NodeId, latency_ms: u64) {
        let shard_idx = self.shard_index_for_id(node_id);
        let nodes = self.shards[shard_idx].nodes.read().unwrap();
        if let Some(pn) = nodes.iter().find(|n| n.node.id == *node_id) {
            pn.record_latency(latency_ms);
        }
    }

    fn release(&self, node_id: &NodeId, result: &ResultKind) {
        self.release_with_latency(node_id, result, 0);
    }

    fn remove(&self, node_id: &NodeId) {
        let shard_idx = self.shard_index_for_id(node_id);
        let mut nodes = self.shards[shard_idx].nodes.write().unwrap();
        nodes.retain(|n| n.node.id != *node_id);
    }

    fn add(&self, node: NodeRef) {
        let shard_idx = self.shard_index_for_id(&node.id);
        let mut nodes = self.shards[shard_idx].nodes.write().unwrap();
        if !nodes.iter().any(|n| n.node.id == node.id) {
            nodes.push(PoolNode::new(node, self.budget_limits.clone(), &self.aimd));
        }
    }

    fn available(&self) -> usize {
        self.shards
            .iter()
            .map(|shard| {
                shard
                    .nodes
                    .read()
                    .unwrap()
                    .iter()
                    .filter(|node| node.snapshot().node_state == "dispatch")
                    .count()
            })
            .sum()
    }

    fn budget_counts(&self) -> (usize, usize, usize) {
        self.budget_counts()
    }

    fn budget_details(&self) -> Vec<Value> {
        let mut details = Vec::new();
        for shard in &self.shards {
            details.extend(
                shard
                    .nodes
                    .read()
                    .unwrap()
                    .iter()
                    .map(|node| node.detail(self.global_budget.as_ref())),
            );
        }
        details
    }

    fn node_budget_detail(&self, node_id: &NodeId) -> Option<Value> {
        let shard_idx = self.shard_index_for_id(node_id);
        self.shards[shard_idx]
            .nodes
            .read()
            .unwrap()
            .iter()
            .find(|node| node.node.id == *node_id)
            .map(|node| node.detail(self.global_budget.as_ref()))
    }

    fn name(&self) -> &'static str {
        "dispatch"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(body_size: u64) -> RequestMeta {
        RequestMeta {
            model: "deepseek-v4-flash".to_string(),
            stream: true,
            body_size,
        }
    }

    #[test]
    fn acquire_respects_node_concurrency_lease() {
        let pool = DispatchPool::new();
        pool.add(NodeRef::new(
            "socks5h://user:pass@127.0.0.1:1080".to_string(),
        ));

        for _ in 0..5 {
            assert!(pool.acquire_for(&meta(100)).is_some());
        }
        assert!(pool.acquire_for(&meta(100)).is_none());

        let snapshot = pool.budget_snapshots().pop().unwrap();
        assert_eq!(snapshot.concurrent_now, 5);
        assert_eq!(
            snapshot.budget_hit_reason.as_deref(),
            Some("max_concurrent")
        );
    }

    #[test]
    fn acquire_moves_node_to_cooldown_when_call_budget_is_hit() {
        let pool = DispatchPool::new_with_limits(NodeBudgetLimits {
            max_calls_per_window: 3,
            ..NodeBudgetLimits::default()
        });
        pool.add(NodeRef::new(
            "socks5h://user:pass@127.0.0.1:1081".to_string(),
        ));
        let node_id = pool.budget_snapshots()[0].node_id.clone();

        for _ in 0..3 {
            let node = pool.acquire_for(&meta(100)).unwrap();
            pool.release(&node.id, &ResultKind::Success(200));
        }

        assert!(pool.acquire_for(&meta(100)).is_none());
        let snapshot = pool
            .budget_snapshots()
            .into_iter()
            .find(|snapshot| snapshot.node_id == node_id)
            .unwrap();
        assert_eq!(snapshot.node_state, "cooldown");
        assert_eq!(snapshot.budget_hit_reason.as_deref(), Some("max_calls"));
    }

    #[test]
    fn release_with_latency_updates_node_latency_score_input() {
        let pool = DispatchPool::new();
        pool.add(NodeRef::new(
            "socks5h://user:pass@127.0.0.1:1085".to_string(),
        ));

        let node = pool.acquire_for(&meta(100)).unwrap();
        pool.release_with_latency(&node.id, &ResultKind::Success(200), 12_345);

        let detail = pool.node_budget_detail(&node.id).unwrap();
        assert_eq!(detail["avg_latency_ms"].as_u64(), Some(12_345));
    }

    #[test]
    fn latency_hint_updates_score_without_releasing_lease() {
        let pool = DispatchPool::new();
        pool.add(NodeRef::new(
            "socks5h://user:pass@127.0.0.1:10850".to_string(),
        ));

        let node = pool.acquire_for(&meta(256 * 1024)).unwrap();
        pool.record_latency_hint(&node.id, 1_234);

        let snapshot = pool
            .budget_snapshots()
            .into_iter()
            .find(|snapshot| snapshot.node_id == node.id)
            .unwrap();
        let detail = pool.node_budget_detail(&node.id).unwrap();
        assert_eq!(snapshot.concurrent_now, 1);
        assert_eq!(detail["avg_latency_ms"].as_u64(), Some(1_234));
    }

    #[test]
    fn successful_nodes_are_promoted_above_unverified_nodes() {
        let pool = DispatchPool::new();
        pool.add(NodeRef::new(
            "socks5h://user:pass@127.0.0.1:1086".to_string(),
        ));
        let node = pool.acquire_for(&meta(100)).unwrap();
        let before = pool.node_budget_detail(&node.id).unwrap()["base_score"]
            .as_f64()
            .unwrap();

        pool.release_with_latency(&node.id, &ResultKind::Success(200), 20_000);

        let after = pool.node_budget_detail(&node.id).unwrap()["base_score"]
            .as_f64()
            .unwrap();
        assert!(after > before);
    }

    #[test]
    fn dispatch_pool_spreads_nodes_across_configured_shards() {
        let pool =
            DispatchPool::new_with_options(NodeBudgetLimits::default(), AimdConfig::default(), 4);
        for port in 2000..2020 {
            pool.add(NodeRef::new(format!(
                "socks5h://user:pass@127.0.0.1:{port}"
            )));
        }

        assert_eq!(pool.shard_count(), 4);
        assert_eq!(pool.budget_snapshots().len(), 20);
        assert_eq!(pool.available(), 20);
    }

    #[test]
    fn acquire_samples_before_global_best_to_avoid_hot_node_lock_in() {
        fastrand::seed(7);
        let pool =
            DispatchPool::new_with_options(NodeBudgetLimits::default(), AimdConfig::default(), 8);
        for port in 2100..2140 {
            pool.add(NodeRef::new(format!(
                "socks5h://user:pass@127.0.0.1:{port}"
            )));
        }

        let first = pool.acquire_for(&meta(100)).unwrap();
        for _ in 0..30 {
            pool.release_with_latency(&first.id, &ResultKind::Success(200), 100);
        }

        let mut selected = std::collections::HashSet::new();
        for _ in 0..80 {
            let node = pool.acquire_for(&meta(100)).unwrap();
            selected.insert(node.id.clone());
            pool.release_with_latency(&node.id, &ResultKind::Success(200), 100);
        }

        assert!(
            selected.len() > 8,
            "scheduler locked into too few nodes: {selected:?}"
        );
    }

    #[test]
    fn aimd_expands_after_successes_and_reduces_after_errors() {
        let pool = DispatchPool::new_with_options(
            NodeBudgetLimits::default(),
            AimdConfig {
                min_concurrent: 1,
                max_concurrent: 8,
                success_step: 1,
                failure_percent: 50,
                slow_latency_ms: 10_000,
            },
            2,
        );
        pool.add(NodeRef::new(
            "socks5h://user:pass@127.0.0.1:2080".to_string(),
        ));

        let node = pool.acquire_for(&meta(100)).unwrap();
        for _ in 0..3 {
            pool.release_with_latency(&node.id, &ResultKind::Success(200), 100);
        }
        let expanded = pool.node_budget_detail(&node.id).unwrap()["local_budget"]["max_concurrent"]
            .as_u64()
            .unwrap();
        assert!(expanded > 5);

        pool.release_with_latency(&node.id, &ResultKind::RateLimited, 100);
        let reduced = pool.node_budget_detail(&node.id).unwrap()["local_budget"]["max_concurrent"]
            .as_u64()
            .unwrap();
        assert!(reduced < expanded);
    }

    #[test]
    fn global_budget_rejection_classifier_keeps_budget_limits_fail_closed() {
        assert!(DispatchPool::is_global_budget_rejection("max_calls"));
        assert!(DispatchPool::is_global_budget_rejection("max_tokens"));
        assert!(DispatchPool::is_global_budget_rejection("max_kb"));
        assert!(DispatchPool::is_global_budget_rejection("cooldown"));
        assert!(DispatchPool::is_global_budget_rejection("max_concurrent"));
        assert!(!DispatchPool::is_global_budget_rejection(
            "Connection refused"
        ));
    }

    #[test]
    fn acquire_moves_node_to_cooldown_when_token_budget_is_hit() {
        let pool = DispatchPool::new_with_limits(NodeBudgetLimits {
            max_tokens_per_window: 100,
            ..NodeBudgetLimits::default()
        });
        pool.add(NodeRef::new(
            "socks5h://user:pass@127.0.0.1:1082".to_string(),
        ));

        for _ in 0..2 {
            let node = pool.acquire_for(&meta(200)).unwrap();
            pool.release(&node.id, &ResultKind::Success(200));
        }

        assert!(pool.acquire_for(&meta(200)).is_none());
        let snapshot = pool.budget_snapshots().pop().unwrap();
        assert_eq!(snapshot.node_state, "cooldown");
        assert_eq!(snapshot.budget_hit_reason.as_deref(), Some("max_tokens"));
    }

    #[test]
    fn single_request_over_token_budget_does_not_preflight_reject() {
        let pool = DispatchPool::new_with_limits(NodeBudgetLimits {
            max_tokens_per_window: 100,
            max_kb_per_window: 64 * 1024,
            ..NodeBudgetLimits::default()
        });

        assert_eq!(pool.preflight(&meta(1_200)), Ok(()));
    }

    #[test]
    fn single_request_over_budget_does_not_cooldown_nodes() {
        let pool = DispatchPool::new_with_limits(NodeBudgetLimits {
            max_kb_per_window: 1,
            ..NodeBudgetLimits::default()
        });
        pool.add(NodeRef::new(
            "socks5h://user:pass@127.0.0.1:1083".to_string(),
        ));
        pool.add(NodeRef::new(
            "socks5h://user:pass@127.0.0.1:1084".to_string(),
        ));

        assert_eq!(
            pool.preflight(&meta(1_200)),
            Err(DispatchError::RequestTooLarge)
        );
        assert!(pool.acquire_for(&meta(1_200)).is_none());

        for snapshot in pool.budget_snapshots() {
            assert_eq!(snapshot.node_state, "dispatch");
            assert_eq!(snapshot.budget_hit_reason, None);
        }
    }
}
