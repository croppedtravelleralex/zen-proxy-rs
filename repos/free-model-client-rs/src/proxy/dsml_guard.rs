//! Detect DeepSeek DSML / invoke-XML tool markup leaking into assistant text.

const DSML_MARKERS: &[&str] = &[
    "DSML|tool_calls",
    "DSML｜tool_calls",
    "｜DSML｜",
    "|DSML|",
    "<｜DSML｜",
    "<|DSML|",
    "</｜DSML｜",
    "</|DSML|",
    "invoke name=",
    "<invoke name",
    "</invoke>",
    "<parameter name=",
    "</parameter>",
];

/// Prefixes that may arrive split across stream chunks before a full marker is visible.
const DSML_PREFIX_MARKERS: &[&str] = &[
    "<｜DSML｜",
    "<|DSML|",
    "</｜DSML｜",
    "</|DSML|",
    "<invoke",
    "invoke name=",
    "</invoke>",
    "<parameter",
    "</parameter",
    "DSML|",
    "DSML｜",
    "｜DSML｜",
    "|DSML|",
];

const MAX_PARTIAL_SUFFIX_HOLD: usize = 16;

pub fn contains_dsml_tool_leak(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    DSML_MARKERS.iter().any(|marker| text.contains(marker))
}

pub fn dsml_source_blob(reasoning: &str, text: &str) -> String {
    match (reasoning.is_empty(), text.is_empty()) {
        (true, true) => String::new(),
        (true, false) => text.to_string(),
        (false, true) => reasoning.to_string(),
        (false, false) => format!("{reasoning}\n{text}"),
    }
}

/// Bytes at the end of `text` that may be an incomplete DSML marker prefix.
pub fn partial_dsml_suffix_hold_len(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    if let Some(pos) = text.rfind("<invoke") {
        let tail = &text[pos..];
        if !tail.contains("invoke name") && !tail.contains("</invoke>") {
            return text.len() - pos;
        }
    }
    if let Some(pos) = text.rfind("<parameter") {
        let tail = &text[pos..];
        if !tail.contains("parameter name") && !tail.contains("</parameter>") {
            return text.len() - pos;
        }
    }
    let byte_len = text.len();
    let max_hold = MAX_PARTIAL_SUFFIX_HOLD.min(byte_len);
    for hold in 1..=max_hold {
        let start = byte_len - hold;
        if !text.is_char_boundary(start) {
            continue;
        }
        let suffix = &text[start..];
        for marker in DSML_PREFIX_MARKERS {
            if marker.starts_with(suffix) && suffix.len() < marker.len() {
                return hold;
            }
        }
        if suffix == "<" {
            return hold;
        }
    }
    0
}

/// Merge a stream chunk into `holdback`, emit only text that cannot be a DSML prefix,
/// and report whether a full DSML leak was detected.
pub fn take_emittable_text(holdback: &mut String, chunk: &str) -> (Option<String>, bool) {
    if chunk.is_empty() {
        return (None, false);
    }
    holdback.push_str(chunk);
    if contains_dsml_tool_leak(holdback.as_str()) {
        return (None, true);
    }
    let hold = partial_dsml_suffix_hold_len(holdback.as_str());
    if hold > 0 {
        let emit_len = holdback.len() - hold;
        if emit_len == 0 {
            return (None, false);
        }
        let emit = holdback[..emit_len].to_string();
        *holdback = holdback[emit_len..].to_string();
        return (Some(emit), false);
    }
    let emit = holdback.clone();
    holdback.clear();
    (Some(emit), false)
}

/// Release leftover holdback at end of stream. A trailing leak is reported rather than emitted.
pub fn flush_holdback(holdback: &mut String) -> (Option<String>, bool) {
    if holdback.is_empty() {
        return (None, false);
    }
    if contains_dsml_tool_leak(holdback.as_str()) {
        return (None, true);
    }
    let emit = std::mem::take(holdback);
    if emit.is_empty() {
        (None, false)
    } else {
        (Some(emit), false)
    }
}

pub fn raw_tool_format_label(tool_use_chunks: u64, dsml_leak: bool) -> &'static str {
    if tool_use_chunks > 0 {
        "anthropic"
    } else if dsml_leak {
        "dsml"
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_common_dsml_leak_markers() {
        assert!(contains_dsml_tool_leak("<｜DSML｜tool_calls>"));
        assert!(contains_dsml_tool_leak("|DSML|invoke name=Write|"));
        assert!(contains_dsml_tool_leak("<invoke name=\"Bash\">"));
        assert!(contains_dsml_tool_leak(
            "</｜DSML｜parameter>\n</｜DSML｜invoke>\n</｜DSML｜tool_calls>"
        ));
        assert!(contains_dsml_tool_leak(
            "<｜DSML｜parameter name=\"command\">pwd</｜DSML｜parameter>"
        ));
        assert!(!contains_dsml_tool_leak("修正 .gitignore 让 .env.example 可提交"));
        assert!(
            !contains_dsml_tool_leak("<command>ls</command>"),
            "bare HTML <command> must not be treated as DSML"
        );
        assert!(!contains_dsml_tool_leak("see the <prompt> in the docs"));
        assert!(!contains_dsml_tool_leak("<description>not a tool</description>"));
    }

    #[test]
    fn raw_tool_format_labels() {
        assert_eq!(raw_tool_format_label(1, false), "anthropic");
        assert_eq!(raw_tool_format_label(0, true), "dsml");
        assert_eq!(raw_tool_format_label(0, false), "");
    }

    #[test]
    fn holds_partial_dsml_prefix_across_chunks() {
        let mut hold = String::new();
        let (emit, leak) = take_emittable_text(&mut hold, "ok ");
        assert!(!leak);
        assert_eq!(emit.as_deref(), Some("ok "));

        let (emit, leak) = take_emittable_text(&mut hold, "<invoke name=");
        assert!(leak);
        assert!(emit.is_none());
        assert!(contains_dsml_tool_leak(&hold));
    }

    #[test]
    fn holds_incomplete_opener_before_invoke_name() {
        let mut hold = String::new();
        let (emit, leak) = take_emittable_text(&mut hold, "prefix <");
        assert!(!leak);
        assert_eq!(emit.as_deref(), Some("prefix "));
        assert_eq!(hold, "<");

        let (emit, leak) = take_emittable_text(&mut hold, "invoke");
        assert!(!leak);
        assert!(emit.is_none());
    }

    #[test]
    fn flush_holdback_emits_safe_suffix() {
        let mut hold = String::new();
        let (emit, leak) = take_emittable_text(&mut hold, "done <");
        assert!(!leak);
        assert_eq!(emit.as_deref(), Some("done "));
        assert_eq!(hold, "<");

        let (flush, leak) = flush_holdback(&mut hold);
        assert!(!leak);
        assert_eq!(flush.as_deref(), Some("<"));
        assert!(hold.is_empty());
    }

    #[test]
    fn flush_holdback_reports_leak_instead_of_emitting() {
        let mut hold = "</｜DSML｜invoke>".to_string();
        let (flush, leak) = flush_holdback(&mut hold);
        assert!(leak);
        assert!(flush.is_none());
        assert_eq!(hold, "</｜DSML｜invoke>");
    }

    #[test]
    fn partial_suffix_hold_safe_on_cjk_text() {
        assert_eq!(partial_dsml_suffix_hold_len("继续"), 0);
        assert_eq!(partial_dsml_suffix_hold_len("ok 继续"), 0);
        assert_eq!(partial_dsml_suffix_hold_len("修正 .gitignore"), 0);
    }

    #[test]
    fn take_emittable_text_cjk_chunks_do_not_panic() {
        let mut hold = String::new();
        for chunk in ["请", "继续", "执行"] {
            let (_, leak) = take_emittable_text(&mut hold, chunk);
            assert!(!leak);
        }
    }

    #[test]
    fn dsml_source_blob_joins_reasoning_and_text() {
        assert_eq!(dsml_source_blob("", ""), "");
        assert_eq!(dsml_source_blob("r", ""), "r");
        assert_eq!(dsml_source_blob("", "t"), "t");
        assert_eq!(dsml_source_blob("r", "t"), "r\nt");
    }
}
