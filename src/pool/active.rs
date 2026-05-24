use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::RwLock;
use std::time::Instant;

use crate::pool::*;

struct ActiveEntry {
    node_ref: NodeRef,
    active_requests: AtomicI64,
    max_concurrent: AtomicU32,
    entered_at: Instant,
}

impl ActiveEntry {
    fn new(node_ref: NodeRef) -> Self {
        Self {
            node_ref,
            active_requests: AtomicI64::new(0),
            max_concurrent: AtomicU32::new(5),
            entered_at: Instant::now(),
        }
    }
}

pub struct ActivePool {
    entries: RwLock<HashMap<NodeId, ActiveEntry>>,
}

impl ActivePool {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for ActivePool {
    fn default() -> Self {
        Self::new()
    }
}

impl Pool for ActivePool {
    fn acquire(&self) -> Option<NodeRef> {
        let entries = self.entries.read().unwrap();
        for entry in entries.values() {
            let current = entry.active_requests.load(Ordering::Relaxed);
            let max = entry.max_concurrent.load(Ordering::Relaxed) as i64;
            if current < max {
                entry.active_requests.fetch_add(1, Ordering::SeqCst);
                return Some(entry.node_ref.clone());
            }
        }
        None
    }

    fn release(&self, node_id: &NodeId, result: &ResultKind) {
        let entries = self.entries.read().unwrap();
        if let Some(entry) = entries.get(node_id) {
            entry.active_requests.fetch_sub(1, Ordering::SeqCst);

            match result {
                ResultKind::Success(_) => {
                    let cur = entry.max_concurrent.load(Ordering::Relaxed);
                    let next = cur.saturating_add(1).min(20);
                    entry.max_concurrent.store(next, Ordering::Relaxed);
                }
                _ => {
                    let cur = entry.max_concurrent.load(Ordering::Relaxed);
                    let next = (cur / 2).max(1);
                    entry.max_concurrent.store(next, Ordering::Relaxed);
                }
            }
        }
    }

    fn remove(&self, node_id: &NodeId) {
        self.entries.write().unwrap().remove(node_id);
    }

    fn add(&self, node: NodeRef) {
        let mut entries = self.entries.write().unwrap();
        let entry = entries
            .entry(node.id.clone())
            .or_insert_with(|| ActiveEntry::new(node));
        entry.active_requests.fetch_add(1, Ordering::SeqCst);
    }

    fn available(&self) -> usize {
        let entries = self.entries.read().unwrap();
        entries
            .values()
            .map(|e| e.active_requests.load(Ordering::Relaxed) as usize)
            .sum()
    }

    fn name(&self) -> &'static str {
        "active"
    }
}
