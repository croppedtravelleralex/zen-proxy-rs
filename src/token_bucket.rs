use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use crate::config::TokenMode;

const SCALE: u64 = 1_000_000;

pub struct TokenBucket {
    pub rate: f64,
    pub burst: f64,
    pub mode: TokenMode,
    tokens: AtomicU64,
    last_refill: AtomicU64,
    adaptive_rate: AtomicU64,
    adaptive_successes: AtomicU64,
    adaptive_total: AtomicU64,
    pub total_allowed: AtomicU64,
    pub total_denied: AtomicU64,
    pub total_requests: AtomicU64,
    adaptive_min_rate: f64,
    adaptive_max_rate: f64,
    #[allow(dead_code)]
    adaptive_window: u64,
}

impl TokenBucket {
    pub fn new(
        rate: f64,
        burst: f64,
        mode: TokenMode,
        min_rate: f64,
        max_rate: f64,
        window: u64,
    ) -> Self {
        let now = Self::now_ns();
        Self {
            rate,
            burst,
            mode,
            tokens: AtomicU64::new(Self::to_fixed(burst)),
            last_refill: AtomicU64::new(now),
            adaptive_rate: AtomicU64::new(Self::to_fixed(rate)),
            adaptive_successes: AtomicU64::new(0),
            adaptive_total: AtomicU64::new(0),
            total_allowed: AtomicU64::new(0),
            total_denied: AtomicU64::new(0),
            total_requests: AtomicU64::new(0),
            adaptive_min_rate: min_rate,
            adaptive_max_rate: max_rate,
            adaptive_window: window,
        }
    }

    pub fn allow(&self) -> bool {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        match self.mode {
            TokenMode::Unlimited => {
                self.total_allowed.fetch_add(1, Ordering::Relaxed);
                return true;
            }
            _ => {}
        }
        let effective_rate = if self.mode == TokenMode::Adaptive {
            Self::from_fixed(self.adaptive_rate.load(Ordering::Acquire))
        } else {
            self.rate
        };
        loop {
            self.refill_with_rate(effective_rate);
            let current = self.tokens.load(Ordering::Acquire);
            if current == 0 {
                self.total_denied.fetch_add(1, Ordering::Relaxed);
                return false;
            }
            let new_val = current.saturating_sub(Self::to_fixed(1.0));
            if self.tokens.compare_exchange(current, new_val, Ordering::Release, Ordering::Relaxed).is_ok() {
                self.total_allowed.fetch_add(1, Ordering::Relaxed);
                return true;
            }
        }
    }

    pub fn record_429(&self) {
        if self.mode != TokenMode::Adaptive { return; }
        let current_rate = Self::from_fixed(self.adaptive_rate.load(Ordering::Acquire));
        let new_rate = (current_rate * 0.3).max(self.adaptive_min_rate);
        self.adaptive_rate.store(Self::to_fixed(new_rate), Ordering::Release);
    }

    pub fn record_success(&self) {
        if self.mode == TokenMode::Adaptive {
            self.adaptive_successes.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_failure(&self) {
        if self.mode == TokenMode::Adaptive {
            self.adaptive_total.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn adapt_step(&self) {
        if self.mode != TokenMode::Adaptive { return; }
        let successes = self.adaptive_successes.load(Ordering::Acquire);
        let total = self.adaptive_total.load(Ordering::Acquire);
        if total == 0 { return; }
        let success_rate = successes as f64 / total as f64;
        let current_rate = Self::from_fixed(self.adaptive_rate.load(Ordering::Acquire));
        let new_rate = if success_rate > 0.95 {
            (current_rate * 1.25).min(self.adaptive_max_rate)
        } else if success_rate < 0.80 {
            (current_rate * 0.5).max(self.adaptive_min_rate)
        } else {
            current_rate
        };
        self.adaptive_rate.store(Self::to_fixed(new_rate), Ordering::Release);
        self.adaptive_successes.store(successes / 2, Ordering::Release);
        self.adaptive_total.store(total / 2, Ordering::Release);
    }

    pub fn effective_rate(&self) -> f64 {
        match self.mode {
            TokenMode::Adaptive => Self::from_fixed(self.adaptive_rate.load(Ordering::Acquire)),
            _ => self.rate,
        }
    }
    pub fn current_rate(&self) -> f64 {
        match self.mode {
            TokenMode::Adaptive => Self::from_fixed(self.adaptive_rate.load(Ordering::Acquire)),
            _ => self.rate,
        }
    }
    pub fn get_burst(&self) -> f64 { self.burst }

    fn refill_with_rate(&self, rate: f64) {
        let now = Self::now_ns();
        let last = self.last_refill.load(Ordering::Acquire);
        if now <= last { return; }
        let burst_fixed = Self::to_fixed(self.burst);
        loop {
            let current = self.tokens.load(Ordering::Acquire);
            let rate_fixed = Self::to_fixed(rate);
            let ns_elapsed = now.saturating_sub(last);
            let add: u64 = (rate_fixed as u128).saturating_mul(ns_elapsed as u128).saturating_div(1_000_000_000u128) as u64;
            let new_tokens = current.saturating_add(add).min(burst_fixed);
            if self.tokens.compare_exchange(current, new_tokens, Ordering::Release, Ordering::Relaxed).is_ok() {
                let _ = self.last_refill.compare_exchange(last, now, Ordering::Release, Ordering::Relaxed);
                return;
            }
        }
    }

    pub fn to_fixed(v: f64) -> u64 { (v * SCALE as f64).round() as u64 }
    pub fn from_fixed(v: u64) -> f64 { v as f64 / SCALE as f64 }

    pub fn now_ns() -> u64 {
        use std::sync::OnceLock;
        static EPOCH: OnceLock<Instant> = OnceLock::new();
        let epoch = EPOCH.get_or_init(Instant::now);
        Instant::now().duration_since(*epoch).as_nanos() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fixed_point_conversion() {
        for &v in &[0.0, 0.5, 1.0, 10.0, 100.0, 999999.0] {
            let fixed = TokenBucket::to_fixed(v);
            let back = TokenBucket::from_fixed(fixed);
            assert!((v - back).abs() < 1e-6, "round-trip failed for {}", v);
        }
    }
    #[test]
    fn test_new_bucket_starts_full() {
        let b = TokenBucket::new(10.0, 20.0, TokenMode::Fixed, 1.0, 100.0, 100);
        assert_eq!(b.tokens.load(Ordering::Relaxed), TokenBucket::to_fixed(20.0));
    }
    #[test]
    fn test_normal_mode_allows_then_denies() {
        let b = TokenBucket::new(1.0, 1.0, TokenMode::Fixed, 0.5, 10.0, 100);
        assert!(b.allow());
        let _ = b.allow();
        assert!(b.total_allowed.load(Ordering::Relaxed) >= 1);
}
}
