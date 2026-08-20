//! Repair DeepSeek DSML / invoke-XML tool leaks into ClaudeCode tool calls.

use serde_json::Value;

const HTML_PARAM_TAGS: &[&str] = &[
    "command",
    "description",
    "prompt",
    "subagent_type",
    "file_path",
    "content",
    "path",
    "query",
    "url",
    "pattern",
    "old_string",
    "new_string",
];

pub fn repair_invoke_leak(text: &str) -> Option<(String, String)> {
    let invoke_name = extract_invoke_name(text)?;
    let mut input = extract_named_parameters(text);
    for tag in HTML_PARAM_TAGS {
        if input.contains_key(*tag) {
            continue;
        }
        if let Some(value) = extract_element_content(text, tag) {
            input.insert((*tag).to_string(), Value::String(value));
        }
    }
    if input.is_empty() {
        return None;
    }
    let args = serde_json::to_string(&Value::Object(input)).ok()?;
    Some((invoke_name, args))
}

fn extract_invoke_name(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let needle = "invoke name=";
    let idx = lower.find(needle)?;
    let rest = text[idx + needle.len()..].trim_start();
    parse_attr_value(rest).map(|(name, _)| name).filter(|name| !name.is_empty())
}

fn parse_attr_value(rest: &str) -> Option<(String, usize)> {
    if rest.is_empty() {
        return None;
    }
    if rest.starts_with('"') {
        let inner = &rest[1..];
        let end = inner.find('"')?;
        return Some((inner[..end].trim().to_string(), end + 2));
    }
    if rest.starts_with('\'') {
        let inner = &rest[1..];
        let end = inner.find('\'')?;
        return Some((inner[..end].trim().to_string(), end + 2));
    }
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '>' || c == '|')
        .unwrap_or(rest.len());
    let name = rest[..end].trim();
    if name.is_empty() {
        None
    } else {
        Some((name.to_string(), end))
    }
}

fn extract_named_parameters(text: &str) -> serde_json::Map<String, Value> {
    let mut input = serde_json::Map::new();
    let lower = text.to_ascii_lowercase();
    let needle = "parameter name=";
    let mut from = 0usize;
    while from < lower.len() {
        let Some(rel) = lower[from..].find(needle) else {
            break;
        };
        let name_at = from + rel + needle.len();
        if name_at >= text.len() || !text.is_char_boundary(name_at) {
            break;
        }
        let rest = text[name_at..].trim_start();
        let trimmed = name_at + (text[name_at..].len() - rest.len());
        let Some((name, name_consumed)) = parse_attr_value(rest) else {
            from = name_at + 1;
            continue;
        };
        if name.is_empty() {
            from = name_at + 1;
            continue;
        }
        let after_name = &text[trimmed + name_consumed..];
        let Some(gt) = after_name.find('>') else {
            break;
        };
        let content_start = trimmed + name_consumed + gt + 1;
        if content_start > text.len() || !text.is_char_boundary(content_start) {
            break;
        }
        let Some(close_len) = parameter_close_offset(&text[content_start..]) else {
            from = content_start;
            continue;
        };
        let content = text[content_start..content_start + close_len].trim();
        if !content.is_empty() {
            input.insert(name, Value::String(content.to_string()));
        }
        from = content_start + close_len + 1;
    }
    input
}

fn parameter_close_offset(tail: &str) -> Option<usize> {
    let lower = tail.to_ascii_lowercase();
    let mut found: Option<usize> = None;
    for marker in [
        "</｜dsml｜parameter>",
        "</|dsml|parameter>",
        "</parameter>",
    ] {
        if let Some(idx) = lower.find(marker) {
            found = Some(found.map_or(idx, |cur| cur.min(idx)));
        }
    }
    // Fullwidth DSML close is not ascii-lowercased; search original too.
    for marker in ["</｜DSML｜parameter>", "</|DSML|parameter>"] {
        if let Some(idx) = tail.find(marker) {
            found = Some(found.map_or(idx, |cur| cur.min(idx)));
        }
    }
    found
}

fn extract_element_content(text: &str, tag: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let open = format!("<{tag}");
    let open_idx = lower.find(&open)?;
    let after_open = &text[open_idx..];
    let gt = after_open.find('>')?;
    let content_start = open_idx + gt + 1;
    let close = format!("</{tag}>");
    let tail_lower = text[content_start..].to_ascii_lowercase();
    let close_idx = tail_lower.find(&close.to_ascii_lowercase())?;
    let content = text[content_start..content_start + close_idx].trim();
    if content.is_empty() {
        None
    } else {
        Some(content.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repairs_agent_invoke_with_subagent_type() {
        let text = r#"<invoke name="Agent">
<description>修 STONE 挖石落包</description>
<prompt>修致命 bug</prompt>
<subagent_type>general-purpose</subagent_type>
</invoke>"#;
        let (name, args) = repair_invoke_leak(text).expect("repair");
        assert_eq!(name, "Agent");
        let parsed: Value = serde_json::from_str(&args).expect("json");
        assert_eq!(parsed["subagent_type"], "general-purpose");
        assert_eq!(parsed["description"], "修 STONE 挖石落包");
        assert_eq!(parsed["prompt"], "修致命 bug");
    }

    #[test]
    fn repairs_bash_invoke() {
        let text = r#"<invoke name="Bash">
<command>node --check app.js</command>
<description>syntax check</description>
</invoke>"#;
        let (name, args) = repair_invoke_leak(text).expect("repair");
        assert_eq!(name, "Bash");
        let parsed: Value = serde_json::from_str(&args).expect("json");
        assert_eq!(parsed["command"], "node --check app.js");
    }

    #[test]
    fn repairs_dsml_parameter_name_format() {
        let text = concat!(
            "<｜DSML｜tool_calls>",
            "<｜DSML｜invoke name=\"Bash\">",
            "<｜DSML｜parameter name=\"command\">pwd && ls</｜DSML｜parameter>",
            "<｜DSML｜parameter name=\"description\">list files</｜DSML｜parameter>",
            "</｜DSML｜invoke>",
            "</｜DSML｜tool_calls>",
        );
        let (name, args) = repair_invoke_leak(text).expect("repair");
        assert_eq!(name, "Bash");
        let parsed: Value = serde_json::from_str(&args).expect("json");
        assert_eq!(parsed["command"], "pwd && ls");
        assert_eq!(parsed["description"], "list files");
    }

    #[test]
    fn repairs_invoke_xml_parameter_name_format() {
        let text = r#"<invoke name="Read">
<parameter name="file_path">README.md</parameter>
</invoke>"#;
        let (name, args) = repair_invoke_leak(text).expect("repair");
        assert_eq!(name, "Read");
        let parsed: Value = serde_json::from_str(&args).expect("json");
        assert_eq!(parsed["file_path"], "README.md");
    }

    #[test]
    fn repairs_truncated_tail_with_subagent_type() {
        let text = "…<subagent_type>general-purpose</subagent_type>\n</invoke>";
        assert!(repair_invoke_leak(text).is_none());
        let partial = r#"<invoke name="Agent"><subagent_type>general-purpose</subagent_type></invoke>"#;
        let (_, args) = repair_invoke_leak(partial).expect("repair");
        let parsed: Value = serde_json::from_str(&args).expect("json");
        assert_eq!(parsed["subagent_type"], "general-purpose");
    }

    #[test]
    fn truncated_dsml_closing_tags_cannot_be_repaired() {
        let text = "</｜DSML｜parameter>\n</｜DSML｜invoke>\n</｜DSML｜tool_calls>";
        assert!(repair_invoke_leak(text).is_none());
    }
}
