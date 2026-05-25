use crate::collector::RequestTelemetry;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

pub struct RingBuffer {
    buffer: Mutex<VecDeque<RingEntry>>,
    capacity: usize,
    head: AtomicU64,
}

struct RingEntry {
    seq: u64,
    item: RequestTelemetry,
}

impl RingBuffer {
    pub fn new(capacity: usize) -> Self {
        RingBuffer {
            buffer: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity: capacity.max(1),
            head: AtomicU64::new(0),
        }
    }

    pub fn push(&self, item: RequestTelemetry) {
        let seq = self.head.fetch_add(1, Ordering::AcqRel);
        let mut buffer = self.buffer.lock().unwrap();
        if buffer.len() >= self.capacity {
            buffer.pop_front();
        }
        buffer.push_back(RingEntry { seq, item });
    }

    pub fn query(
        &self,
        since: Option<i64>,
        limit: usize,
        cursor: Option<u64>,
    ) -> (Vec<RequestTelemetry>, Option<u64>) {
        let head = self.head.load(Ordering::Relaxed);
        let start_seq = cursor.unwrap_or_else(|| head.saturating_sub(1));
        let limit = limit.max(1);
        let buffer = self.buffer.lock().unwrap();
        let mut results = Vec::with_capacity(limit.min(buffer.len()));
        let mut next_cursor = None;

        for entry in buffer.iter().rev().filter(|entry| entry.seq <= start_seq) {
            if let Some(since_ts) = since {
                if entry.item.ts < since_ts {
                    continue;
                }
            }
            results.push(entry.item.clone());
            if results.len() >= limit {
                next_cursor = entry.seq.checked_sub(1);
                break;
            }
        }

        (results, next_cursor)
    }
}
