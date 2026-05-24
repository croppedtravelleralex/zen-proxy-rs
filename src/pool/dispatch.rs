use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::Duration;

use crate::pool::global_budget::GlobalBudgetRegistry;
use crate::pool::*;
use reqwest::Client;
use serde_json::{json, Value};

const SCORE_SCALE: u64 = 100;
const DEFAULT_MAX_CALLS_PER_WINDOW: u64 = 100;
const DEFAULT_MAX_TOKENS_PER_WINDOW: u64 = 250_000;
const DEFAULT_MAX_KB_PER_WINDOW: u64 = 64 * 1024;
const DEFAULT_COOLDOWN_SECS: i64 = 60;

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
    client: Client,
}

impl PoolNode {
    fn new(node: NodeRef, client: Client, limits: NodeBudgetLimits) -> Self {
        Self {
            node,
            base_score: AtomicU64::new(80 * SCORE_SCALE),
            consecutive_successes: AtomicU32::new(0),
            recent_results: RwLock::new(VecDeque::with_capacity(20)),
            avg_latency_ms: AtomicU64::new(0),
            idle_since: AtomicI64::new(chrono::Utc::now().timestamp()),
            max_concurrent: AtomicU32::new(5),
            active_leases: AtomicU32::new(0),
            budget: RwLock::new(NodeBudget::from(limits)),
            client,
        }
    }

    fn score(&self) -> f64 {
        let base_pct = self.base_score.load(Ordering::Relaxed) as f64 / SCORE_SCALE as f64;
        let health = (base_pct / 100.0).clamp(0.0, 1.0) * 0.50;

        let recent = self.recent_results.read().unwrap();
        let total = recent.len();
        let successes = recent.iter().filter(|&&r| r).count();
        let success_rate = if total > 0 {
            successes as f64 / total as f64 * 0.20
        } else {
            0.0
        };
        drop(recent);

        let now = chrono::Utc::now().timestamp();
        let idle_secs = now - self.idle_since.load(Ordering::Relaxed);
        let idle_factor = (idle_secs as f64 / 60.0).min(1.0) * 0.15;

        let avg_lat = self.avg_latency_ms.load(Ordering::Relaxed) as f64;
        let latency_factor = (1.0 - (avg_lat / 5000.0).min(1.0)).max(0.0) * 0.10;

        let consec = self.consecutive_successes.load(Ordering::Relaxed) as f64;
        let momentum = (consec / 10.0).min(1.0) * 0.05;

        health + success_rate + idle_factor + latency_factor + momentum
    }

    fn record_result(&self, success: bool, latency_ms: u64) {
        {
            let mut recent = self.recent_results.write().unwrap();
            recent.push_back(success);
            while recent.len() > 20 {
                recent.pop_front();
            }
        }

        self.avg_latency_ms.store(latency_ms, Ordering::Relaxed);

        if success {
            let prev = self.consecutive_successes.fetch_add(1, Ordering::Relaxed);
            let _ = prev;
        } else {
            self.consecutive_successes.store(0, Ordering::Relaxed);
        }
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

pub struct DispatchPool {
    nodes: RwLock<Vec<PoolNode>>,
    idle_since: AtomicI64,
    budget_limits: NodeBudgetLimits,
    global_budget: Option<GlobalBudgetRegistry>,
}

impl DispatchPool {
    pub fn new() -> Self {
        Self::new_with_limits(NodeBudgetLimits::default())
    }

    pub fn new_with_limits(budget_limits: NodeBudgetLimits) -> Self {
        Self {
            nodes: RwLock::new(Vec::new()),
            idle_since: AtomicI64::new(chrono::Utc::now().timestamp()),
            budget_limits,
            global_budget: None,
        }
    }

    pub fn with_global_budget(mut self, global_budget: GlobalBudgetRegistry) -> Self {
        self.global_budget = Some(global_budget);
        self
    }

    pub fn budget_snapshots(&self) -> Vec<NodeBudgetSnapshot> {
        self.nodes
            .read()
            .unwrap()
            .iter()
            .map(PoolNode::snapshot)
            .collect()
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
                if matches!(
                    reason.as_str(),
                    "max_calls" | "max_tokens" | "max_kb" | "cooldown"
                ) {
                    budget.cooldown(chrono::Utc::now().timestamp(), format!("global_{reason}"));
                } else {
                    budget.budget_hit_reason = Some(format!("global_{reason}"));
                }
                false
            }
        }
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
        let nodes = self.nodes.read().unwrap();
        if nodes.is_empty() {
            return None;
        }

        let now = chrono::Utc::now().timestamp();
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

        let total: f64 = eligible.iter().map(|n| n.score()).sum();
        if total <= 0.0 {
            return None;
        }

        let threshold = fastrand::f64() * total;
        let mut cumulative = 0.0;
        for n in eligible {
            cumulative += n.score();
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

    fn try_acquire_sticky(
        &self,
        _meta: &RequestMeta,
        node_id: &NodeId,
    ) -> Result<NodeRef, DispatchError> {
        let nodes = self.nodes.read().unwrap();
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

    fn release(&self, node_id: &NodeId, result: &ResultKind) {
        let mut nodes = self.nodes.write().unwrap();
        if let Some(pn) = nodes.iter_mut().find(|n| n.node.id == *node_id) {
            pn.release_lease();
            if let Some(registry) = &self.global_budget {
                registry.release_one(node_id);
            }
            match result {
                ResultKind::Success(_) => {
                    pn.record_result(true, 0);
                    pn.idle_since
                        .store(chrono::Utc::now().timestamp(), Ordering::Relaxed);
                }
                ResultKind::RateLimited => {
                    pn.record_result(false, 0);
                }
                ResultKind::Error { .. } => {
                    pn.record_result(false, 0);
                }
            }
        }
    }

    fn remove(&self, node_id: &NodeId) {
        let mut nodes = self.nodes.write().unwrap();
        nodes.retain(|n| n.node.id != *node_id);
    }

    fn add(&self, node: NodeRef) {
        let mut nodes = self.nodes.write().unwrap();
        if !nodes.iter().any(|n| n.node.id == node.id) {
            let client = reqwest::Client::builder()
                .proxy(reqwest::Proxy::all(&node.url).unwrap())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap();
            nodes.push(PoolNode::new(node, client, self.budget_limits.clone()));
        }
    }

    fn available(&self) -> usize {
        self.nodes
            .read()
            .unwrap()
            .iter()
            .filter(|node| node.snapshot().node_state == "dispatch")
            .count()
    }

    fn budget_counts(&self) -> (usize, usize, usize) {
        self.budget_counts()
    }

    fn budget_details(&self) -> Vec<Value> {
        self.nodes
            .read()
            .unwrap()
            .iter()
            .map(|node| node.detail(self.global_budget.as_ref()))
            .collect()
    }

    fn node_budget_detail(&self, node_id: &NodeId) -> Option<Value> {
        self.nodes
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
    fn acquire_moves_node_to_cooldown_when_token_budget_is_hit() {
        let pool = DispatchPool::new_with_limits(NodeBudgetLimits {
            max_tokens_per_window: 100,
            ..NodeBudgetLimits::default()
        });
        pool.add(NodeRef::new(
            "socks5h://user:pass@127.0.0.1:1082".to_string(),
        ));

        assert!(pool.acquire_for(&meta(1_200)).is_none());
        let snapshot = pool.budget_snapshots().pop().unwrap();
        assert_eq!(snapshot.node_state, "cooldown");
        assert_eq!(snapshot.budget_hit_reason.as_deref(), Some("max_tokens"));
    }
}
