use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const BACKOFF_BASE_MS: u64 = 1000;
const SOFT_BACKOFF_MS: u64 = 500;
const WINDOW_SECS: u64 = 60;

pub struct UpstreamHealth {
    pub timestamps: RwLock<VecDeque<(Instant, u16)>>,
    pub backoff_until: AtomicU64,
    pub soft_backoff_until: AtomicU64,
    pub window_size: usize,
}

impl UpstreamHealth {
    pub fn new(window_size: usize) -> Self {
        Self {
            timestamps: RwLock::new(VecDeque::with_capacity(window_size)),
            backoff_until: AtomicU64::new(0),
            soft_backoff_until: AtomicU64::new(0),
            window_size,
        }
    }

    pub fn record(&self, status: u16) {
        let now = now_ns();
        if let Ok(mut ts) = self.timestamps.write() {
            ts.push_back((Instant::now(), status));
            while ts.len() > self.window_size { ts.pop_front(); }
        }
        if status == 429 {
            let consecutive = self.consecutive_429();
            let backoff = BACKOFF_BASE_MS * (1u64 << (consecutive.min(10).saturating_sub(5)));
            let until = now + backoff * 1_000_000;
            self.backoff_until.store(until, Ordering::Release);
        }
    }

    pub fn is_backoff(&self) -> bool {
        let now = now_ns();
        let hard = self.backoff_until.load(Ordering::Acquire);
        if hard > 0 && now < hard { return true; }
        let soft = self.soft_backoff_until.load(Ordering::Acquire);
        if soft > 0 && now < soft { return true; }
        false
    }

    pub fn stats(&self) -> UpstreamStats {
        let (total, count_429) = self.window_stats();
        let rate = if total == 0 { 0.0 } else { count_429 as f64 / total as f64 };
        let success_rate = if total == 0 { 100.0 } else { (total - count_429) as f64 / total as f64 * 100.0 };
        UpstreamStats {
            backoff: self.is_backoff(),
            rate_429: rate,
            total_requests: total as u64,
            success_rate,
        }
    }

    fn window_stats(&self) -> (usize, usize) {
        if let Ok(ts) = self.timestamps.read() {
            let cutoff = Instant::now() - Duration::from_secs(WINDOW_SECS);
            let total = ts.iter().filter(|(t, _)| *t > cutoff).count();
            let c429 = ts.iter().filter(|(t, s)| *t > cutoff && *s == 429).count();
            (total, c429)
        } else { (0, 0) }
    }

    fn consecutive_429(&self) -> u64 {
        if let Ok(ts) = self.timestamps.read() {
            let mut count = 0u64;
            for (_, s) in ts.iter().rev() {
                if *s == 429 { count += 1; } else { break; }
            }
            count
        } else { 0 }
    }
}

pub struct UpstreamStats {
    pub backoff: bool,
    pub rate_429: f64,
    pub total_requests: u64,
    pub success_rate: f64,
}

pub struct ModelHealth {
    models: RwLock<std::collections::HashMap<String, bool>>,
}

impl ModelHealth {
    pub fn new() -> Self {
        Self { models: RwLock::new(std::collections::HashMap::new()) }
    }

    pub fn probe(&self, model_name: &str) -> bool {
        // Simplified: mark model as healthy
        if let Ok(mut m) = self.models.write() {
            m.insert(model_name.to_string(), true);
        }
        true
    }

    pub fn get_all(&self) -> std::collections::HashMap<String, bool> {
        self.models.read().map(|m| m.clone()).unwrap_or_default()
    }
}

fn now_ns() -> u64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let epoch = EPOCH.get_or_init(Instant::now);
    Instant::now().duration_since(*epoch).as_nanos() as u64
}


/// Upstream behavior state machine (normal -> slow -> circuit-break)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamState {
    Normal,
    Slow,
    CircuitBreak,
}

pub struct UpstreamBehaviorAnalyzer {
    state: std::sync::RwLock<UpstreamState>,
    error_window: std::sync::RwLock<std::collections::VecDeque<(Instant, u16)>>,
    window_size: usize,
    error_threshold: f64,
    slow_threshold: f64,
    circuit_break_threshold: f64,
}

impl UpstreamBehaviorAnalyzer {
    pub fn new(window_size: usize, error_threshold: f64, slow_threshold: f64, circuit_break_threshold: f64) -> Self {
        Self {
            state: std::sync::RwLock::new(UpstreamState::Normal),
            error_window: std::sync::RwLock::new(std::collections::VecDeque::with_capacity(window_size)),
            window_size,
            error_threshold,
            slow_threshold,
            circuit_break_threshold,
        }
    }

    pub fn record(&self, status: u16) {
        let now = Instant::now();
        if let Ok(mut w) = self.error_window.write() {
            w.push_back((now, status));
            while w.len() > self.window_size { w.pop_front(); }
        }
    }

    pub fn analyze(&self) -> UpstreamState {
        let error_rate = self.error_rate();
        let new_state = if error_rate > self.circuit_break_threshold {
            UpstreamState::CircuitBreak
        } else if error_rate > self.slow_threshold {
            UpstreamState::Slow
        } else {
            UpstreamState::Normal
        };
        if let Ok(mut s) = self.state.write() {
            *s = new_state;
        }
        new_state
    }

    pub fn state(&self) -> UpstreamState {
        self.state.read().map(|g| *g).unwrap_or(UpstreamState::Normal)
    }

    pub fn error_rate(&self) -> f64 {
        if let Ok(w) = self.error_window.read() {
            let cutoff = Instant::now() - Duration::from_secs(60);
            let recent: Vec<_> = w.iter().filter(|(t, _)| *t > cutoff).collect();
            let total = recent.len();
            if total == 0 { return 0.0; }
            let errors = recent.iter().filter(|(_, s)| *s >= 500 || *s == 429).count();
            errors as f64 / total as f64
        } else { 0.0 }
    }

    pub fn circuit_break_until(&self) -> Option<Instant> {
        if self.state() == UpstreamState::CircuitBreak {
            Some(Instant::now() + Duration::from_secs(30))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod analyzer_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_upstream_analyzer_initial_state() {
        let a = UpstreamBehaviorAnalyzer::new(10, 0.3, 0.5, 0.8);
        assert_eq!(a.state(), UpstreamState::Normal);
        assert_eq!(a.error_rate(), 0.0);
        assert!(a.circuit_break_until().is_none());
    }

    #[test]
    fn test_upstream_analyzer_normal_stays_normal() {
        let a = UpstreamBehaviorAnalyzer::new(10, 0.3, 0.5, 0.8);
        for _ in 0..10 { a.record(200); }
        assert_eq!(a.analyze(), UpstreamState::Normal);
    }

    #[test]
    fn test_upstream_analyzer_high_error_triggers_circuit_break() {
        let a = UpstreamBehaviorAnalyzer::new(10, 0.3, 0.5, 0.5);
        for _ in 0..6 { a.record(503); }
        for _ in 0..4 { a.record(200); }
        assert_eq!(a.analyze(), UpstreamState::CircuitBreak);
        assert!(a.circuit_break_until().is_some());
    }

    #[test]
    fn test_upstream_analyzer_error_rate_calculation() {
        let a = UpstreamBehaviorAnalyzer::new(20, 0.3, 0.5, 0.8);
        for _ in 0..5 { a.record(200); }
        for _ in 0..5 { a.record(429); }
        let rate = a.error_rate();
        assert!((rate - 0.5).abs() < 0.001, "expected 0.5, got {}", rate);


    #[test]
    fn test_analyzer_triggers_slow_at_threshold() {
        let analyzer = UpstreamBehaviorAnalyzer::new(100, 0.3, 0.3, 0.8);
        for _ in 0..30 { analyzer.record(200); }
        for _ in 0..20 { analyzer.record(502); }
        assert_eq!(analyzer.analyze(), UpstreamState::Slow, "40% errors should trigger Slow");
    }

    #[test]
    fn test_analyzer_triggers_circuit_break() {
        let analyzer = UpstreamBehaviorAnalyzer::new(100, 0.3, 0.3, 0.5);
        for _ in 0..40 { analyzer.record(200); }
        for _ in 0..60 { analyzer.record(502); }
        assert_eq!(analyzer.analyze(), UpstreamState::CircuitBreak, "60% errors should trigger CircuitBreak");
    }

    #[test]
    fn test_analyzer_recovers_to_normal() {
        let analyzer = UpstreamBehaviorAnalyzer::new(100, 0.3, 0.3, 0.8);
        for _ in 0..30 { analyzer.record(200); }
        for _ in 0..20 { analyzer.record(502); }
        assert_eq!(analyzer.analyze(), UpstreamState::Slow);
        for _ in 0..50 { analyzer.record(200); }
        assert_eq!(analyzer.analyze(), UpstreamState::Normal);
    }

    #[test]
    fn test_analyzer_circuit_break_returns_some() {
        let analyzer = UpstreamBehaviorAnalyzer::new(10, 0.3, 0.3, 0.3);
        for _ in 0..10 { analyzer.record(502); }
        analyzer.analyze();
        assert!(analyzer.circuit_break_until().is_some(), "circuit-break state should return Some");
    }

    #[test]
    fn test_analyzer_normal_returns_none() {
        let analyzer = UpstreamBehaviorAnalyzer::new(10, 0.3, 0.3, 0.8);
        for _ in 0..10 { analyzer.record(200); }
        analyzer.analyze();
        assert!(analyzer.circuit_break_until().is_none(), "normal state should return None");
    }

    }
}
