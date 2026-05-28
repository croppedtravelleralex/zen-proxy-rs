#[derive(Debug, Default, Clone)]
pub struct MarkdownFenceGuard {
    inside: Option<FenceMarker>,
    at_line_start: bool,
    lines_inside_fence: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FenceMarker {
    Backtick,
    Tilde,
}

impl MarkdownFenceGuard {
    pub fn new() -> Self {
        Self {
            inside: None,
            at_line_start: true,
            lines_inside_fence: 0,
        }
    }

    pub fn repair_text(text: &str) -> String {
        let mut guard = Self::new();
        let mut repaired = guard.push(text);
        repaired.push_str(&guard.finish());
        repaired
    }

    pub fn push(&mut self, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        let mut output = String::with_capacity(text.len() + 16);
        for segment in split_inclusive_newline(text) {
            output.push_str(&self.repair_segment(segment));
        }
        output
    }

    pub fn finish(&mut self) -> String {
        if let Some(marker) = self.inside.take() {
            self.at_line_start = true;
            self.lines_inside_fence = 0;
            format!("\n{}\n", marker.as_fence())
        } else {
            String::new()
        }
    }

    fn repair_segment(&mut self, segment: &str) -> String {
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        let mut output = String::new();

        if self.inside.is_some()
            && self.at_line_start
            && self.lines_inside_fence > 0
            && looks_like_document_boundary(line)
            && !looks_like_code_line(line)
        {
            output.push_str(self.inside.take().unwrap().as_fence());
            output.push('\n');
            self.lines_inside_fence = 0;
        }

        if let Some((prefix, marker)) = split_trailing_inline_fence(line, self.inside) {
            output.push_str(prefix);
            output.push('\n');
            output.push_str(marker.as_fence());
            if segment.ends_with('\n') {
                output.push('\n');
            }
            self.inside = None;
            self.at_line_start = segment.ends_with('\n');
            self.lines_inside_fence = 0;
            return output;
        }

        output.push_str(segment);
        self.observe_segment(segment);
        output
    }

    fn observe_segment(&mut self, segment: &str) {
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        if self.at_line_start {
            if let Some(marker) = independent_fence_marker(line) {
                if self.inside == Some(marker) {
                    self.inside = None;
                    self.lines_inside_fence = 0;
                } else if self.inside.is_none() {
                    self.inside = Some(marker);
                    self.lines_inside_fence = 0;
                }
            } else if self.inside.is_some() && !line.trim().is_empty() {
                self.lines_inside_fence = self.lines_inside_fence.saturating_add(1);
            }
        }
        self.at_line_start = segment.ends_with('\n');
    }
}

impl FenceMarker {
    fn as_fence(self) -> &'static str {
        match self {
            Self::Backtick => "```",
            Self::Tilde => "~~~",
        }
    }
}

fn split_inclusive_newline(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut parts = Vec::new();
    let mut start = 0usize;
    for (idx, ch) in text.char_indices() {
        if ch == '\n' {
            let end = idx + ch.len_utf8();
            parts.push(&text[start..end]);
            start = end;
        }
    }
    if start < text.len() {
        parts.push(&text[start..]);
    }
    parts
}

fn independent_fence_marker(line: &str) -> Option<FenceMarker> {
    let trimmed = line.trim();
    if trimmed.starts_with("```") {
        let rest = trimmed.trim_start_matches('`').trim();
        if rest.chars().all(is_fence_info_char) {
            return Some(FenceMarker::Backtick);
        }
    }
    if trimmed.starts_with("~~~") {
        let rest = trimmed.trim_start_matches('~').trim();
        if rest.chars().all(is_fence_info_char) {
            return Some(FenceMarker::Tilde);
        }
    }
    None
}

fn is_fence_info_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '+' | '#')
}

fn split_trailing_inline_fence(
    line: &str,
    inside: Option<FenceMarker>,
) -> Option<(&str, FenceMarker)> {
    let marker = inside?;
    let fence = marker.as_fence();
    let trimmed_end = line.trim_end();
    if !trimmed_end.ends_with(fence) {
        return None;
    }
    let pos = trimmed_end.rfind(fence)?;
    if pos == 0 {
        return None;
    }
    let prefix = &line[..pos];
    if prefix.trim().is_empty() {
        return None;
    }
    Some((prefix.trim_end(), marker))
}

fn looks_like_document_boundary(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("# ")
        || trimmed.starts_with("## ")
        || trimmed.starts_with("### ")
        || trimmed.starts_with("#### ")
        || trimmed.starts_with("- ")
        || trimmed.starts_with("1. ")
        || trimmed.starts_with("| ")
        || trimmed == "---"
}

fn looks_like_code_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    (trimmed.starts_with('#') && !trimmed.starts_with("# ") && !trimmed.starts_with("## "))
        || trimmed.starts_with("//")
        || trimmed.starts_with('{')
        || trimmed.starts_with('}')
        || trimmed.starts_with("let ")
        || trimmed.starts_with("const ")
        || trimmed.starts_with("fn ")
        || trimmed.starts_with("def ")
        || trimmed.starts_with("class ")
        || trimmed.starts_with("import ")
        || trimmed.starts_with("from ")
        || trimmed.contains("();")
        || trimmed.contains(" = ")
}

#[cfg(test)]
mod tests {
    use super::MarkdownFenceGuard;

    #[test]
    fn repairs_trailing_inline_closing_fence() {
        let text = "```text\nProcessBTCmd```\n## Result\n";
        let repaired = MarkdownFenceGuard::repair_text(text);
        assert_eq!(repaired, "```text\nProcessBTCmd\n```\n## Result\n");
    }

    #[test]
    fn closes_unclosed_fence_at_finish() {
        let text = "```text\nlog line";
        let repaired = MarkdownFenceGuard::repair_text(text);
        assert_eq!(repaired, "```text\nlog line\n```\n");
    }

    #[test]
    fn closes_before_document_boundary_after_suspicious_block() {
        let text = "```text\nplain output\n## Result\n";
        let repaired = MarkdownFenceGuard::repair_text(text);
        assert_eq!(repaired, "```text\nplain output\n```\n## Result\n");
    }

    #[test]
    fn preserves_valid_code_block() {
        let text = "```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n## Result\n";
        let repaired = MarkdownFenceGuard::repair_text(text);
        assert_eq!(repaired, text);
    }
}
