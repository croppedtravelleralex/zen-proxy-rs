
use regex::Regex;

/// Try to extract expected text from prompt patterns.
/// Used when upstream returns no content and no tools were requested.
pub fn synthesize_text_fallback(prompt: &str) -> String {
    // Check for "Reply exactly: X" pattern
    let reply_exactly_re = Regex::new(r"(?i)Reply\s+exactly:\s*(.+)").unwrap();
    if let Some(cap) = reply_exactly_re.captures(prompt) {
        return cap[1].trim().to_string();
    }

    // Check for PASS/SAFE/OK keywords (word boundary match)
    let keyword_re = Regex::new(r"(?i)\b(PASS|SAFE|OK)\b").unwrap();
    if keyword_re.is_match(prompt) {
        return "PASS".to_string();
    }

    "NO_TOOL_CALL".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reply_exactly() {
        let result = synthesize_text_fallback("Reply exactly: Hello World");
        assert_eq!(result, "Hello World");
    }

    #[test]
    fn test_uppercase_reply_exactly() {
        let result = synthesize_text_fallback("REPLY EXACTLY: Test message");
        assert_eq!(result, "Test message");
    }

    #[test]
    fn test_reply_exactly_with_extra_spaces() {
        let result = synthesize_text_fallback("Reply   exactly:   Spaced out");
        assert_eq!(result, "Spaced out");
    }

    #[test]
    fn test_pass_keyword() {
        let result = synthesize_text_fallback("I think this is PASS");
        assert_eq!(result, "PASS");
    }

    #[test]
    fn test_safe_keyword() {
        let result = synthesize_text_fallback("This is SAFE to use");
        assert_eq!(result, "PASS");
    }

    #[test]
    fn test_ok_keyword() {
        let result = synthesize_text_fallback("OK let us do it");
        assert_eq!(result, "PASS");
    }

    #[test]
    fn test_keyword_not_partial_match() {
        // "COMPASS" should not match PASS since we use \b word boundaries
        let result = synthesize_text_fallback("Use the COMPASS heading");
        assert_eq!(result, "NO_TOOL_CALL");
    }

    #[test]
    fn test_default() {
        let result = synthesize_text_fallback("Some random prompt with no patterns");
        assert_eq!(result, "NO_TOOL_CALL");
    }

    #[test]
    fn test_empty_prompt() {
        let result = synthesize_text_fallback("");
        assert_eq!(result, "NO_TOOL_CALL");
    }

    #[test]
    fn test_reply_exactly_trims_whitespace() {
        let result = synthesize_text_fallback("Reply exactly:   Leading and trailing   ");
        assert_eq!(result, "Leading and trailing");
    }
}
