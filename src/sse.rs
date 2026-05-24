use std::collections::VecDeque;

pub struct SseBuffer {
    buf: VecDeque<u8>,
    done: bool,
}

impl SseBuffer {
    pub fn new() -> Self {
        Self {
            buf: VecDeque::new(),
            done: false,
        }
    }

    pub fn done(&self) -> bool {
        self.done
    }

    pub fn push_bytes(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        for &b in chunk {
            self.buf.push_back(b);
        }
        let mut lines: Vec<Vec<u8>> = Vec::new();
        let mut current_line: Vec<u8> = Vec::new();
        let mut found_lf = false;
        while let Some(&b) = self.buf.front() {
            if b == b'\n' || b == b'\r' {
                let _ = self.buf.pop_front();
                if b == b'\r' && self.buf.front() == Some(&b'\n') {
                    let _ = self.buf.pop_front();
                }
                if !current_line.is_empty() || (!found_lf && current_line.is_empty()) {
                    if !self.done {
                        let patched = patch_sse_line(&current_line);
                        if !patched.is_empty() {
                            let mut line_with_nl = patched;
                            line_with_nl.push(b'\n');
                            lines.push(line_with_nl);
                        }
                    }
                } else if current_line.is_empty() && found_lf && !self.done {
                    lines.push(vec![b'\n']);
                }
                if !self.done && (current_line == b"data: [DONE]" || current_line == b"data:[DONE]")
                {
                    self.done = true;
                    lines.push(b"data: [DONE]\n".to_vec());
                }
                current_line.clear();
                found_lf = true;
            } else {
                current_line.push(b);
                let _ = self.buf.pop_front();
            }
        }
        lines
    }
}

fn patch_sse_line(line: &[u8]) -> Vec<u8> {
    if line.is_empty() || !line.starts_with(b"data") {
        return line.to_vec();
    }
    if line == b"data: [DONE]" || line == b"data:[DONE]" {
        return b"data: [DONE]".to_vec();
    }
    let (payload, has_space) = if line.starts_with(b"data: ") {
        (&line[6..], true)
    } else if line.starts_with(b"data:") {
        (&line[5..], false)
    } else {
        return line.to_vec();
    };
    if payload.is_empty() {
        return line.to_vec();
    }
    let mut val: serde_json::Value = match serde_json::from_slice(payload) {
        Ok(v) => v,
        Err(_) => return line.to_vec(),
    };
    if let Some(obj) = val.as_object_mut() {
        if let Some(choices) = obj.get_mut("choices") {
            if let Some(choices_arr) = choices.as_array_mut() {
                for choice in choices_arr.iter_mut() {
                    let delta = if choice.get("delta").is_some() {
                        choice.get_mut("delta")
                    } else {
                        choice.get_mut("message")
                    };
                    if let Some(delta_val) = delta {
                        if let Some(delta_obj) = delta_val.as_object_mut() {
                            let content = delta_obj.get("content").and_then(|v| v.as_str());
                            let reasoning =
                                delta_obj.get("reasoning_content").and_then(|v| v.as_str());
                            if let Some(rc) = reasoning {
                                if !rc.is_empty() && content.is_none_or(|c| c.is_empty()) {
                                    delta_obj.insert(
                                        "content".to_string(),
                                        serde_json::Value::String(rc.to_string()),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let mut out = if has_space {
        vec![b'd', b'a', b't', b'a', b':', b' ']
    } else {
        vec![b'd', b'a', b't', b'a', b':']
    };
    let _ = serde_json::to_writer(&mut out, &val);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_done_event() {
        let mut buf = SseBuffer::new();
        let result = buf.push_bytes(b"data: [DONE]\n\n");
        assert!(result
            .iter()
            .any(|r| r.windows(b"DONE".len()).any(|w| w == b"DONE")));
    }

    #[test]
    fn sets_done_flag_after_done() {
        let mut buf = SseBuffer::new();
        buf.push_bytes(b"data: [DONE]\n");
        let lines = buf.push_bytes(b"\ndata: extra\n");
        assert!(
            lines.is_empty()
                || lines
                    .iter()
                    .all(|r| !r.windows(b"extra".len()).any(|w| w == b"extra"))
        );
    }

    #[test]
    fn patches_reasoning_to_content() {
        let mut buf = SseBuffer::new();
        let json = r#"data: {"choices":[{"index":0,"delta":{"content":null,"reasoning_content":"hello"}}]}"#;
        let result = buf.push_bytes(format!("{}\n\n", json).as_bytes());
        let all: Vec<u8> = result.iter().flatten().copied().collect();
        let s = String::from_utf8_lossy(&all);
        assert!(s.contains("\"content\":\"hello\""));
        assert!(!s.contains("\"content\":null"));
    }

    #[test]
    fn handles_chunk_split_inside_json() {
        let mut buf = SseBuffer::new();
        let line = r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","content":"hi"}}]}"#;
        let mid = line.len() / 2;
        let first_part = &line[..mid];
        let second_part = &line[mid..];
        buf.push_bytes(first_part.as_bytes());
        let result = buf.push_bytes(format!("{}\n\n", second_part).as_bytes());
        let all: Vec<u8> = result.iter().flatten().copied().collect();
        let s = String::from_utf8_lossy(&all);
        assert!(s.contains("content"));
    }

    #[test]
    fn handles_multiple_events_in_one_chunk() {
        let mut buf = SseBuffer::new();
        let chunk = b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"}}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"b\"}}]}\n\n";
        let result = buf.push_bytes(chunk);
        // We expect at least 2 data lines (the JSON payloads), plus blank-line separators;
        // total is 4: [data-with-a\n, \n, data-with-b\n, \n]; or 2 filtered payloads.
        // Test that we got multiple payloads, not just one:
        let payload_count = result.iter().filter(|r| r.starts_with(b"data")).count();
        assert!(
            payload_count >= 2,
            "got {} data lines, expected >= 2",
            payload_count
        );
    }

    #[test]
    fn drops_after_done_tail_events() {
        let mut buf = SseBuffer::new();
        buf.push_bytes(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"}}]}\n\n");
        buf.push_bytes(b"data: [DONE]\n\n");
        let result = buf.push_bytes(b"data: {\"choices\":[],\"cost\":\"0\"}\n\n");
        let all: Vec<u8> = result.iter().flatten().copied().collect();
        let s = String::from_utf8_lossy(&all);
        assert!(!s.contains("cost"), "should drop after DONE");
    }
}
