use crate::collector::RequestTelemetry;
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct RingBuffer {
    buffer: Box<[UnsafeCell<Option<RequestTelemetry>>]>,
    capacity: usize,
    head: AtomicU64,
}

unsafe impl Sync for RingBuffer {}

impl RingBuffer {
    pub fn new(capacity: usize) -> Self {
        let mut buf = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            buf.push(UnsafeCell::new(None));
        }
        RingBuffer {
            buffer: buf.into_boxed_slice(),
            capacity,
            head: AtomicU64::new(0),
        }
    }

    pub fn push(&self, item: RequestTelemetry) {
        let pos = self.head.fetch_add(1, Ordering::Relaxed) as usize % self.capacity;
        unsafe {
            *self.buffer[pos].get() = Some(item);
        }
    }

    pub fn query(
        &self,
        since: Option<i64>,
        limit: usize,
        cursor: Option<u64>,
    ) -> (Vec<RequestTelemetry>, Option<u64>) {
        let head = self.head.load(Ordering::Relaxed);
        let start = cursor.unwrap_or(head.saturating_sub(1));
        let mut results = Vec::with_capacity(limit.min(self.capacity));
        let mut next_cursor = None;

        let scan_count = self.capacity.min(head as usize);
        for i in 0..scan_count {
            let idx = (start as usize).wrapping_sub(i) % self.capacity;
            let entry = unsafe { &*self.buffer[idx].get() };
            if let Some(ref tele) = entry {
                if let Some(since_ts) = since {
                    if tele.ts < since_ts {
                        continue;
                    }
                }
                results.push(tele.clone());
                if results.len() >= limit {
                    let new_pos = (start as usize).wrapping_sub(i).wrapping_sub(1);
                    next_cursor = Some(new_pos as u64);
                    break;
                }
            }
        }

        (results, next_cursor)
    }
}
