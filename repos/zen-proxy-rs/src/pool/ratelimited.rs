use std::collections::HashMap;
use std::sync::RwLock;

use crate::pool::*;
use chrono::Datelike;

fn today_ymd() -> u32 {
    let now = chrono::Utc::now();
    let y = now.year() as u32;
    let m = now.month();
    let d = now.day();
    y * 10000 + m * 100 + d
}

struct QuarantineEntry {
    node: NodeRef,
    last_429_date: u32,
    consecutive_days: u32,
}

pub struct RateLimitedPoolImpl {
    entries: RwLock<HashMap<NodeId, QuarantineEntry>>,
}

impl RateLimitedPoolImpl {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for RateLimitedPoolImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl Pool for RateLimitedPoolImpl {
    fn acquire(&self) -> Option<NodeRef> {
        let entries = self.entries.read().unwrap();
        entries.values().next().map(|e| e.node.clone())
    }

    fn release(&self, node_id: &NodeId, result: &ResultKind) {
        let mut entries = self.entries.write().unwrap();
        if let Some(entry) = entries.get_mut(node_id) {
            if matches!(result, ResultKind::Success(_)) {
                entry.consecutive_days = 0;
            }
        }
    }

    fn remove(&self, node_id: &NodeId) {
        self.entries.write().unwrap().remove(node_id);
    }

    fn add(&self, node: NodeRef) {
        let mut entries = self.entries.write().unwrap();
        entries.entry(node.id.clone()).or_insert(QuarantineEntry {
            node,
            last_429_date: 0,
            consecutive_days: 0,
        });
    }

    fn available(&self) -> usize {
        self.entries.read().unwrap().len()
    }

    fn name(&self) -> &'static str {
        "ratelimited"
    }
}

impl RateLimitedPool for RateLimitedPoolImpl {
    fn quarantine(&self, node_id: NodeId) {
        let today = today_ymd();
        let mut entries = self.entries.write().unwrap();
        if let Some(entry) = entries.get_mut(&node_id) {
            if entry.last_429_date == today {
                entry.consecutive_days += 1;
            } else {
                entry.consecutive_days = 1;
            }
            entry.last_429_date = today;
        } else {
            entries.insert(
                node_id,
                QuarantineEntry {
                    node: NodeRef::new("unknown".to_string()),
                    last_429_date: today,
                    consecutive_days: 1,
                },
            );
        }
    }

    fn select_for_probe(&self, batch_size: usize) -> Vec<NodeId> {
        let today = today_ymd();
        let entries = self.entries.read().unwrap();
        entries
            .iter()
            .filter(|(_, e)| e.last_429_date < today)
            .take(batch_size)
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn select_all_for_probe(&self, batch_size: usize) -> Vec<NodeId> {
        let entries = self.entries.read().unwrap();
        entries
            .iter()
            .take(batch_size)
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn recover(&self, node_id: &NodeId) {
        self.entries.write().unwrap().remove(node_id);
    }

    fn quarantined_today(&self) -> usize {
        let today = today_ymd();
        let entries = self.entries.read().unwrap();
        entries
            .values()
            .filter(|e| e.last_429_date == today)
            .count()
    }

    fn get_node_ref(&self, node_id: &NodeId) -> Option<NodeRef> {
        self.entries
            .read()
            .unwrap()
            .get(node_id)
            .map(|e| e.node.clone())
    }
}
