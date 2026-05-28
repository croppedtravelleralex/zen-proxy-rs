use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::http::StatusCode;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::collector::ContextTelemetry;
use crate::config::{ArtifactCacheMode, CompactorMode, Config};

const MIB: usize = 1024 * 1024;
const PLACEHOLDER_HEAD_CHARS: usize = 2048;
const PLACEHOLDER_TAIL_CHARS: usize = 1024;
const MAX_TRACE_ITEMS: usize = 24;

#[derive(Debug, Clone)]
pub struct ContextPlan {
    pub body: Value,
    pub before: ContextProfile,
    pub after: ContextProfile,
    pub mode: CompactorMode,
    pub action: ContextAction,
    pub cache: ArtifactCacheStats,
    pub trace: Vec<String>,
}

impl ContextPlan {
    pub fn telemetry(&self) -> ContextTelemetry {
        ContextTelemetry {
            original_body_bytes: self.before.body_bytes,
            effective_body_bytes: self.after.body_bytes,
            estimated_prompt_tokens: self.after.estimated_prompt_tokens,
            message_count: self.before.message_count,
            tools_count: self.before.tools_count,
            largest_message_bytes: self.before.largest_message_bytes,
            tool_result_bytes: self.before.tool_result_bytes,
            mode: self.mode.to_string(),
            action: self.action.as_str().to_string(),
            trimmed: self.after.body_bytes < self.before.body_bytes,
            trimmed_bytes: self.before.body_bytes.saturating_sub(self.after.body_bytes),
            artifact_cache_mode: self.cache.mode.to_string(),
            artifact_cache_hits: self.cache.hits,
            artifact_cache_writes: self.cache.writes,
            trace: self.trace.clone(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ContextProfile {
    pub body_bytes: u64,
    pub estimated_prompt_tokens: u64,
    pub message_count: u32,
    pub tools_count: u32,
    pub largest_message_bytes: u64,
    pub tool_result_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextAction {
    Pass,
    Warn,
    ObserveCompact,
    Compact,
    CompactPartial,
}

impl ContextAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::ObserveCompact => "observe_compact",
            Self::Compact => "compact",
            Self::CompactPartial => "compact_partial",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ContextReject {
    pub status: StatusCode,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ArtifactCacheStats {
    pub mode: ArtifactCacheMode,
    pub hits: u32,
    pub writes: u32,
}

impl ArtifactCacheStats {
    fn new(mode: ArtifactCacheMode) -> Self {
        Self {
            mode,
            hits: 0,
            writes: 0,
        }
    }
}

pub fn govern_request(
    conf: &Config,
    path: &str,
    body: Value,
    original_body_bytes: usize,
) -> Result<ContextPlan, ContextReject> {
    let before = profile_request(path, &body, original_body_bytes);
    let mut trace = Vec::new();
    let mut cache = ArtifactCacheStats::new(conf.zen_artifact_cache_mode);
    let mode = conf.zen_compactor_mode;
    let needs_compact = should_compact(conf, &before);
    let needs_warn = should_warn(conf, &before);

    push_trace(
        &mut trace,
        format!(
            "profile body={} tokens={} messages={} tools={} tool_result_bytes={}",
            before.body_bytes,
            before.estimated_prompt_tokens,
            before.message_count,
            before.tools_count,
            before.tool_result_bytes
        ),
    );

    if mode == CompactorMode::Off {
        let action = if needs_warn {
            ContextAction::Warn
        } else {
            ContextAction::Pass
        };
        return Ok(ContextPlan {
            body,
            after: before.clone(),
            before,
            mode,
            action,
            cache,
            trace,
        });
    }

    if !needs_compact {
        let action = if needs_warn {
            ContextAction::Warn
        } else {
            ContextAction::Pass
        };
        return Ok(ContextPlan {
            body,
            after: before.clone(),
            before,
            mode,
            action,
            cache,
            trace,
        });
    }

    if mode == CompactorMode::Observe {
        push_trace(
            &mut trace,
            "observe mode: request would be compacted, body left unchanged",
        );
        return Ok(ContextPlan {
            body,
            after: before.clone(),
            before,
            mode,
            action: ContextAction::ObserveCompact,
            cache,
            trace,
        });
    }

    let target_bytes = target_body_bytes(conf);
    let target_tokens = conf.context_token_target.max(1);
    let mut compacted = body;

    compact_old_tool_results(conf, path, &mut compacted, &mut cache, &mut trace);
    let mut current = profile_request(path, &compacted, serialized_len(&compacted));
    if current.body_bytes > target_bytes || current.estimated_prompt_tokens > target_tokens {
        compact_old_large_text(conf, path, &mut compacted, &mut cache, &mut trace);
        current = profile_request(path, &compacted, serialized_len(&compacted));
    }
    if current.body_bytes > target_bytes {
        collapse_old_prefix(conf, &mut compacted, &mut trace);
        current = profile_request(path, &compacted, serialized_len(&compacted));
    }

    let upstream_limit = mb_to_bytes(conf.context_upstream_body_limit_mb.max(1)) as u64;
    if current.body_bytes > upstream_limit {
        return Err(ContextReject {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            message: format!(
                "request is too large after safe compaction: original={} effective={} upstream_limit={}",
                before.body_bytes, current.body_bytes, upstream_limit
            ),
        });
    }
    if current.estimated_prompt_tokens > target_tokens {
        return Err(ContextReject {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            message: format!(
                "request is too large after safe compaction: original_tokens={} effective_tokens={} token_target={}",
                before.estimated_prompt_tokens, current.estimated_prompt_tokens, target_tokens
            ),
        });
    }

    let action = if current.body_bytes <= target_bytes {
        ContextAction::Compact
    } else {
        ContextAction::CompactPartial
    };

    push_trace(
        &mut trace,
        format!(
            "compaction result body={} tokens={} trimmed={}",
            current.body_bytes,
            current.estimated_prompt_tokens,
            before.body_bytes.saturating_sub(current.body_bytes)
        ),
    );

    Ok(ContextPlan {
        body: compacted,
        before,
        after: current,
        mode,
        action,
        cache,
        trace,
    })
}

pub fn profile_request(path: &str, body: &Value, body_bytes: usize) -> ContextProfile {
    let messages = body.get("messages").and_then(Value::as_array);
    let message_count = messages.map(|items| items.len()).unwrap_or(0) as u32;
    let tools_count = body
        .get("tools")
        .and_then(Value::as_array)
        .map(|items| items.len())
        .unwrap_or(0) as u32;

    let largest_message_bytes = messages
        .map(|items| {
            items
                .iter()
                .map(|message| serialized_len(message) as u64)
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);

    let tool_result_bytes = messages
        .map(|items| items.iter().map(tool_result_bytes_in_message).sum())
        .unwrap_or(0);

    let mut estimated_prompt_tokens = estimate_value_tokens(body.get("system"));
    if let Some(items) = messages {
        estimated_prompt_tokens += items
            .iter()
            .map(|message| estimate_value_tokens(message.get("content")))
            .sum::<u64>();
    }
    if let Some(tools) = body.get("tools") {
        estimated_prompt_tokens += (serialized_len(tools) as u64).div_ceil(4);
    }

    if path == "messages" {
        estimated_prompt_tokens += (message_count as u64).saturating_mul(4);
    }

    ContextProfile {
        body_bytes: body_bytes as u64,
        estimated_prompt_tokens,
        message_count,
        tools_count,
        largest_message_bytes,
        tool_result_bytes,
    }
}

fn should_warn(conf: &Config, profile: &ContextProfile) -> bool {
    profile.body_bytes >= mb_to_bytes(conf.context_warn_body_mb) as u64
        || profile.estimated_prompt_tokens >= conf.context_token_warn
}

fn should_compact(conf: &Config, profile: &ContextProfile) -> bool {
    profile.body_bytes >= mb_to_bytes(conf.context_compact_body_mb) as u64
        || profile.estimated_prompt_tokens >= conf.context_token_compact
}

fn target_body_bytes(conf: &Config) -> u64 {
    let target = mb_to_bytes(conf.context_target_body_mb.max(1)) as u64;
    let upstream = mb_to_bytes(conf.context_upstream_body_limit_mb.max(1)) as u64;
    target.min(upstream.saturating_sub(512 * 1024).max(512 * 1024))
}

fn compact_old_tool_results(
    conf: &Config,
    path: &str,
    body: &mut Value,
    cache: &mut ArtifactCacheStats,
    trace: &mut Vec<String>,
) {
    let latest_user_idx = latest_user_message_index(body);
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };

    for (idx, message) in messages.iter_mut().enumerate() {
        if latest_user_idx.is_some_and(|latest| idx >= latest) || is_system_like_message(message) {
            continue;
        }
        let role_is_tool = message
            .get("role")
            .and_then(Value::as_str)
            .is_some_and(|role| role == "tool");
        let Some(content) = message.get_mut("content") else {
            continue;
        };
        if role_is_tool {
            trim_content_value(conf, path, content, "openai_tool_message", cache, trace);
            continue;
        }
        trim_tool_result_blocks(conf, path, content, cache, trace);
    }
}

fn compact_old_large_text(
    conf: &Config,
    path: &str,
    body: &mut Value,
    cache: &mut ArtifactCacheStats,
    trace: &mut Vec<String>,
) {
    let latest_user_idx = latest_user_message_index(body);
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };

    let threshold = conf
        .context_large_chunk_bytes
        .saturating_mul(2)
        .max(64 * 1024);
    for (idx, message) in messages.iter_mut().enumerate() {
        if latest_user_idx.is_some_and(|latest| idx >= latest) || is_system_like_message(message) {
            continue;
        }
        let Some(content) = message.get_mut("content") else {
            continue;
        };
        trim_large_text_value(
            conf,
            path,
            content,
            threshold,
            "old_message_text",
            cache,
            trace,
        );
    }
}

fn collapse_old_prefix(conf: &Config, body: &mut Value, trace: &mut Vec<String>) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    let preserve_recent = conf.context_preserve_recent_messages.max(1);
    if messages.len() <= preserve_recent + 2 {
        return;
    }

    let split_at = pair_aware_split_at(messages, messages.len().saturating_sub(preserve_recent));
    if split_at == 0 {
        push_trace(
            trace,
            "collapsed old prefix skipped: tool-call pair crossed preserve boundary",
        );
        return;
    }
    let old_prefix = messages.drain(0..split_at).collect::<Vec<_>>();
    let old_bytes = serialized_len(&Value::Array(old_prefix.clone()));
    let old_hash = sha256_hex(
        serde_json::to_vec(&Value::Array(old_prefix.clone()))
            .unwrap_or_default()
            .as_slice(),
    );

    let mut rebuilt = Vec::new();
    let mut omitted_count = 0usize;
    for message in old_prefix {
        if is_system_like_message(&message) {
            rebuilt.push(message);
        } else {
            omitted_count += 1;
        }
    }

    if omitted_count > 0 {
        rebuilt.push(json!({
            "role": "user",
            "content": format!(
                "[ZenProxy context compactor: omitted {} older messages; bytes={}; sha256={}]",
                omitted_count,
                old_bytes,
                short_hash(&old_hash)
            )
        }));
    }
    rebuilt.append(messages);
    *messages = rebuilt;

    push_trace(
        trace,
        format!(
            "collapsed old prefix omitted_messages={} old_bytes={}",
            omitted_count, old_bytes
        ),
    );
}

fn pair_aware_split_at(messages: &[Value], split_at: usize) -> usize {
    let mut adjusted = split_at.min(messages.len());
    adjusted = adjusted.min(openai_pair_aware_split_at(messages, adjusted));
    adjusted.min(anthropic_pair_aware_split_at(messages, adjusted))
}

fn openai_pair_aware_split_at(messages: &[Value], split_at: usize) -> usize {
    let mut recent_tool_ids = std::collections::HashSet::<String>::new();
    for message in messages.iter().skip(split_at) {
        if message
            .get("role")
            .and_then(Value::as_str)
            .is_some_and(|role| role == "tool")
        {
            if let Some(id) = message
                .get("tool_call_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
            {
                recent_tool_ids.insert(id.to_string());
            }
        }
    }
    if recent_tool_ids.is_empty() {
        return split_at;
    }

    let mut adjusted = split_at;
    for (idx, message) in messages.iter().take(split_at).enumerate() {
        let Some(calls) = message.get("tool_calls").and_then(Value::as_array) else {
            continue;
        };
        let has_recent_result = calls.iter().any(|call| {
            call.get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .is_some_and(|id| recent_tool_ids.contains(id))
        });
        if has_recent_result {
            adjusted = adjusted.min(idx);
        }
    }
    adjusted
}

fn anthropic_pair_aware_split_at(messages: &[Value], split_at: usize) -> usize {
    let mut recent_tool_ids = std::collections::HashSet::<String>::new();
    for message in messages.iter().skip(split_at) {
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for block in blocks {
            if block
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind == "tool_result")
            {
                if let Some(id) = block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                {
                    recent_tool_ids.insert(id.to_string());
                }
            }
        }
    }
    if recent_tool_ids.is_empty() {
        return split_at;
    }

    let mut adjusted = split_at;
    for (idx, message) in messages.iter().take(split_at).enumerate() {
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        let has_recent_result = blocks.iter().any(|block| {
            block
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind == "tool_use")
                && block
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .is_some_and(|id| recent_tool_ids.contains(id))
        });
        if has_recent_result {
            adjusted = adjusted.min(idx);
        }
    }
    adjusted
}

fn trim_tool_result_blocks(
    conf: &Config,
    path: &str,
    content: &mut Value,
    cache: &mut ArtifactCacheStats,
    trace: &mut Vec<String>,
) {
    let Value::Array(parts) = content else {
        return;
    };
    for part in parts {
        let is_tool_result = part
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind == "tool_result");
        if !is_tool_result {
            continue;
        }
        if let Some(value) = part.get_mut("content") {
            trim_content_value(conf, path, value, "anthropic_tool_result", cache, trace);
        }
    }
}

fn trim_content_value(
    conf: &Config,
    path: &str,
    value: &mut Value,
    kind: &str,
    cache: &mut ArtifactCacheStats,
    trace: &mut Vec<String>,
) {
    trim_large_text_value(
        conf,
        path,
        value,
        conf.context_large_chunk_bytes.max(1024),
        kind,
        cache,
        trace,
    );
}

fn trim_large_text_value(
    conf: &Config,
    path: &str,
    value: &mut Value,
    threshold: usize,
    kind: &str,
    cache: &mut ArtifactCacheStats,
    trace: &mut Vec<String>,
) {
    match value {
        Value::String(text) if text.len() > threshold => {
            let original = std::mem::take(text);
            let replacement = replacement_text(conf, path, kind, &original, cache, trace);
            *text = replacement;
        }
        Value::Array(parts) => {
            for part in parts {
                if let Some(Value::String(text)) = part.get_mut("text") {
                    if text.len() > threshold {
                        let original = std::mem::take(text);
                        let replacement =
                            replacement_text(conf, path, kind, &original, cache, trace);
                        *text = replacement;
                    }
                }
                if let Some(content) = part.get_mut("content") {
                    trim_large_text_value(conf, path, content, threshold, kind, cache, trace);
                }
            }
        }
        _ => {}
    }
}

fn replacement_text(
    conf: &Config,
    path: &str,
    kind: &str,
    original: &str,
    cache: &mut ArtifactCacheStats,
    trace: &mut Vec<String>,
) -> String {
    let hash = sha256_hex(original.as_bytes());
    let cache_status = maybe_store_artifact(conf, path, kind, &hash, original, cache);
    let head = neutralize_markdown_fences(&take_head(original, PLACEHOLDER_HEAD_CHARS));
    let tail = neutralize_markdown_fences(&take_tail(original, PLACEHOLDER_TAIL_CHARS));
    push_trace(
        trace,
        format!(
            "trimmed kind={} bytes={} sha256={} cache={}",
            kind,
            original.len(),
            short_hash(&hash),
            cache_status
        ),
    );
    format!(
        "[ZenProxy context compactor: omitted old {}; bytes={}; sha256={}; cache={}]\n{}\n...\n{}",
        kind,
        original.len(),
        short_hash(&hash),
        cache_status,
        head,
        tail
    )
}

fn maybe_store_artifact(
    conf: &Config,
    path: &str,
    kind: &str,
    hash: &str,
    original: &str,
    cache: &mut ArtifactCacheStats,
) -> &'static str {
    match conf.zen_artifact_cache_mode {
        ArtifactCacheMode::Off => "off",
        ArtifactCacheMode::Metadata | ArtifactCacheMode::Full => {
            let dir = PathBuf::from(&conf.artifact_cache_dir);
            let meta_path = dir.join(format!("{hash}.json"));
            let hit = meta_path.exists();
            if hit {
                cache.hits = cache.hits.saturating_add(1);
                return "hit";
            }
            cache.writes = cache.writes.saturating_add(1);

            let mode = conf.zen_artifact_cache_mode;
            let hash = hash.to_string();
            let kind = kind.to_string();
            let path = path.to_string();
            let bytes = original.len();
            let max_bytes = conf.artifact_cache_max_mb.saturating_mul(1024 * 1024);
            let ttl = Duration::from_secs(conf.artifact_cache_ttl_hours.saturating_mul(3600));
            let content = if mode == ArtifactCacheMode::Full {
                Some(original.to_string())
            } else {
                None
            };
            tokio::spawn(async move {
                let _ = write_artifact(ArtifactWrite {
                    dir,
                    hash,
                    kind,
                    path,
                    bytes,
                    content,
                    max_bytes,
                    ttl,
                })
                .await;
            });
            "write"
        }
    }
}

struct ArtifactWrite {
    dir: PathBuf,
    hash: String,
    kind: String,
    path: String,
    bytes: usize,
    content: Option<String>,
    max_bytes: u64,
    ttl: Duration,
}

async fn write_artifact(write: ArtifactWrite) -> std::io::Result<()> {
    let ArtifactWrite {
        dir,
        hash,
        kind,
        path,
        bytes,
        content,
        max_bytes,
        ttl,
    } = write;
    tokio::fs::create_dir_all(&dir).await?;
    let now = unix_secs();
    let has_content = content.is_some();
    let meta = json!({
        "sha256": hash,
        "kind": kind,
        "path": path,
        "bytes": bytes,
        "created_unix": now,
        "has_content": has_content
    });
    tokio::fs::write(
        dir.join(format!(
            "{}.json",
            meta["sha256"].as_str().unwrap_or_default()
        )),
        serde_json::to_vec(&meta).unwrap_or_default(),
    )
    .await?;
    if let Some(content) = content {
        tokio::fs::write(
            dir.join(format!(
                "{}.txt",
                meta["sha256"].as_str().unwrap_or_default()
            )),
            content,
        )
        .await?;
    }
    cleanup_cache_dir(&dir, max_bytes, ttl).await;
    Ok(())
}

async fn cleanup_cache_dir(dir: &Path, max_bytes: u64, ttl: Duration) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    let now = SystemTime::now();
    let mut files = Vec::new();
    let mut total = 0u64;

    while let Ok(Some(entry)) = entries.next_entry().await {
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(UNIX_EPOCH);
        let size = meta.len();
        if now
            .duration_since(modified)
            .is_ok_and(|age| age > ttl && ttl.as_secs() > 0)
        {
            let _ = tokio::fs::remove_file(entry.path()).await;
            continue;
        }
        total = total.saturating_add(size);
        files.push((modified, size, entry.path()));
    }

    if max_bytes == 0 || total <= max_bytes {
        return;
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    for (_, size, path) in files {
        if total <= max_bytes {
            break;
        }
        if tokio::fs::remove_file(&path).await.is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

fn preserve_from(body: &Value, preserve_recent: usize) -> usize {
    body.get("messages")
        .and_then(Value::as_array)
        .map(|messages| messages.len().saturating_sub(preserve_recent.max(1)))
        .unwrap_or(0)
}

fn latest_user_message_index(body: &Value) -> Option<usize> {
    body.get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages.iter().rposition(|message| {
                message
                    .get("role")
                    .and_then(Value::as_str)
                    .is_some_and(|role| role == "user")
            })
        })
}

fn is_system_like_message(message: &Value) -> bool {
    message
        .get("role")
        .and_then(Value::as_str)
        .is_some_and(|role| matches!(role, "system" | "developer"))
}

fn tool_result_bytes_in_message(message: &Value) -> u64 {
    let role_is_tool = message
        .get("role")
        .and_then(Value::as_str)
        .is_some_and(|role| role == "tool");
    let Some(content) = message.get("content") else {
        return 0;
    };
    if role_is_tool {
        return value_text_bytes(content);
    }
    match content {
        Value::Array(parts) => parts
            .iter()
            .filter(|part| {
                part.get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind == "tool_result")
            })
            .map(|part| value_text_bytes(part.get("content").unwrap_or(&Value::Null)))
            .sum(),
        _ => 0,
    }
}

fn value_text_bytes(value: &Value) -> u64 {
    match value {
        Value::String(text) => text.len() as u64,
        Value::Array(items) => items.iter().map(value_text_bytes).sum(),
        Value::Object(map) => map.values().map(value_text_bytes).sum(),
        _ => 0,
    }
}

fn estimate_value_tokens(value: Option<&Value>) -> u64 {
    match value {
        Some(Value::String(text)) => estimate_text_tokens(text),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                estimate_value_tokens(item.get("text"))
                    + estimate_value_tokens(item.get("content"))
                    + estimate_value_tokens(item.get("input"))
            })
            .sum(),
        Some(Value::Object(map)) => map
            .values()
            .map(|value| estimate_value_tokens(Some(value)))
            .sum(),
        _ => 0,
    }
}

fn estimate_text_tokens(text: &str) -> u64 {
    let word_like = text.split_whitespace().count() as u64;
    let char_like = (text.chars().count() as u64).div_ceil(4);
    word_like.max(char_like)
}

fn serialized_len(value: &Value) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

fn mb_to_bytes(mb: usize) -> usize {
    mb.saturating_mul(MIB)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    hex::encode(hash)
}

fn short_hash(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

fn take_head(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn take_tail(text: &str, max_chars: usize) -> String {
    let tail = text.chars().rev().take(max_chars).collect::<Vec<_>>();
    tail.into_iter().rev().collect()
}

fn neutralize_markdown_fences(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '`' && ch != '~' {
            output.push(ch);
            continue;
        }

        let mut run_len = 1usize;
        while chars.peek().is_some_and(|next| *next == ch) {
            chars.next();
            run_len += 1;
        }

        if run_len >= 3 {
            let marker = if ch == '`' { "backticks" } else { "tildes" };
            output.push_str(&format!("[markdown fence {} x{}]", marker, run_len));
        } else {
            for _ in 0..run_len {
                output.push(ch);
            }
        }
    }
    output
}

fn push_trace(trace: &mut Vec<String>, item: impl Into<String>) {
    if trace.len() < MAX_TRACE_ITEMS {
        trace.push(item.into());
    }
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ArtifactCacheMode, CompactorMode};

    fn test_config(mode: CompactorMode) -> Config {
        let mut cfg = Config::from_env();
        cfg.zen_compactor_mode = mode;
        cfg.zen_artifact_cache_mode = ArtifactCacheMode::Off;
        cfg.context_warn_body_mb = 1;
        cfg.context_compact_body_mb = 1;
        cfg.context_target_body_mb = 1;
        cfg.context_large_chunk_bytes = 1024;
        cfg.context_preserve_recent_messages = 2;
        cfg
    }

    #[test]
    fn profile_counts_tool_result_bytes() {
        let body = json!({
            "model": "deepseek-v4-flash",
            "tools": [{"name": "x"}],
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "tool", "content": "abc"},
                {"role": "user", "content": [{"type": "tool_result", "content": "abcdef"}]}
            ]
        });
        let profile = profile_request("chat/completions", &body, serialized_len(&body));
        assert_eq!(profile.message_count, 3);
        assert_eq!(profile.tools_count, 1);
        assert_eq!(profile.tool_result_bytes, 9);
        assert!(profile.estimated_prompt_tokens > 0);
    }

    #[test]
    fn observe_mode_keeps_body_unchanged() {
        let cfg = test_config(CompactorMode::Observe);
        let big = "x".repeat(2 * MIB);
        let body = json!({
            "model": "deepseek-v4-flash",
            "messages": [
                {"role": "tool", "content": big},
                {"role": "user", "content": "latest"}
            ]
        });
        let original = serialized_len(&body);
        let plan = govern_request(&cfg, "chat/completions", body, original).unwrap();
        assert_eq!(plan.action, ContextAction::ObserveCompact);
        assert_eq!(plan.before.body_bytes, plan.after.body_bytes);
        assert!(!plan.telemetry().trimmed);
    }

    #[test]
    fn enforce_mode_trims_old_tool_output_and_keeps_latest_message() {
        let cfg = test_config(CompactorMode::Enforce);
        let big = "x".repeat(2 * MIB);
        let body = json!({
            "model": "deepseek-v4-flash",
            "messages": [
                {"role": "tool", "content": big},
                {"role": "assistant", "content": "recent assistant"},
                {"role": "user", "content": "latest user"}
            ]
        });
        let original = serialized_len(&body);
        let plan = govern_request(&cfg, "chat/completions", body, original).unwrap();
        assert!(matches!(
            plan.action,
            ContextAction::Compact | ContextAction::CompactPartial
        ));
        assert!(plan.after.body_bytes < plan.before.body_bytes);
        let messages = plan.body["messages"].as_array().unwrap();
        assert_eq!(messages.last().unwrap()["content"], "latest user");
        assert!(messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("ZenProxy context compactor"));
    }

    #[test]
    fn enforce_mode_trims_large_tool_output_even_inside_recent_window() {
        let mut cfg = test_config(CompactorMode::Enforce);
        cfg.context_preserve_recent_messages = 8;
        let big = "x".repeat(2 * MIB);
        let body = json!({
            "model": "deepseek-v4-flash",
            "messages": [
                {"role": "tool", "content": big, "tool_call_id": "old-tool"},
                {"role": "assistant", "content": "recent assistant"},
                {"role": "user", "content": "latest user"}
            ]
        });
        let original = serialized_len(&body);
        let plan = govern_request(&cfg, "chat/completions", body, original).unwrap();
        assert!(plan.after.body_bytes < plan.before.body_bytes);
        let messages = plan.body["messages"].as_array().unwrap();
        assert_eq!(messages.last().unwrap()["content"], "latest user");
        assert!(messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("ZenProxy context compactor"));
    }

    #[test]
    fn compactor_placeholder_neutralizes_markdown_fences_in_previews() {
        let mut cfg = test_config(CompactorMode::Enforce);
        cfg.context_preserve_recent_messages = 8;
        let big = format!(
            "```text\nProcessBTCmd```\n{}\n~~~json\n{{}}\n~~~",
            "x".repeat(2 * MIB)
        );
        let body = json!({
            "model": "deepseek-v4-flash",
            "messages": [
                {"role": "tool", "content": big, "tool_call_id": "old-tool"},
                {"role": "assistant", "content": "recent assistant"},
                {"role": "user", "content": "latest user"}
            ]
        });
        let original = serialized_len(&body);
        let plan = govern_request(&cfg, "chat/completions", body, original).unwrap();
        let compacted = plan.body["messages"][0]["content"].as_str().unwrap();
        assert!(compacted.contains("ZenProxy context compactor"));
        assert!(compacted.contains("[markdown fence backticks x3]"));
        assert!(compacted.contains("[markdown fence tildes x3]"));
        assert!(!compacted.contains("```"));
        assert!(!compacted.contains("~~~"));
    }

    #[test]
    fn enforce_mode_rejects_uncompressible_latest_message_over_token_target() {
        let mut cfg = test_config(CompactorMode::Enforce);
        cfg.context_token_target = 100;
        cfg.context_token_compact = 100;
        let body = json!({
            "model": "deepseek-v4-flash",
            "messages": [
                {"role": "user", "content": "x".repeat(2 * 1024)}
            ]
        });
        let original = serialized_len(&body);
        let reject = govern_request(&cfg, "chat/completions", body, original).unwrap_err();
        assert_eq!(reject.status, StatusCode::PAYLOAD_TOO_LARGE);
        assert!(reject.message.contains("effective_tokens"));
    }

    #[test]
    fn enforce_mode_trims_old_large_text_inside_recent_window() {
        let mut cfg = test_config(CompactorMode::Enforce);
        cfg.context_preserve_recent_messages = 8;
        cfg.context_token_target = 1_000;
        cfg.context_token_compact = 100;
        let body = json!({
            "model": "deepseek-v4-flash",
            "messages": [
                {"role": "user", "content": "x".repeat(2 * MIB)},
                {"role": "assistant", "content": "old assistant"},
                {"role": "user", "content": "latest user"}
            ]
        });
        let original = serialized_len(&body);
        let plan = govern_request(&cfg, "chat/completions", body, original).unwrap();
        assert!(plan.after.body_bytes < plan.before.body_bytes);
        let messages = plan.body["messages"].as_array().unwrap();
        assert_eq!(messages.last().unwrap()["content"], "latest user");
        assert!(messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("ZenProxy context compactor"));
    }

    #[test]
    fn collapse_boundary_keeps_openai_tool_pair_together() {
        let messages = vec![
            json!({"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"Read","arguments":"{}"}}]}),
            json!({"role":"tool","tool_call_id":"call_1","content":"result"}),
            json!({"role":"user","content":"latest"}),
        ];

        assert_eq!(pair_aware_split_at(&messages, 1), 0);
    }

    #[test]
    fn collapse_boundary_keeps_anthropic_tool_pair_together() {
        let messages = vec![
            json!({"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{}}]}),
            json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"result"}]}),
            json!({"role":"user","content":"latest"}),
        ];

        assert_eq!(pair_aware_split_at(&messages, 1), 0);
    }
}
