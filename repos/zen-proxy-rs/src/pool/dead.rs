use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Instant;

use crate::pool::*;

struct DeadEntry {
    node: NodeRef,
    entered_at: Instant,
    last_probe_at: Option<Instant>,
    consecutive_probe_successes: u8,
    dead_count: u32,
}

pub struct DeadPoolImpl {
    entries: RwLock<HashMap<NodeId, DeadEntry>>,
}

impl DeadPoolImpl {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for DeadPoolImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl Pool for DeadPoolImpl {
    fn acquire(&self) -> Option<NodeRef> {
        let entries = self.entries.read().unwrap();
        entries.values().next().map(|e| e.node.clone())
    }

    fn release(&self, _node_id: &NodeId, _result: &ResultKind) {}

    fn remove(&self, node_id: &NodeId) {
        self.entries.write().unwrap().remove(node_id);
    }

    fn add(&self, node: NodeRef) {
        let mut entries = self.entries.write().unwrap();
        entries.entry(node.id.clone()).or_insert(DeadEntry {
            node,
            entered_at: Instant::now(),
            last_probe_at: None,
            consecutive_probe_successes: 0,
            dead_count: 0,
        });
    }

    fn available(&self) -> usize {
        self.entries.read().unwrap().len()
    }

    fn name(&self) -> &'static str {
        "dead"
    }
}

impl DeadPool for DeadPoolImpl {
    fn bury(&self, node_id: NodeId) {
        let mut entries = self.entries.write().unwrap();
        if let Some(entry) = entries.get_mut(&node_id) {
            entry.dead_count += 1;
            entry.entered_at = Instant::now();
            entry.last_probe_at = None;
            entry.consecutive_probe_successes = 0;
        } else {
            entries.insert(
                node_id,
                DeadEntry {
                    node: NodeRef::new("unknown".to_string()),
                    entered_at: Instant::now(),
                    last_probe_at: None,
                    consecutive_probe_successes: 0,
                    dead_count: 1,
                },
            );
        }
    }

    fn select_all_for_probe(&self) -> Vec<NodeId> {
        self.entries.read().unwrap().keys().cloned().collect()
    }

    fn dead_age_secs(&self, node_id: &NodeId) -> Option<u64> {
        self.entries
            .read()
            .unwrap()
            .get(node_id)
            .map(|entry| entry.entered_at.elapsed().as_secs())
    }

    fn last_probe_age_secs(&self, node_id: &NodeId) -> Option<u64> {
        self.entries
            .read()
            .unwrap()
            .get(node_id)
            .and_then(|entry| entry.last_probe_at.map(|last| last.elapsed().as_secs()))
    }

    fn record_probe_result(&self, node_id: &NodeId, success: bool) -> u8 {
        let mut entries = self.entries.write().unwrap();
        let Some(entry) = entries.get_mut(node_id) else {
            return 0;
        };
        entry.last_probe_at = Some(Instant::now());
        if success {
            entry.consecutive_probe_successes = entry.consecutive_probe_successes.saturating_add(1);
        } else {
            entry.consecutive_probe_successes = 0;
        }
        entry.consecutive_probe_successes
    }

    fn recover(&self, node_id: &NodeId) {
        self.entries.write().unwrap().remove(node_id);
    }

    fn dead_count(&self, node_id: &NodeId) -> u32 {
        self.entries
            .read()
            .unwrap()
            .get(node_id)
            .map(|e| e.dead_count)
            .unwrap_or(0)
    }

    fn get_node_ref(&self, node_id: &NodeId) -> Option<NodeRef> {
        self.entries
            .read()
            .unwrap()
            .get(node_id)
            .map(|e| e.node.clone())
    }
}
