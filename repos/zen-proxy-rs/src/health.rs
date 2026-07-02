use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{Duration, Instant};

const BACKOFF_BASE_MS: u64 = 1000;
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
            while ts.len() > self.window_size {
                ts.pop_front();
            }
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
        if hard > 0 && now < hard {
            return true;
        }
        let soft = self.soft_backoff_until.load(Ordering::Acquire);
        if soft > 0 && now < soft {
            return true;
        }
        false
    }

    pub fn stats(&self) -> UpstreamStats {
        let (total, count_429) = self.window_stats();
        let rate = if total == 0 {
            0.0
        } else {
            count_429 as f64 / total as f64
        };
        let success_rate = if total == 0 {
            100.0
        } else {
            (total - count_429) as f64 / total as f64 * 100.0
        };
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
        } else {
            (0, 0)
        }
    }

    fn consecutive_429(&self) -> u64 {
        if let Ok(ts) = self.timestamps.read() {
            let mut count = 0u64;
            for (_, s) in ts.iter().rev() {
                if *s == 429 {
                    count += 1;
                } else {
                    break;
                }
            }
            count
        } else {
            0
        }
    }
}

fn now_ns() -> u64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let epoch = EPOCH.get_or_init(Instant::now);
    Instant::now().duration_since(*epoch).as_nanos() as u64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Global429State {
    Normal,
    Suspected,
    Confirmed,
}

pub struct Global429Detector {
    state: RwLock<Global429State>,
    window: RwLock<VecDeque<(Instant, bool)>>,
}

impl Global429Detector {
    pub fn new(window_size: usize) -> Self {
        Self {
            state: RwLock::new(Global429State::Normal),
            window: RwLock::new(VecDeque::with_capacity(window_size)),
        }
    }

    pub fn record(&self, is_429: bool) {
        let mut window = self.window.write().unwrap();
        window.push_back((Instant::now(), is_429));
        while window.len() > window.capacity() {
            window.pop_front();
        }
        drop(window);
        self.update_state();
    }

    pub fn state(&self) -> Global429State {
        self.state
            .read()
            .map(|s| *s)
            .unwrap_or(Global429State::Normal)
    }

    pub fn is_global_backoff(&self) -> bool {
        self.state() == Global429State::Confirmed
    }

    fn update_state(&self) {
        let window = self.window.read().unwrap();
        let total = window.len();
        if total == 0 {
            return;
        }
        let count_429 = window.iter().filter(|(_, is_429)| *is_429).count();
        let rate = count_429 as f64 / total as f64;
        drop(window);

        let mut state = self.state.write().unwrap();
        *state = if rate > 0.70 {
            Global429State::Confirmed
        } else if rate > 0.40 {
            Global429State::Suspected
        } else {
            Global429State::Normal
        };
    }
}

#[derive(Debug, Clone)]
pub struct UpstreamStats {
    pub backoff: bool,
    pub rate_429: f64,
    pub total_requests: u64,
    pub success_rate: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_upstream_health_initial() {
        let h = UpstreamHealth::new(100);
        assert!(!h.is_backoff());
        let s = h.stats();
        assert_eq!(s.total_requests, 0);
    }

    #[test]
    fn test_upstream_health_backoff_on_429() {
        let h = UpstreamHealth::new(100);
        for _ in 0..10 {
            h.record(429);
        }
        assert!(h.is_backoff());
    }

    #[test]
    fn test_upstream_health_no_backoff_on_200() {
        let h = UpstreamHealth::new(100);
        for _ in 0..10 {
            h.record(200);
        }
        assert!(!h.is_backoff());
    }

    #[test]
    fn test_upstream_health_stats() {
        let h = UpstreamHealth::new(100);
        h.record(200);
        h.record(429);
        h.record(200);
        let s = h.stats();
        assert_eq!(s.total_requests, 3);
        assert!((s.rate_429 - 1.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn test_global_429_detector_initial() {
        let d = Global429Detector::new(100);
        assert_eq!(d.state(), Global429State::Normal);
        assert!(!d.is_global_backoff());
    }

    #[test]
    fn test_global_429_detector_confirmed() {
        let d = Global429Detector::new(100);
        for _ in 0..8 {
            d.record(true);
        }
        for _ in 0..2 {
            d.record(false);
        }
        assert_eq!(d.state(), Global429State::Confirmed);
        assert!(d.is_global_backoff());
    }

    #[test]
    fn test_global_429_detector_suspected() {
        let d = Global429Detector::new(100);
        for _ in 0..5 {
            d.record(true);
        }
        for _ in 0..5 {
            d.record(false);
        }
        assert_eq!(d.state(), Global429State::Suspected);
        assert!(!d.is_global_backoff());
    }

    #[test]
    fn test_global_429_detector_normal() {
        let d = Global429Detector::new(100);
        for _ in 0..10 {
            d.record(false);
        }
        assert_eq!(d.state(), Global429State::Normal);
    }
}
