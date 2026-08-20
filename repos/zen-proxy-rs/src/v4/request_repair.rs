use serde_json::Value;
use std::collections::HashSet;

/// Outcome of a single `repair_anthropic_request` call.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RepairReport {
    /// (index in tools array, name we assigned)
    pub repaired_tool_names: Vec<(usize, String)>,
    /// true when top-level `messages` is absent, null, or not an array
    pub missing_messages: bool,
    /// sorted top-level key names of the body (KEYS ONLY, never values) — for diagnostics
    pub top_level_keys: Vec<String>,
}

impl RepairReport {
    pub fn is_noop(&self) -> bool {
        self.repaired_tool_names.is_empty() && !self.missing_messages
    }

    /// Compact, log-safe one-line summary.  Returns `None` when `is_noop()`.
    ///
    /// Format (example):
    /// `"repaired_tool_names=2[3:SendMessage,18:ReadFile] missing_messages=true keys=[model,system,tools]"`
    pub fn summary(&self) -> Option<String> {
        if self.is_noop() {
            return None;
        }
        let pairs = self
            .repaired_tool_names
            .iter()
            .map(|(i, n)| format!("{i}:{n}"))
            .collect::<Vec<_>>()
            .join(",");
        let keys = self.top_level_keys.join(",");
        Some(format!(
            "repaired_tool_names={}[{}] missing_messages={} keys=[{}]",
            self.repaired_tool_names.len(),
            pairs,
            self.missing_messages,
            keys,
        ))
    }
}

/// Repairs `body` in place.  Safe to call on any JSON value — never panics.
///
/// On return, `body` has been modified so that every element of the top-level
/// `tools` array that lacked both a `name` and a `function` key now has a
/// synthesised, sanitised, unique `name` injected.  The `messages` field is
/// intentionally never synthesised; a body missing it will still fail
/// downstream deserialization, but the `RepairReport` makes the failure
/// diagnosable from audit logs without ever logging user content.
pub fn repair_anthropic_request(body: &mut Value) -> RepairReport {
    let Some(obj) = body.as_object_mut() else {
        return RepairReport::default();
    };

    let mut top_level_keys: Vec<String> = obj.keys().cloned().collect();
    top_level_keys.sort_unstable();

    let missing_messages = match obj.get("messages") {
        None | Some(Value::Null) => true,
        Some(v) => !v.is_array(),
    };

    let mut repaired_tool_names = Vec::new();

    if let Some(Value::Array(tools)) = obj.get_mut("tools") {
        // First pass (immutable): collect names that already exist so we avoid collisions.
        let mut used_names: HashSet<String> = tools
            .iter()
            .filter_map(|t| t.get("name")?.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();

        // Second pass (mutable): inject names where they are missing.
        for (idx, tool) in tools.iter_mut().enumerate() {
            let Some(tool_obj) = tool.as_object_mut() else {
                continue;
            };

            let has_name = tool_obj
                .get("name")
                .and_then(Value::as_str)
                .map(|s| !s.is_empty())
                .unwrap_or(false);

            // OpenAI-shape tool: has a `function` wrapper — leave it for ToolDef's
            // custom Deserialize, which already handles this shape correctly.
            if has_name || tool_obj.contains_key("function") {
                continue;
            }

            let description = tool_obj
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let input_schema = tool_obj.get("input_schema").cloned();

            let base = derive_tool_name(&description, input_schema.as_ref(), idx);
            let final_name = make_unique(&base, &used_names);

            used_names.insert(final_name.clone());
            tool_obj.insert("name".to_string(), Value::String(final_name.clone()));
            repaired_tool_names.push((idx, final_name));
        }
    }

    RepairReport {
        repaired_tool_names,
        missing_messages,
        top_level_keys,
    }
}

// ── Name derivation helpers ───────────────────────────────────────────────────

fn derive_tool_name(description: &str, input_schema: Option<&Value>, index: usize) -> String {
    // Rule a: first ATX heading (^\s*#{1,6}\s+.+)
    if let Some(h) = first_atx_heading(description) {
        if let Some(n) = sanitize_to_name(h) {
            return n;
        }
    }

    // Rule b: input_schema.title (accessed from raw JSON — ToolInputSchema struct does not
    // expose `title`, but the field may be present in the wire payload)
    if let Some(title) = input_schema
        .and_then(|s| s.get("title"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        if let Some(n) = sanitize_to_name(title) {
            return n;
        }
    }

    // Rule c: first non-empty line of description
    if let Some(line) = description.lines().map(str::trim).find(|s| !s.is_empty()) {
        if let Some(n) = sanitize_to_name(line) {
            return n;
        }
    }

    // Rule d: guaranteed non-empty fallback
    format!("tool_{index}")
}

/// Extract the first ATX heading text from a multiline string.
/// Returns a `&str` pointing into `s` — always on a valid char boundary.
fn first_atx_heading(s: &str) -> Option<&str> {
    for line in s.lines() {
        let stripped = line.trim_start();

        // Count leading '#' via bytes — '#' is ASCII so one byte == one char.
        let hash_count = stripped.bytes().take_while(|&b| b == b'#').count();
        if hash_count == 0 || hash_count > 6 {
            continue;
        }

        // Safe: each '#' occupies exactly 1 byte, so this index is always on a boundary.
        let after = &stripped[hash_count..];

        // The spec (CommonMark ATX heading) requires at least one whitespace after the hashes.
        if !after.starts_with(|c: char| c.is_ascii_whitespace()) {
            continue;
        }

        let text = after.trim();
        if !text.is_empty() {
            return Some(text);
        }
    }
    None
}

/// Sanitize `raw` into a valid tool name matching `^[a-zA-Z0-9_-]{1,64}$`.
/// Returns `None` when the result would be empty (caller falls through to the next rule).
///
/// Steps:
/// 1. Replace every char outside `[a-zA-Z0-9_-]` with `_`; collapse consecutive `_` into one.
/// 2. Trim leading/trailing `_` and `-`.
/// 3. Truncate to 64 chars **on a char boundary** — prevents the class of UTF-8 byte-slicing
///    panics (the same class that caused the `dsml_guard.rs` 502 storm on 2026-08-05).
fn sanitize_to_name(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len().min(256));
    // Held-back pending underscore: we delay emitting `_` until we see the next valid char,
    // which collapses any run of invalid chars (including `_` itself) into one `_`.
    let mut pending_under = false;

    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            if pending_under {
                out.push('_');
                pending_under = false;
            }
            out.push(ch);
        } else {
            // `_`, whitespace, CJK, emoji, punctuation — all become a pending underscore.
            pending_under = true;
        }
    }
    // Trailing pending underscore is intentionally not flushed; `trim_matches` below handles it.

    let trimmed = out.trim_matches(|c: char| c == '_' || c == '-');
    if trimmed.is_empty() {
        return None;
    }

    // All chars in `trimmed` are ASCII at this point (only `[a-zA-Z0-9_-]` survive), but we
    // use `.chars().take(64)` instead of `&trimmed[..64]` to make the char-boundary invariant
    // structural rather than a precondition that could silently erode under future changes.
    let result: String = trimmed.chars().take(64).collect();
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Return `base` if it is not in `used`, otherwise try `base_2`, `base_3`, … until a free slot
/// is found.  Each candidate is trimmed so the full name stays ≤ 64 chars.
fn make_unique(base: &str, used: &HashSet<String>) -> String {
    if !used.contains(base) {
        return base.to_owned();
    }
    for n in 2u32.. {
        let suffix = format!("_{n}");
        // `base` is all-ASCII after `sanitize_to_name`, so `.chars().count() == .len()`.
        let available = 64_usize.saturating_sub(suffix.len());
        let truncated: String = base.chars().take(available).collect();
        let candidate = format!("{truncated}{suffix}");
        if !used.contains(&candidate) {
            return candidate;
        }
    }
    // Unreachable: `used` is finite, so a unique suffix always exists within u32 range.
    unreachable!("make_unique exhausted all u32 suffixes — impossible with a finite set")
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Name derivation ───────────────────────────────────────────────────────

    #[test]
    fn name_from_atx_heading_in_description() {
        // Real-world production shape: "# SendMessage\n\nSend a message to another agent."
        let mut body = json!({
            "model": "claude-3",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{
                "description": "# SendMessage\n\nSend a message to another agent.",
                "input_schema": {"type": "object", "properties": {}}
            }]
        });
        let report = repair_anthropic_request(&mut body);
        assert_eq!(
            report.repaired_tool_names,
            vec![(0usize, "SendMessage".to_owned())]
        );
        assert_eq!(body["tools"][0]["name"], "SendMessage");
    }

    #[test]
    fn name_from_input_schema_title_when_no_heading() {
        let mut body = json!({
            "model": "m",
            "messages": [],
            "tools": [{
                "description": "A flat description with no ATX heading.",
                "input_schema": {"type": "object", "title": "MyTool"}
            }]
        });
        let report = repair_anthropic_request(&mut body);
        assert_eq!(report.repaired_tool_names[0].1, "MyTool");
    }

    #[test]
    fn name_from_first_non_empty_line_when_no_heading_or_title() {
        let mut body = json!({
            "model": "m",
            "messages": [],
            "tools": [{
                "description": "\n\nuse_this_line\n\nmore content",
                "input_schema": {"type": "object"}
            }]
        });
        let report = repair_anthropic_request(&mut body);
        assert_eq!(report.repaired_tool_names[0].1, "use_this_line");
    }

    #[test]
    fn name_fallback_to_tool_index_when_all_candidates_sanitize_to_empty() {
        // CJK-only description → sanitize yields None → fallback to tool_<index>
        let mut body = json!({
            "model": "m",
            "messages": [],
            "tools": [
                {"description": "你好世界", "input_schema": {"type": "object"}},
                {"description": "你好世界", "input_schema": {"type": "object"}}
            ]
        });
        let report = repair_anthropic_request(&mut body);
        // Both tools get their index as fallback name
        assert_eq!(report.repaired_tool_names[0].1, "tool_0");
        assert_eq!(report.repaired_tool_names[1].1, "tool_1");
    }

    // ── Leave-alone cases ─────────────────────────────────────────────────────

    #[test]
    fn tool_with_valid_name_is_left_untouched() {
        let mut body = json!({
            "model": "m",
            "messages": [],
            "tools": [{"name": "existing", "description": "...", "input_schema": {"type": "object"}}]
        });
        let original_name = body["tools"][0]["name"].clone();
        let report = repair_anthropic_request(&mut body);
        assert!(report.repaired_tool_names.is_empty());
        assert_eq!(body["tools"][0]["name"], original_name);
    }

    #[test]
    fn tool_with_function_key_openai_shape_is_left_untouched() {
        let mut body = json!({
            "model": "m",
            "messages": [],
            "tools": [{
                "function": {"name": "openai_fn", "description": "...", "parameters": {}}
            }]
        });
        let report = repair_anthropic_request(&mut body);
        assert!(report.repaired_tool_names.is_empty());
        assert!(body["tools"][0].get("function").is_some());
        // No `name` key was injected at the top level of this tool
        assert!(body["tools"][0].get("name").is_none());
    }

    // ── Unicode / UTF-8 safety ────────────────────────────────────────────────

    #[test]
    fn cjk_emoji_description_does_not_panic_and_yields_valid_name() {
        // Emoji and CJK before an ASCII word: only the ASCII word survives sanitization.
        let mut body = json!({
            "model": "m",
            "messages": [],
            "tools": [{
                "description": "# 🦀 RustTool 🔧",
                "input_schema": {"type": "object"}
            }]
        });
        let report = repair_anthropic_request(&mut body);
        let name = &report.repaired_tool_names[0].1;
        assert!(!name.is_empty());
        assert!(name.len() <= 64);
        assert!(
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "non-tool-name chars in: {name}"
        );
        assert_eq!(name, "RustTool");
    }

    #[test]
    fn truncation_at_multibyte_char_boundary_does_not_panic() {
        // 62 ASCII chars followed by a 4-byte emoji, then 10 more ASCII chars.
        // Total bytes in heading text: 62 + 4 + 10 = 76.
        // A naive `&s[..64]` on the raw string would cut the 4-byte emoji in half → panic.
        // Our sanitize path converts the emoji to `_` (ASCII) before any truncation.
        let heading_text = format!("{}{}{}", "A".repeat(62), "🎯", "Z".repeat(10));
        let description = format!("# {heading_text}");
        let mut body = json!({
            "model": "m",
            "messages": [],
            "tools": [{"description": description, "input_schema": {"type": "object"}}]
        });
        let report = repair_anthropic_request(&mut body);
        let name = &report.repaired_tool_names[0].1;
        assert!(name.len() <= 64, "name too long: {}", name.len());
        assert!(
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "non-tool-name chars in: {name}"
        );
    }

    // ── Sanitization edge cases ───────────────────────────────────────────────

    #[test]
    fn sanitize_spaces_punctuation_slashes_collapse_to_underscore() {
        assert_eq!(
            sanitize_to_name("  hello / world.tool  ").unwrap(),
            "hello_world_tool"
        );
    }

    #[test]
    fn sanitize_empty_and_only_invalid_chars_returns_none() {
        assert!(sanitize_to_name("").is_none());
        assert!(sanitize_to_name("___").is_none());
        assert!(sanitize_to_name("你好").is_none());
        assert!(sanitize_to_name("---").is_none());
        assert!(sanitize_to_name("   ").is_none());
    }

    #[test]
    fn sanitize_preserves_hyphen_and_underscore() {
        assert_eq!(sanitize_to_name("foo-bar_baz").unwrap(), "foo-bar_baz");
    }

    #[test]
    fn sanitize_collapses_consecutive_underscores() {
        assert_eq!(sanitize_to_name("a__b___c").unwrap(), "a_b_c");
    }

    #[test]
    fn sanitize_trims_leading_and_trailing_separators() {
        assert_eq!(sanitize_to_name("_-hello-_").unwrap(), "hello");
        assert_eq!(sanitize_to_name("---foo---").unwrap(), "foo");
    }

    #[test]
    fn sanitize_truncates_to_64_chars() {
        let long = "a".repeat(100);
        let result = sanitize_to_name(&long).unwrap();
        assert_eq!(result.len(), 64);
        assert!(result.chars().all(|c| c == 'a'));
    }

    // ── Collision handling ────────────────────────────────────────────────────

    #[test]
    fn collision_between_two_derived_names_produces_suffix_2() {
        let mut body = json!({
            "model": "m",
            "messages": [],
            "tools": [
                {"description": "# Foo", "input_schema": {"type": "object"}},
                {"description": "# Foo", "input_schema": {"type": "object"}}
            ]
        });
        let report = repair_anthropic_request(&mut body);
        assert_eq!(report.repaired_tool_names[0].1, "Foo");
        assert_eq!(report.repaired_tool_names[1].1, "Foo_2");
    }

    #[test]
    fn collision_handles_3_tools_with_same_derived_name() {
        let mut body = json!({
            "model": "m",
            "messages": [],
            "tools": [
                {"description": "# Dup", "input_schema": {"type": "object"}},
                {"description": "# Dup", "input_schema": {"type": "object"}},
                {"description": "# Dup", "input_schema": {"type": "object"}}
            ]
        });
        let report = repair_anthropic_request(&mut body);
        assert_eq!(report.repaired_tool_names[0].1, "Dup");
        assert_eq!(report.repaired_tool_names[1].1, "Dup_2");
        assert_eq!(report.repaired_tool_names[2].1, "Dup_3");
    }

    #[test]
    fn collision_with_existing_name_skips_to_next_available_suffix() {
        // Tool 0 already has name "Bar". Tool 1 derives "Bar" from heading.
        let mut body = json!({
            "model": "m",
            "messages": [],
            "tools": [
                {"name": "Bar", "description": "...", "input_schema": {"type": "object"}},
                {"description": "# Bar", "input_schema": {"type": "object"}}
            ]
        });
        let report = repair_anthropic_request(&mut body);
        assert_eq!(
            report.repaired_tool_names,
            vec![(1usize, "Bar_2".to_owned())]
        );
    }

    // ── `missing_messages` flag ───────────────────────────────────────────────

    #[test]
    fn missing_messages_true_for_absent_field() {
        let mut body = json!({"model": "m"});
        assert!(repair_anthropic_request(&mut body).missing_messages);
    }

    #[test]
    fn missing_messages_true_for_null_field() {
        let mut body = json!({"model": "m", "messages": null});
        assert!(repair_anthropic_request(&mut body).missing_messages);
    }

    #[test]
    fn missing_messages_true_for_non_array_value() {
        let mut body = json!({"model": "m", "messages": "not an array"});
        assert!(repair_anthropic_request(&mut body).missing_messages);

        let mut body2 = json!({"model": "m", "messages": 42});
        assert!(repair_anthropic_request(&mut body2).missing_messages);
    }

    #[test]
    fn missing_messages_false_for_populated_array() {
        let mut body = json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert!(!repair_anthropic_request(&mut body).missing_messages);
    }

    #[test]
    fn missing_messages_false_for_empty_array() {
        // An empty array IS an array — not considered missing.
        let mut body = json!({"model": "m", "messages": []});
        assert!(!repair_anthropic_request(&mut body).missing_messages);
    }

    // ── `top_level_keys` — sorted, no values ─────────────────────────────────

    #[test]
    fn top_level_keys_are_sorted_and_contain_no_values() {
        let mut body = json!({
            "model": "secret-model-name",
            "system": "you are a helpful assistant",
            "messages": [{"role": "user", "content": "SECRET_USER_CONTENT"}],
            "tools": []
        });
        let report = repair_anthropic_request(&mut body);
        assert_eq!(
            report.top_level_keys,
            vec!["messages", "model", "system", "tools"]
        );
        let joined = report.top_level_keys.join(",");
        // Values must never appear in the key list
        assert!(!joined.contains("secret-model-name"));
        assert!(!joined.contains("SECRET_USER_CONTENT"));
        assert!(!joined.contains("helpful assistant"));
    }

    // ── Non-object / degenerate inputs — no panic ────────────────────────────

    #[test]
    fn non_object_body_is_noop_no_panic() {
        for mut body in [json!(null), json!(42), json!("a string"), json!([1, 2, 3])] {
            let report = repair_anthropic_request(&mut body);
            assert!(report.is_noop(), "expected noop for {body}");
        }
    }

    #[test]
    fn tools_not_array_is_noop_no_panic() {
        let mut body = json!({"model": "m", "messages": [], "tools": "not an array"});
        let report = repair_anthropic_request(&mut body);
        assert!(report.repaired_tool_names.is_empty());
    }

    #[test]
    fn tool_elements_that_are_not_objects_are_skipped_no_panic() {
        let mut body = json!({
            "model": "m",
            "messages": [],
            "tools": [
                42,
                "string",
                null,
                ["nested", "array"],
                {"description": "# GoodTool", "input_schema": {"type": "object"}}
            ]
        });
        let report = repair_anthropic_request(&mut body);
        // Only the last element (index 4) is an object needing repair
        assert_eq!(report.repaired_tool_names.len(), 1);
        assert_eq!(
            report.repaired_tool_names[0],
            (4usize, "GoodTool".to_owned())
        );
    }

    #[test]
    fn tool_with_empty_string_name_is_repaired() {
        let mut body = json!({
            "model": "m",
            "messages": [],
            "tools": [{"name": "", "description": "# EmptyName", "input_schema": {"type": "object"}}]
        });
        let report = repair_anthropic_request(&mut body);
        assert_eq!(report.repaired_tool_names[0].1, "EmptyName");
    }

    // ── `is_noop()` and `summary()` ──────────────────────────────────────────

    #[test]
    fn is_noop_true_and_summary_none_when_nothing_to_repair() {
        let mut body = json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"name": "existing", "description": "...", "input_schema": {"type": "object"}}]
        });
        let report = repair_anthropic_request(&mut body);
        assert!(report.is_noop());
        assert!(report.summary().is_none());
    }

    #[test]
    fn summary_present_and_correct_when_tool_name_repaired() {
        let mut body = json!({
            "model": "m",
            "messages": [],
            "tools": [{"description": "# Foo", "input_schema": {"type": "object"}}]
        });
        let report = repair_anthropic_request(&mut body);
        assert!(!report.is_noop());
        let summary = report.summary().unwrap();
        assert!(
            summary.contains("repaired_tool_names=1[0:Foo]"),
            "unexpected summary: {summary}"
        );
        assert!(
            summary.contains("missing_messages=false"),
            "unexpected summary: {summary}"
        );
    }

    #[test]
    fn summary_present_when_messages_missing_no_tools_repaired() {
        // Mirrors the provider.rs test: body has `system` value "test" that must not leak.
        let mut body = json!({"model": "m", "system": "test"});
        let report = repair_anthropic_request(&mut body);
        assert!(!report.is_noop());
        let summary = report.summary().unwrap();
        assert!(
            summary.contains("missing_messages=true"),
            "unexpected summary: {summary}"
        );
        assert!(
            summary.contains("keys=[model,system]"),
            "unexpected summary: {summary}"
        );
        // Values must never appear
        assert!(
            !summary.contains("test"),
            "system value leaked into summary: {summary}"
        );
    }

    // ── Full integration: realistic ClaudeCode-style body ─────────────────────

    #[test]
    fn full_claudecode_body_repaired_and_deserializes_into_anthropic_request() {
        use free_model_client_rs::protocol::types::AnthropicRequest;

        fn make_tool(name: &str) -> serde_json::Value {
            json!({
                "name": name,
                "description": format!("Does the {name} operation."),
                "input_schema": {"type": "object", "properties": {}}
            })
        }

        // 18 tools with valid names (indices 0–17)
        let mut tools: Vec<serde_json::Value> =
            (0..18).map(|i| make_tool(&format!("Tool{i:02}"))).collect();

        // Tool at index 18: missing `name`, ATX heading matches the real production shape
        tools.push(json!({
            "description": "# SendMessage\n\nSend a message to another agent.\n\n```json\n{}\n```",
            "input_schema": {
                "type": "object",
                "properties": {
                    "recipient": {"type": "string"},
                    "message":   {"type": "string"}
                },
                "required": ["recipient", "message"]
            }
        }));

        // Tool at index 19: valid
        tools.push(make_tool("Cleanup"));

        let mut body = json!({
            "model": "claude-3-5-sonnet-20241022",
            "messages": [{"role": "user", "content": "please coordinate the agents"}],
            "tools": tools,
            "max_tokens": 8192
        });

        let report = repair_anthropic_request(&mut body);

        // Exactly one tool repaired
        assert_eq!(report.repaired_tool_names.len(), 1);
        assert_eq!(
            report.repaired_tool_names[0],
            (18usize, "SendMessage".to_owned())
        );
        assert_eq!(body["tools"][18]["name"], "SendMessage");
        assert!(!report.missing_messages);
        assert!(!report.is_noop());

        // The repaired body must round-trip through AnthropicRequest deserialization.
        let request: AnthropicRequest = serde_json::from_value(body)
            .expect("repaired body must deserialize into AnthropicRequest without error");

        let tools_out = request.tools.expect("tools field present");
        assert_eq!(tools_out.len(), 20);
        assert_eq!(tools_out[18].name, "SendMessage");
        assert_eq!(tools_out[19].name, "Cleanup");
    }
}
