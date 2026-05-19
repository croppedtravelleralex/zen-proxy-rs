use crate::collector::RequestTelemetry;
use serde_json::json;
use std::collections::HashMap;
use std::sync::RwLock;

struct AggWindow {
    window_start: i64,
    by_model: HashMap<String, AggRow>,
    by_node: HashMap<String, AggRow>,
    by_pool: HashMap<String, AggRow>,
    by_status: HashMap<String, AggRow>,
}

#[derive(Default, Clone)]
struct AggRow {
    count: u64,
    bytes: u64,
    latency_total: u64,
    tokens: u64,
}

pub struct RollingAggregator {
    windows: RwLock<Vec<AggWindow>>,
    current: RwLock<AggWindow>,
    window_ms: i64,
    max_windows: usize,
}

impl RollingAggregator {
    pub fn new(window_ms: i64, max_windows: usize) -> Self {
        let now_ms = crate::collector::telemetry::unix_ms();
        let aligned = now_ms - (now_ms % window_ms);
        RollingAggregator {
            windows: RwLock::new(Vec::with_capacity(max_windows)),
            current: RwLock::new(AggWindow {
                window_start: aligned,
                by_model: HashMap::new(),
                by_node: HashMap::new(),
                by_pool: HashMap::new(),
                by_status: HashMap::new(),
            }),
            window_ms,
            max_windows,
        }
    }

    pub fn record(&self, tele: &RequestTelemetry) {
        let now_ms = crate::collector::telemetry::unix_ms();
        let aligned = now_ms - (now_ms % self.window_ms);

        let mut cur = self.current.write().unwrap();
        if cur.window_start != aligned {
            let old = std::mem::replace(
                &mut *cur,
                AggWindow {
                    window_start: aligned,
                    by_model: HashMap::new(),
                    by_node: HashMap::new(),
                    by_pool: HashMap::new(),
                    by_status: HashMap::new(),
                },
            );
            let mut windows = self.windows.write().unwrap();
            windows.push(old);
            while windows.len() > self.max_windows {
                windows.remove(0);
            }
        }

        let status_key = if tele.rate_limited {
            "429".to_string()
        } else if tele.status >= 500 {
            "5xx".to_string()
        } else if tele.status >= 400 {
            "4xx".to_string()
        } else {
            "2xx".to_string()
        };

        let row = AggRow {
            count: 1,
            bytes: tele.bytes_sent + tele.bytes_received,
            latency_total: tele.latency_total_ms,
            tokens: tele.total_tokens as u64,
        };

        Self::merge_row(&mut cur.by_model, &tele.model, &row);
        Self::merge_row(&mut cur.by_node, &tele.node_url, &row);
        Self::merge_row(&mut cur.by_pool, &tele.pool, &row);
        Self::merge_row(&mut cur.by_status, &status_key, &row);
    }

    fn merge_row(map: &mut HashMap<String, AggRow>, key: &str, row: &AggRow) {
        let entry = map.entry(key.to_string()).or_default();
        entry.count += row.count;
        entry.bytes += row.bytes;
        entry.latency_total += row.latency_total;
        entry.tokens += row.tokens;
    }

    pub fn snapshot(&self) -> serde_json::Value {
        let cur = self.current.read().unwrap();
        let windows = self.windows.read().unwrap();

        let mut all = Vec::with_capacity(windows.len() + 1);
        let mut cur_snapshot = Self::window_to_json(&cur);
        cur_snapshot["current"] = json!(true);
        all.push(cur_snapshot);

        for w in windows.iter() {
            let mut wj = Self::window_to_json(w);
            wj["current"] = json!(false);
            all.push(wj);
        }

        json!({
            "windows": all,
            "window_ms": self.window_ms,
            "max_windows": self.max_windows,
        })
    }

    fn window_to_json(w: &AggWindow) -> serde_json::Value {
        json!({
            "window_start": w.window_start,
            "by_model": Self::rows_to_json(&w.by_model),
            "by_node": Self::rows_to_json(&w.by_node),
            "by_pool": Self::rows_to_json(&w.by_pool),
            "by_status": Self::rows_to_json(&w.by_status),
        })
    }

    fn rows_to_json(map: &HashMap<String, AggRow>) -> serde_json::Value {
        let mut out = serde_json::Map::new();
        for (k, v) in map {
            out.insert(
                k.clone(),
                json!({
                    "count": v.count,
                    "bytes": v.bytes,
                    "latency_total": v.latency_total,
                    "tokens": v.tokens,
                }),
            );
        }
        serde_json::Value::Object(out)
    }

    pub fn load_snapshot(&self, data: &str) {
        let v: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return,
        };
        let windows_arr = match v.get("windows").and_then(|w| w.as_array()) {
            Some(arr) => arr,
            None => return,
        };
        let mut windows = self.windows.write().unwrap();
        windows.clear();
        for wv in windows_arr.iter() {
            let ws = wv.get("window_start").and_then(|x| x.as_i64()).unwrap_or(0);
            let by_model = Self::json_to_rows(wv.get("by_model"));
            let by_node = Self::json_to_rows(wv.get("by_node"));
            let by_pool = Self::json_to_rows(wv.get("by_pool"));
            let by_status = Self::json_to_rows(wv.get("by_status"));
            windows.push(AggWindow {
                window_start: ws,
                by_model,
                by_node,
                by_pool,
                by_status,
            });
        }
    }

    fn json_to_rows(val: Option<&serde_json::Value>) -> HashMap<String, AggRow> {
        let mut map = HashMap::new();
        let obj = match val.and_then(|v| v.as_object()) {
            Some(o) => o,
            None => return map,
        };
        for (k, v) in obj {
            let row = AggRow {
                count: v.get("count").and_then(|x| x.as_u64()).unwrap_or(0),
                bytes: v.get("bytes").and_then(|x| x.as_u64()).unwrap_or(0),
                latency_total: v.get("latency_total").and_then(|x| x.as_u64()).unwrap_or(0),
                tokens: v.get("tokens").and_then(|x| x.as_u64()).unwrap_or(0),
            };
            map.insert(k.clone(), row);
        }
        map
    }
}
