use std::collections::{HashMap, HashSet};
use std::time::Instant;

use axum::http::StatusCode;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::collector::ProtocolGuardTelemetry;
use crate::config::{Config, ProtocolGuardMode, ProtocolGuardOrphanPolicy};

const PREVIEW_CHARS: usize = 2048;

#[derive(Debug, Clone)]
pub struct ProtocolGuardReject {
    pub status: StatusCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardPhase {
    PreCompact,
    PostCompact,
}

#[derive(Debug, Clone)]
struct PendingCall {
    id: String,
    message_index: usize,
    used: bool,
}

pub fn raw_body_has_tool_markers(body: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(body) else {
        return true;
    };
    [
        "tool_calls",
        "tool_call_id",
        "tool_use",
        "tool_result",
        "tool_use_id",
        "input_schema",
        "\"tools\"",
        "\"role\":\"tool\"",
        "\"role\": \"tool\"",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

pub fn guard_body(
    conf: &Config,
    path: &str,
    body: &mut Value,
    source_client: &str,
    phase: GuardPhase,
    force_scan: bool,
) -> Result<ProtocolGuardTelemetry, ProtocolGuardReject> {
    let total_start = Instant::now();
    let mut telemetry = ProtocolGuardTelemetry {
        mode: conf.protocol_guard_mode.to_string(),
        source_client: source_client.to_string(),
        post_valid: true,
        quality_risk: "none".to_string(),
        ..ProtocolGuardTelemetry::default()
    };

    if conf.protocol_guard_mode == ProtocolGuardMode::Off {
        return Ok(telemetry);
    }
    if !force_scan && !body_has_tool_shape(path, body) {
        return Ok(telemetry);
    }

    let scan_start = Instant::now();
    let before_invalid = count_invalid(path, body);
    telemetry.scan_ms = scan_start.elapsed().as_millis() as u64;
    telemetry.pre_invalid = before_invalid > 0;
    telemetry.applied = telemetry.pre_invalid || force_scan;
    telemetry.message_count_before = message_count(body);

    if should_repair(conf.protocol_guard_mode) {
        let repair_start = Instant::now();
        if path == "messages" {
            repair_anthropic(conf, body, &mut telemetry);
        } else {
            repair_openai(conf, body, &mut telemetry);
        }
        telemetry.repair_ms = repair_start.elapsed().as_millis() as u64;
    }

    let validate_start = Instant::now();
    let after_invalid = count_invalid(path, body);
    telemetry.validate_ms = validate_start.elapsed().as_millis() as u64;
    telemetry.post_valid = after_invalid == 0;
    telemetry.message_count_after = message_count(body);
    telemetry.total_ms = total_start.elapsed().as_millis() as u64;

    if telemetry.total_ms > conf.protocol_guard_max_ms {
        raise_risk(&mut telemetry, "high");
    }
    if phase == GuardPhase::PostCompact
        && !telemetry.post_valid
        && matches!(
            conf.protocol_guard_mode,
            ProtocolGuardMode::Repair | ProtocolGuardMode::Strict
        )
    {
        return Err(ProtocolGuardReject {
            status: StatusCode::BAD_REQUEST,
            message: "request contains unrecoverable tool-call history after protocol guard"
                .to_string(),
        });
    }
    if conf.protocol_guard_mode == ProtocolGuardMode::Strict && telemetry.pre_invalid {
        return Err(ProtocolGuardReject {
            status: StatusCode::BAD_REQUEST,
            message: "request contains invalid tool-call history".to_string(),
        });
    }
    Ok(telemetry)
}

fn should_repair(mode: ProtocolGuardMode) -> bool {
    matches!(mode, ProtocolGuardMode::Repair)
}

fn body_has_tool_shape(path: &str, body: &Value) -> bool {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return false;
    };
    if path == "messages" {
        body.get("tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| !tools.is_empty())
            || messages.iter().any(|message| {
                content_blocks(message).is_some_and(|blocks| {
                    blocks.iter().any(|block| {
                        matches!(
                            block.get("type").and_then(Value::as_str),
                            Some("tool_use" | "tool_result")
                        )
                    })
                })
            })
    } else {
        messages.iter().any(|message| {
            message
                .get("role")
                .and_then(Value::as_str)
                .is_some_and(|role| role == "tool")
                || message.get("tool_calls").is_some()
        })
    }
}

fn count_invalid(path: &str, body: &Value) -> u32 {
    if path == "messages" {
        count_invalid_anthropic(body)
    } else {
        count_invalid_openai(body)
    }
}

fn count_invalid_openai(body: &Value) -> u32 {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return 0;
    };
    let mut invalid = 0u32;
    let mut pending = Vec::<PendingCall>::new();
    for (idx, message) in messages.iter().enumerate() {
        let role = message.get("role").and_then(Value::as_str);
        if role != Some("tool") && !pending.iter().all(|item| item.used) {
            invalid =
                invalid.saturating_add(pending.iter().filter(|item| !item.used).count() as u32);
            pending.clear();
        }
        match role {
            Some("assistant") => {
                if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        if let Some(id) = non_empty_str(call.get("id")) {
                            pending.push(PendingCall {
                                id: id.to_string(),
                                message_index: idx,
                                used: false,
                            });
                        } else {
                            invalid = invalid.saturating_add(1);
                        }
                    }
                }
            }
            Some("tool") => {
                let Some(id) = non_empty_str(message.get("tool_call_id")) else {
                    invalid = invalid.saturating_add(1);
                    continue;
                };
                if let Some(call) = pending.iter_mut().find(|item| item.id == id && !item.used) {
                    call.used = true;
                } else {
                    invalid = invalid.saturating_add(1);
                }
            }
            _ => {}
        }
    }
    invalid.saturating_add(pending.iter().filter(|item| !item.used).count() as u32)
}

fn repair_openai(conf: &Config, body: &mut Value, telemetry: &mut ProtocolGuardTelemetry) {
    let max_messages = conf.protocol_guard_max_graph_messages;
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    if messages.len() > max_messages {
        raise_risk(telemetry, "high");
    }

    let mut pending = Vec::<PendingCall>::new();
    for idx in 0..messages.len() {
        let role = messages[idx]
            .get("role")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        if role.as_deref() != Some("tool") && !pending.iter().all(|item| item.used) {
            downgrade_unresolved_openai_tool_calls(messages, &pending, telemetry);
            pending.clear();
        }
        match role.as_deref() {
            Some("assistant") => {
                let message = &mut messages[idx];
                let Some(calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut)
                else {
                    continue;
                };
                for (tool_idx, call) in calls.iter_mut().enumerate() {
                    ensure_openai_tool_call_shape(call, idx, tool_idx, conf, telemetry);
                    if let Some(id) = non_empty_str(call.get("id")) {
                        pending.push(PendingCall {
                            id: id.to_string(),
                            message_index: idx,
                            used: false,
                        });
                    }
                }
            }
            Some("tool") => {
                let message = &mut messages[idx];
                let current_id =
                    non_empty_str(message.get("tool_call_id")).map(ToString::to_string);
                let matched = match current_id {
                    Some(ref id) => mark_pending_used(&mut pending, id),
                    None => consume_next_pending(&mut pending),
                };
                match matched {
                    Some(id) => {
                        if message.get("tool_call_id").is_none()
                            || non_empty_str(message.get("tool_call_id")).is_none()
                        {
                            telemetry.missing_tool_call_id_count =
                                telemetry.missing_tool_call_id_count.saturating_add(1);
                            message["tool_call_id"] = Value::String(id);
                        }
                        telemetry.paired_tool_result_count =
                            telemetry.paired_tool_result_count.saturating_add(1);
                        if pending.iter().filter(|item| !item.used).count() > 1 {
                            raise_risk(telemetry, "medium");
                        } else {
                            raise_risk(telemetry, "low");
                        }
                    }
                    None => {
                        telemetry.orphan_tool_result_count =
                            telemetry.orphan_tool_result_count.saturating_add(1);
                        if conf.protocol_guard_orphan_policy == ProtocolGuardOrphanPolicy::Reject {
                            raise_risk(telemetry, "critical");
                        } else {
                            downgrade_openai_tool_message(message);
                            telemetry.downgraded_tool_result_count =
                                telemetry.downgraded_tool_result_count.saturating_add(1);
                            raise_risk(telemetry, "high");
                        }
                    }
                }
            }
            _ => {}
        }
    }
    downgrade_unresolved_openai_tool_calls(messages, &pending, telemetry);
}

fn ensure_openai_tool_call_shape(
    call: &mut Value,
    message_idx: usize,
    tool_idx: usize,
    conf: &Config,
    telemetry: &mut ProtocolGuardTelemetry,
) {
    let id_missing = non_empty_str(call.get("id")).is_none();
    if id_missing && conf.protocol_guard_synthetic_ids {
        let id = synthetic_openai_tool_id(message_idx, tool_idx, call);
        call["id"] = Value::String(id);
        telemetry.synthetic_tool_id_count = telemetry.synthetic_tool_id_count.saturating_add(1);
        raise_risk(telemetry, "low");
    }
    if call.get("type").is_none() {
        call["type"] = Value::String("function".to_string());
    }
    if call.get("function").is_none() || !call["function"].is_object() {
        call["function"] = json!({"name":"unknown_tool","arguments":"{}"});
    }
    if non_empty_str(call["function"].get("name")).is_none() {
        call["function"]["name"] = Value::String("unknown_tool".to_string());
    }
    if call["function"].get("arguments").is_none() {
        call["function"]["arguments"] = Value::String("{}".to_string());
    } else if !call["function"]["arguments"].is_string() {
        let arguments = serde_json::to_string(&call["function"]["arguments"]).unwrap_or_default();
        call["function"]["arguments"] = Value::String(arguments);
    }
}

fn downgrade_unresolved_openai_tool_calls(
    messages: &mut [Value],
    pending: &[PendingCall],
    telemetry: &mut ProtocolGuardTelemetry,
) {
    let mut unresolved = HashMap::<usize, HashSet<String>>::new();
    for call in pending.iter().filter(|item| !item.used) {
        unresolved
            .entry(call.message_index)
            .or_default()
            .insert(call.id.clone());
    }
    for (idx, ids) in unresolved {
        let Some(message) = messages.get_mut(idx) else {
            continue;
        };
        let Some(calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut) else {
            continue;
        };
        let before = calls.len();
        calls.retain(|call| non_empty_str(call.get("id")).is_some_and(|id| !ids.contains(id)));
        let removed = before.saturating_sub(calls.len());
        if removed > 0 {
            telemetry.orphan_assistant_call_count = telemetry
                .orphan_assistant_call_count
                .saturating_add(removed as u32);
            raise_risk(telemetry, "high");
        }
        if calls.is_empty() {
            if let Some(obj) = message.as_object_mut() {
                obj.remove("tool_calls");
                let content_empty = obj
                    .get("content")
                    .is_none_or(|value| value.is_null() || value == "");
                if content_empty {
                    obj.insert(
                        "content".to_string(),
                        Value::String(
                            "[Tool call recovered as plain context: matching tool result missing]"
                                .to_string(),
                        ),
                    );
                }
            }
        }
    }
}

fn downgrade_openai_tool_message(message: &mut Value) {
    let preview = value_preview(message.get("content").unwrap_or(&Value::Null));
    if let Some(obj) = message.as_object_mut() {
        obj.insert("role".to_string(), Value::String("user".to_string()));
        obj.remove("tool_call_id");
        obj.remove("tool_calls");
        obj.insert(
            "content".to_string(),
            Value::String(format!(
                "[Tool result recovered as plain context: original tool_call_id missing or invalid]\n{preview}"
            )),
        );
    }
}

fn count_invalid_anthropic(body: &Value) -> u32 {
    let mut invalid = count_invalid_anthropic_tools(body);
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return invalid;
    };
    let mut pending = Vec::<PendingCall>::new();
    for (idx, message) in messages.iter().enumerate() {
        let Some(blocks) = content_blocks(message) else {
            continue;
        };
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    if let Some(id) = non_empty_str(block.get("id")) {
                        pending.push(PendingCall {
                            id: id.to_string(),
                            message_index: idx,
                            used: false,
                        });
                    } else {
                        invalid = invalid.saturating_add(1);
                    }
                }
                Some("tool_result") => {
                    let Some(id) = non_empty_str(block.get("tool_use_id")) else {
                        invalid = invalid.saturating_add(1);
                        continue;
                    };
                    if let Some(call) = pending.iter_mut().find(|item| item.id == id && !item.used)
                    {
                        call.used = true;
                    } else {
                        invalid = invalid.saturating_add(1);
                    }
                }
                _ => {}
            }
        }
    }
    invalid.saturating_add(pending.iter().filter(|item| !item.used).count() as u32)
}

fn count_invalid_anthropic_tools(body: &Value) -> u32 {
    body.get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter(|tool| {
                    let Some(schema) = tool.get("input_schema") else {
                        return true;
                    };
                    if !schema.is_object() {
                        return true;
                    }
                    schema.get("type").and_then(Value::as_str) != Some("object")
                        || !schema.get("properties").is_some_and(Value::is_object)
                })
                .count() as u32
        })
        .unwrap_or(0)
}

fn repair_anthropic(conf: &Config, body: &mut Value, telemetry: &mut ProtocolGuardTelemetry) {
    repair_anthropic_tools(body, telemetry);

    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    if messages.len() > conf.protocol_guard_max_graph_messages {
        raise_risk(telemetry, "high");
    }

    let mut pending = Vec::<PendingCall>::new();
    for (idx, message) in messages.iter_mut().enumerate() {
        let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for (block_idx, block) in blocks.iter_mut().enumerate() {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    if non_empty_str(block.get("id")).is_none() && conf.protocol_guard_synthetic_ids
                    {
                        let id = synthetic_anthropic_tool_id(idx, block_idx, block);
                        block["id"] = Value::String(id);
                        telemetry.synthetic_tool_id_count =
                            telemetry.synthetic_tool_id_count.saturating_add(1);
                        raise_risk(telemetry, "low");
                    }
                    if let Some(id) = non_empty_str(block.get("id")) {
                        pending.push(PendingCall {
                            id: id.to_string(),
                            message_index: idx,
                            used: false,
                        });
                    }
                }
                Some("tool_result") => {
                    let current_id =
                        non_empty_str(block.get("tool_use_id")).map(ToString::to_string);
                    let matched = match current_id {
                        Some(ref id) => mark_pending_used(&mut pending, id),
                        None => consume_next_pending(&mut pending),
                    };
                    match matched {
                        Some(id) => {
                            if non_empty_str(block.get("tool_use_id")).is_none() {
                                telemetry.missing_tool_use_id_count =
                                    telemetry.missing_tool_use_id_count.saturating_add(1);
                                block["tool_use_id"] = Value::String(id);
                            }
                            telemetry.paired_tool_result_count =
                                telemetry.paired_tool_result_count.saturating_add(1);
                            raise_risk(telemetry, "low");
                        }
                        None => {
                            telemetry.orphan_tool_result_count =
                                telemetry.orphan_tool_result_count.saturating_add(1);
                            if conf.protocol_guard_orphan_policy
                                == ProtocolGuardOrphanPolicy::Reject
                            {
                                raise_risk(telemetry, "critical");
                            } else {
                                downgrade_anthropic_tool_result(block);
                                telemetry.downgraded_tool_result_count =
                                    telemetry.downgraded_tool_result_count.saturating_add(1);
                                raise_risk(telemetry, "high");
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    downgrade_unresolved_anthropic_tool_uses(messages, &pending, telemetry);
}

fn repair_anthropic_tools(body: &mut Value, telemetry: &mut ProtocolGuardTelemetry) {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };
    for tool in tools {
        let schema_is_valid = tool
            .get("input_schema")
            .is_some_and(|schema| schema.is_object());
        if !schema_is_valid {
            tool["input_schema"] = json!({
                "type": "object",
                "properties": {}
            });
            telemetry.applied = true;
            raise_risk(telemetry, "low");
            continue;
        }

        if tool["input_schema"].get("type").and_then(Value::as_str) != Some("object") {
            tool["input_schema"]["type"] = Value::String("object".to_string());
            telemetry.applied = true;
            raise_risk(telemetry, "low");
        }
        if !tool["input_schema"]
            .get("properties")
            .is_some_and(Value::is_object)
        {
            tool["input_schema"]["properties"] = json!({});
            telemetry.applied = true;
            raise_risk(telemetry, "low");
        }
    }
}

fn downgrade_unresolved_anthropic_tool_uses(
    messages: &mut [Value],
    pending: &[PendingCall],
    telemetry: &mut ProtocolGuardTelemetry,
) {
    let mut unresolved = HashMap::<usize, HashSet<String>>::new();
    for call in pending.iter().filter(|item| !item.used) {
        unresolved
            .entry(call.message_index)
            .or_default()
            .insert(call.id.clone());
    }
    for (idx, ids) in unresolved {
        let Some(blocks) = messages
            .get_mut(idx)
            .and_then(|message| message.get_mut("content"))
            .and_then(Value::as_array_mut)
        else {
            continue;
        };
        for block in blocks {
            if matches!(block.get("type").and_then(Value::as_str), Some("tool_use"))
                && non_empty_str(block.get("id")).is_some_and(|id| ids.contains(id))
            {
                *block = json!({
                    "type": "text",
                    "text": "[Tool use recovered as plain context: matching tool result missing]"
                });
                telemetry.orphan_assistant_call_count =
                    telemetry.orphan_assistant_call_count.saturating_add(1);
                raise_risk(telemetry, "high");
            }
        }
    }
}

fn downgrade_anthropic_tool_result(block: &mut Value) {
    let preview = value_preview(block.get("content").unwrap_or(&Value::Null));
    *block = json!({
        "type": "text",
        "text": format!(
            "[Tool result recovered as plain context: original tool_use_id missing or invalid]\n{preview}"
        )
    });
}

fn mark_pending_used(pending: &mut [PendingCall], id: &str) -> Option<String> {
    let call = pending
        .iter_mut()
        .find(|item| item.id == id && !item.used)?;
    call.used = true;
    Some(call.id.clone())
}

fn consume_next_pending(pending: &mut [PendingCall]) -> Option<String> {
    let call = pending.iter_mut().find(|item| !item.used)?;
    call.used = true;
    Some(call.id.clone())
}

fn content_blocks(message: &Value) -> Option<&Vec<Value>> {
    message.get("content").and_then(Value::as_array)
}

fn non_empty_str(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn message_count(body: &Value) -> u32 {
    body.get("messages")
        .and_then(Value::as_array)
        .map(|messages| messages.len() as u32)
        .unwrap_or(0)
}

fn synthetic_openai_tool_id(message_idx: usize, tool_idx: usize, call: &Value) -> String {
    let name = call
        .get("function")
        .and_then(|function| function.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("unknown_tool");
    let args = call
        .get("function")
        .and_then(|function| function.get("arguments"))
        .map(value_preview)
        .unwrap_or_default();
    format!(
        "call_zen_{}_{}_{}_{}",
        message_idx,
        tool_idx,
        short_hash(name.as_bytes()),
        short_hash(args.as_bytes())
    )
}

fn synthetic_anthropic_tool_id(message_idx: usize, block_idx: usize, block: &Value) -> String {
    let name = block
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("unknown_tool");
    let input = block.get("input").map(value_preview).unwrap_or_default();
    format!(
        "toolu_zen_{}_{}_{}_{}",
        message_idx,
        block_idx,
        short_hash(name.as_bytes()),
        short_hash(input.as_bytes())
    )
}

fn value_preview(value: &Value) -> String {
    match value {
        Value::String(text) => take_preview(text),
        Value::Array(items) => items
            .iter()
            .map(value_preview)
            .filter(|item| !item.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(_) => take_preview(&serde_json::to_string(value).unwrap_or_default()),
        Value::Null => String::new(),
        _ => value.to_string(),
    }
}

fn take_preview(text: &str) -> String {
    let mut out = text.chars().take(PREVIEW_CHARS).collect::<String>();
    if text.chars().count() > PREVIEW_CHARS {
        out.push_str("\n[truncated]");
    }
    out
}

fn short_hash(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    hex::encode(hash).chars().take(12).collect()
}

fn raise_risk(telemetry: &mut ProtocolGuardTelemetry, risk: &str) {
    fn rank(value: &str) -> u8 {
        match value {
            "critical" => 4,
            "high" => 3,
            "medium" => 2,
            "low" => 1,
            _ => 0,
        }
    }
    if rank(risk) > rank(&telemetry.quality_risk) {
        telemetry.quality_risk = risk.to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ArtifactCacheMode, CompactorMode, ProtocolGuardMode, ProtocolGuardOrphanPolicy,
    };

    fn cfg() -> Config {
        let mut cfg = Config::from_env();
        cfg.zen_compactor_mode = CompactorMode::Enforce;
        cfg.zen_artifact_cache_mode = ArtifactCacheMode::Off;
        cfg.protocol_guard_mode = ProtocolGuardMode::Repair;
        cfg.protocol_guard_orphan_policy = ProtocolGuardOrphanPolicy::Downgrade;
        cfg.protocol_guard_synthetic_ids = true;
        cfg
    }

    #[test]
    fn openai_tool_message_missing_id_is_paired() {
        let mut body = json!({
            "model": "deepseek-v4-flash",
            "messages": [
                {"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"Read","arguments":"{}"}}]},
                {"role":"tool","content":"ok"}
            ]
        });

        let telemetry = guard_body(
            &cfg(),
            "chat/completions",
            &mut body,
            "openclaw",
            GuardPhase::PreCompact,
            true,
        )
        .unwrap();

        assert_eq!(body["messages"][1]["tool_call_id"], "call_1");
        assert!(telemetry.post_valid);
        assert_eq!(telemetry.missing_tool_call_id_count, 1);
    }

    #[test]
    fn openai_missing_assistant_tool_call_id_gets_stable_id() {
        let mut body = json!({
            "model": "deepseek-v4-flash",
            "messages": [
                {"role":"assistant","content":null,"tool_calls":[{"type":"function","function":{"name":"Read","arguments":"{\"file\":\"a\"}"}}]},
                {"role":"tool","content":"ok"}
            ]
        });
        let mut second = body.clone();

        guard_body(
            &cfg(),
            "chat/completions",
            &mut body,
            "hermes",
            GuardPhase::PreCompact,
            true,
        )
        .unwrap();
        guard_body(
            &cfg(),
            "chat/completions",
            &mut second,
            "hermes",
            GuardPhase::PreCompact,
            true,
        )
        .unwrap();

        let id = body["messages"][0]["tool_calls"][0]["id"].as_str().unwrap();
        assert!(id.starts_with("call_zen_"));
        assert_eq!(second["messages"][0]["tool_calls"][0]["id"], id);
        assert_eq!(body["messages"][1]["tool_call_id"], id);
    }

    #[test]
    fn openai_orphan_tool_message_is_downgraded() {
        let mut body = json!({
            "model": "deepseek-v4-flash",
            "messages": [
                {"role":"tool","content":"orphan result"}
            ]
        });

        let telemetry = guard_body(
            &cfg(),
            "chat/completions",
            &mut body,
            "openclaw",
            GuardPhase::PostCompact,
            true,
        )
        .unwrap();

        assert_eq!(body["messages"][0]["role"], "user");
        assert!(body["messages"][0].get("tool_call_id").is_none());
        assert!(telemetry.post_valid);
        assert_eq!(telemetry.downgraded_tool_result_count, 1);
    }

    #[test]
    fn openai_intervening_user_breaks_tool_pair() {
        let mut body = json!({
            "model": "deepseek-v4-flash",
            "messages": [
                {"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"Read","arguments":"{}"}}]},
                {"role":"user","content":"ordinary text before result"},
                {"role":"tool","tool_call_id":"call_1","content":"late result"}
            ]
        });

        let telemetry = guard_body(
            &cfg(),
            "chat/completions",
            &mut body,
            "openclaw",
            GuardPhase::PreCompact,
            true,
        )
        .unwrap();

        assert!(body["messages"][0].get("tool_calls").is_none());
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][2]["role"], "user");
        assert!(body["messages"][2].get("tool_call_id").is_none());
        assert!(telemetry.post_valid);
        assert_eq!(telemetry.orphan_assistant_call_count, 1);
        assert_eq!(telemetry.downgraded_tool_result_count, 1);
    }

    #[test]
    fn anthropic_tool_result_missing_id_is_paired() {
        let mut body = json!({
            "model": "deepseek-v4-flash",
            "max_tokens": 100,
            "messages": [
                {"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{}}]},
                {"role":"user","content":[{"type":"tool_result","content":"ok"}]}
            ]
        });

        let telemetry = guard_body(
            &cfg(),
            "messages",
            &mut body,
            "hermes",
            GuardPhase::PreCompact,
            true,
        )
        .unwrap();

        assert_eq!(
            body["messages"][1]["content"][0]["tool_use_id"],
            Value::String("toolu_1".to_string())
        );
        assert!(telemetry.post_valid);
        assert_eq!(telemetry.missing_tool_use_id_count, 1);
    }

    #[test]
    fn anthropic_tool_missing_input_schema_gets_default_schema() {
        let mut body = json!({
            "model": "deepseek-v4-flash",
            "max_tokens": 100,
            "tools": [
                {"name": "Read", "description": "read a file"},
                {"name": "Write", "input_schema": {"type": "string"}}
            ],
            "messages": [
                {"role":"user","content":"hello"}
            ]
        });

        let telemetry = guard_body(
            &cfg(),
            "messages",
            &mut body,
            "hermes",
            GuardPhase::PreCompact,
            false,
        )
        .unwrap();

        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(body["tools"][0]["input_schema"]["properties"], json!({}));
        assert_eq!(body["tools"][1]["input_schema"]["type"], "object");
        assert_eq!(body["tools"][1]["input_schema"]["properties"], json!({}));
        assert!(telemetry.applied);
        assert!(telemetry.post_valid);
    }

    #[test]
    fn anthropic_orphan_tool_result_is_downgraded() {
        let mut body = json!({
            "model": "deepseek-v4-flash",
            "max_tokens": 100,
            "messages": [
                {"role":"user","content":[{"type":"tool_result","content":"orphan result"}]}
            ]
        });

        let telemetry = guard_body(
            &cfg(),
            "messages",
            &mut body,
            "hermes",
            GuardPhase::PostCompact,
            true,
        )
        .unwrap();

        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        assert!(telemetry.post_valid);
        assert_eq!(telemetry.downgraded_tool_result_count, 1);
    }
}
