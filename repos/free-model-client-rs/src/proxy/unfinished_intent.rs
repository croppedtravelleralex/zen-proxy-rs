//! Detect a short “I'll check/wait next” announcement that was treated as a
//! finished answer. Claude Code then stops instead of issuing the tool call.
//!
//! Reproduced from session `270aa06e-8a07-4d35-b3b8-3d731a8f4f56`:
//! `命令超时。检查输出文件是否已生成` with `stop_reason=end_turn` and no `tool_use`.

use crate::client_profile::{ClientKind, ClientProfile};
use crate::protocol::types::ChatRequest;

const MAX_UNFINISHED_INTENT_CHARS: usize = 64;

const PENDING_ZH: &[&str] = &[
    "命令超时",
    "检查输出",
    "检查文件",
    "是否已生成",
    "再等",
    "一会儿取",
    "我去看",
    "我去查",
    "我去检查",
    "让我检查",
    "让我看",
    "稍后检查",
    "看一下输出",
    "先看一下",
    "继续等",
    "继续检查",
];

const PENDING_EN: &[&str] = &[
    "let me check",
    "let me look",
    "i'll check",
    "i will check",
    "i'll look",
    "checking the",
    "check if the",
    "check whether",
    "command timed out",
    "timed out",
    "wait and check",
];

const DONE_ZH: &[&str] = &[
    "如下",
    "汇总",
    "结论",
    "已完成",
    "已删除",
    "检查完成",
    "结果如下",
];

const DONE_EN: &[&str] = &["here is", "summary:", "completed", "already done"];

pub fn request_offers_tools(request: &ChatRequest) -> bool {
    request
        .tools
        .as_ref()
        .is_some_and(|tools| !tools.is_empty())
}

pub fn looks_like_unfinished_tool_intent(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.chars().count() > MAX_UNFINISHED_INTENT_CHARS {
        return false;
    }
    if has_any(trimmed, DONE_ZH) || has_any_ascii(trimmed, DONE_EN) {
        return false;
    }
    has_any(trimmed, PENDING_ZH) || has_any_ascii(trimmed, PENDING_EN)
}

pub fn should_hold_unfinished_tool_intent(
    profile: ClientProfile,
    request: &ChatRequest,
    accumulated_text: &str,
    tool_calls_empty: bool,
) -> bool {
    profile.kind == ClientKind::ClaudeCode
        && tool_calls_empty
        && request_offers_tools(request)
        && looks_like_unfinished_tool_intent(accumulated_text)
}

pub fn should_retry_unfinished_tool_intent(
    profile: ClientProfile,
    attempt: usize,
    max_attempts: usize,
    text: &str,
    tool_calls_empty: bool,
    request: &ChatRequest,
) -> bool {
    profile.kind == ClientKind::ClaudeCode
        && attempt + 1 < max_attempts
        && tool_calls_empty
        && request_offers_tools(request)
        && looks_like_unfinished_tool_intent(text)
}

/// Feed a newly available visible chunk. Returns text now safe to emit
/// (previously held prefix + chunk), or `None` if the prefix should stay held.
pub fn push_visible_chunk(
    held: &mut String,
    emitted: &str,
    chunk: &str,
    profile: ClientProfile,
    request: &ChatRequest,
    tool_calls_empty: bool,
) -> Option<String> {
    if chunk.is_empty() {
        return None;
    }
    let mut candidate = String::with_capacity(emitted.len() + held.len() + chunk.len());
    candidate.push_str(emitted);
    candidate.push_str(held);
    candidate.push_str(chunk);
    if should_hold_unfinished_tool_intent(profile, request, &candidate, tool_calls_empty) {
        held.push_str(chunk);
        return None;
    }
    let mut flush = String::with_capacity(held.len() + chunk.len());
    flush.push_str(held);
    flush.push_str(chunk);
    held.clear();
    Some(flush)
}

pub fn progress_text<'a>(emitted: &'a str, held: &'a str) -> &'a str {
    if emitted.trim().is_empty() {
        held
    } else {
        emitted
    }
}

pub fn combined_visible_text(emitted: &str, held: &str) -> String {
    if held.is_empty() {
        return emitted.to_string();
    }
    if emitted.is_empty() {
        return held.to_string();
    }
    let mut combined = String::with_capacity(emitted.len() + held.len());
    combined.push_str(emitted);
    combined.push_str(held);
    combined
}

fn has_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn has_any_ascii(text: &str, needles: &[&str]) -> bool {
    let lowered = text.to_ascii_lowercase();
    needles.iter().any(|needle| lowered.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_profile::ClientProfileSource;
    use crate::protocol::types::{OpenAITool, OpenAIToolFunction};

    fn claude() -> ClientProfile {
        ClientProfile::new(ClientKind::ClaudeCode, ClientProfileSource::Header)
    }

    fn other() -> ClientProfile {
        ClientProfile::new(ClientKind::Unknown, ClientProfileSource::Header)
    }

    fn request_with_bash() -> ChatRequest {
        ChatRequest {
            model: "deepseek-v4-flash".into(),
            messages: Vec::new(),
            stream: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: Some(vec![OpenAITool {
                tool_type: "function".into(),
                function: OpenAIToolFunction {
                    name: "Bash".into(),
                    description: Some("run".into()),
                    parameters: None,
                },
            }]),
            tool_choice: None,
        }
    }

    fn request_without_tools() -> ChatRequest {
        ChatRequest {
            tools: None,
            ..request_with_bash()
        }
    }

    #[test]
    fn detects_personal_session_fake_end_turn() {
        assert!(looks_like_unfinished_tool_intent(
            "命令超时。检查输出文件是否已生成"
        ));
    }

    #[test]
    fn detects_wait_then_fetch_announcement() {
        assert!(looks_like_unfinished_tool_intent("再等一会儿取结果"));
    }

    #[test]
    fn detects_english_let_me_check() {
        assert!(looks_like_unfinished_tool_intent(
            "Command timed out. Let me check the output file."
        ));
    }

    #[test]
    fn ignores_complete_disk_report() {
        let report = "后台扫描已完成，基线数据齐全。用户已中断检查约 2 小时了，现在直接汇总收尾，不再开新扫描。";
        assert!(!looks_like_unfinished_tool_intent(report));
    }

    #[test]
    fn ignores_short_ok_and_done() {
        assert!(!looks_like_unfinished_tool_intent("ok"));
        assert!(!looks_like_unfinished_tool_intent("好的。"));
        assert!(!looks_like_unfinished_tool_intent("检查完成"));
        assert!(!looks_like_unfinished_tool_intent(""));
    }

    #[test]
    fn hold_and_retry_only_for_claude_code_with_tools() {
        let profile = claude();
        let body = request_with_bash();
        let text = "命令超时。检查输出文件是否已生成";
        assert!(should_hold_unfinished_tool_intent(
            profile, &body, text, true
        ));
        assert!(should_retry_unfinished_tool_intent(
            profile, 0, 3, text, true, &body
        ));
        assert!(!should_retry_unfinished_tool_intent(
            profile, 2, 3, text, true, &body
        ));
        assert!(!should_hold_unfinished_tool_intent(
            profile, &body, text, false
        ));
        assert!(!should_hold_unfinished_tool_intent(
            other(),
            &body,
            text,
            true
        ));
        assert!(!should_hold_unfinished_tool_intent(
            profile,
            &request_without_tools(),
            text,
            true
        ));
    }

    #[test]
    fn holds_short_intent_then_flushes_when_it_grows() {
        let profile = claude();
        let body = request_with_bash();
        let mut held = String::new();
        assert_eq!(
            push_visible_chunk(
                &mut held,
                "",
                "命令超时。检查输出文件是否已生成",
                profile,
                &body,
                true
            ),
            None
        );
        assert_eq!(held, "命令超时。检查输出文件是否已生成");
        let flushed = push_visible_chunk(
            &mut held,
            "",
            "。C 盘可用 174.9 GB，下面按风险分级列出剩余可删项，全部基于本次已采集数据，不再等待后台扫描。",
            profile,
            &body,
            true,
        )
        .expect("grown text should flush");
        assert!(flushed.starts_with("命令超时。检查输出文件是否已生成"));
        assert!(held.is_empty());
    }

    #[test]
    fn flushes_held_prefix_once_a_tool_arrives() {
        let profile = claude();
        let body = request_with_bash();
        let mut held = String::new();
        assert!(push_visible_chunk(
            &mut held,
            "",
            "再等一会儿取结果\n\n",
            profile,
            &body,
            true
        )
        .is_none());
        let flushed = push_visible_chunk(&mut held, "", "x", profile, &body, false)
            .expect("tool call should release hold");
        assert!(flushed.starts_with("再等一会儿取结果"));
        assert!(held.is_empty());
    }
}
