use crate::pool::ProbeResult;
use crate::v4::contracts::{DeadNodeState, DeadProbePolicy};

#[derive(Debug, Clone, Copy)]
pub struct AdaptiveDeadProbePolicy {
    min_delay_secs: u64,
    max_delay_secs: u64,
}

impl Default for AdaptiveDeadProbePolicy {
    fn default() -> Self {
        Self {
            min_delay_secs: 60 * 60,
            max_delay_secs: 120 * 60,
        }
    }
}

impl AdaptiveDeadProbePolicy {
    pub fn new(min_delay_secs: u64, max_delay_secs: u64) -> Self {
        Self {
            min_delay_secs,
            max_delay_secs,
        }
    }

    pub fn recovery_proven(consecutive_probe_successes: u8, chat_success: bool) -> bool {
        chat_success || consecutive_probe_successes >= 2
    }
}

impl DeadProbePolicy for AdaptiveDeadProbePolicy {
    fn next_delay_secs(&self, node: &DeadNodeState) -> u64 {
        let span = self.max_delay_secs.saturating_sub(self.min_delay_secs);
        if span == 0 {
            return self.min_delay_secs;
        }
        let jitter = stable_jitter(&node.node_id, node.dead_count as u64) % (span + 1);
        self.min_delay_secs + jitter
    }

    fn next_batch_size(&self, dead_count: usize, recent_recovery_rate: f64) -> usize {
        if dead_count == 0 {
            return 0;
        }
        let minimum = ((dead_count as f64) * 0.01).floor().max(1.0) as usize;
        let cap = ((dead_count as f64) * 0.10).floor().max(1.0) as usize;
        let cap = cap.min(20);
        let minimum = minimum.min(cap);
        if recent_recovery_rate >= 0.30 {
            (minimum * 2).min(cap)
        } else if recent_recovery_rate < 0.10 {
            minimum
        } else {
            minimum.min(cap)
        }
    }

    fn recovered(&self, result: &ProbeResult) -> bool {
        result.success
    }
}

fn stable_jitter(node_id: &str, salt: u64) -> u64 {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(node_id.as_bytes());
    hasher.update(salt.to_le_bytes());
    let hash = hasher.finalize();
    u64::from_le_bytes(hash[..8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dead_node(id: &str, dead_count: u32, recovery_rate: f64) -> DeadNodeState {
        DeadNodeState {
            node_id: id.to_string(),
            dead_count,
            last_probe_ts_ms: None,
            recent_recovery_rate: recovery_rate,
        }
    }

    #[test]
    fn delay_is_between_sixty_and_one_hundred_twenty_minutes() {
        let policy = AdaptiveDeadProbePolicy::default();
        for id in ["a", "b", "c", "d"] {
            let delay = policy.next_delay_secs(&dead_node(id, 3, 0.0));
            assert!((3600..=7200).contains(&delay));
        }
    }

    #[test]
    fn batch_size_starts_small_and_caps_growth() {
        let policy = AdaptiveDeadProbePolicy::default();
        assert_eq!(policy.next_batch_size(1, 0.0), 1);
        assert_eq!(policy.next_batch_size(100, 0.0), 1);
        assert_eq!(policy.next_batch_size(100, 0.35), 2);
        assert_eq!(policy.next_batch_size(10_000, 0.35), 20);
    }

    #[test]
    fn low_recovery_rate_resets_to_minimum_batch() {
        let policy = AdaptiveDeadProbePolicy::default();
        assert_eq!(policy.next_batch_size(500, 0.09), 5);
        assert_eq!(policy.next_batch_size(500, 0.29), 5);
    }

    #[test]
    fn recovery_requires_two_probe_successes_or_one_chat_success() {
        assert!(!AdaptiveDeadProbePolicy::recovery_proven(1, false));
        assert!(AdaptiveDeadProbePolicy::recovery_proven(2, false));
        assert!(AdaptiveDeadProbePolicy::recovery_proven(0, true));
    }
}
