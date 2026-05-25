use crate::collector::{RequestTelemetry, RequestTimings};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub fn new_telemetry() -> RequestTelemetry {
    let ts = unix_ms();
    RequestTelemetry {
        rid: Uuid::new_v4().to_string(),
        ts,
        external_request_id: String::new(),
        gateway: String::new(),
        gateway_channel_id: String::new(),
        model: String::new(),
        public_model: String::new(),
        upstream_model: String::new(),
        protocol: String::new(),
        client_id: String::new(),
        path: String::new(),
        method: String::new(),
        is_streaming: false,
        node_url: String::new(),
        selected_node_id: String::new(),
        selected_node_url_redacted: String::new(),
        observed_exit_ip: String::new(),
        outcome: String::new(),
        pool: String::new(),
        exit_ip: String::new(),
        status: 0,
        rate_limited: false,
        retry_count: 0,
        latency_total_ms: 0,
        upstream_ms: 0,
        ttft_ms: 0,
        timings: RequestTimings::default(),
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        bytes_sent: 0,
        bytes_received: 0,
        failure_kind: String::new(),
        failure_message: String::new(),
        retry_chain: Vec::new(),
        context: None,
    }
}

pub fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

pub fn today_ymd() -> u32 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let days = now / 86400;
    let mut y = 1970i64;
    let mut rem = days as i64;
    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if rem < days_in_year {
            break;
        }
        rem -= days_in_year;
        y += 1;
    }
    let month_days = if is_leap(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut m = 0usize;
    for (i, &md) in month_days.iter().enumerate() {
        if rem < md {
            m = i;
            break;
        }
        rem -= md;
    }
    let d = rem + 1;
    (y as u32) * 10000 + (m as u32 + 1) * 100 + d as u32
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}
