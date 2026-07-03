//! Cache Control Plane (CCP): USK, ICP scope, feature flags.

use crate::protocol::{translate, types::ChatRequest};
use serde_json::Value;

const USK_VERSION: &str = "usk_v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CcpFlags {
    pub icp_enabled: bool,
    pub prompt_cache_key: bool,
    pub anthropic_breakpoints: bool,
    pub reasoning_sidecar: bool,
    pub trf_strict: bool,
}

impl CcpFlags {
    pub fn from_env() -> Self {
        Self {
            icp_enabled: env_flag("CCP_ICP_ENABLED", true),
            prompt_cache_key: env_flag("CCP_PROMPT_CACHE_KEY", true),
            anthropic_breakpoints: env_flag("CCP_ANTHROPIC_BP", true),
            reasoning_sidecar: env_flag("CCP_REASONING_SIDECAR", true),
            trf_strict: env_flag("CCP_TRF_STRICT", true),
        }
    }
}

fn env_flag(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(default)
}

#[derive(Debug, Clone)]
pub struct UskContext<'a> {
    pub api_key_id: &'a str,
    pub public_model: &'a str,
    pub upstream_model: &'a str,
    pub source_client: &'a str,
}

#[derive(Debug, Clone)]
pub struct IcpIdentity {
    pub usk: String,
    pub icp_scope: String,
    pub prefix_4k_hash: u64,
    pub prefix_32k_hash: u64,
    pub tools_epoch_id: String,
    pub affinity_routing_key: String,
    pub zen_session_id: String,
}

pub fn compute_icp_identity(request: &ChatRequest, ctx: &UskContext<'_>) -> IcpIdentity {
    let shape = translate::request_shape(request);
    let icp_scope =
        icp_scope_for_request(request, shape.estimated_total_tokens, shape.prefix_32k_hash);
    let usk = format!(
        "{USK_VERSION}:{}",
        short_hash16(&format!(
            "{}:{}:{}:{}:{}",
            ctx.api_key_id, ctx.upstream_model, ctx.public_model, ctx.source_client, icp_scope
        ))
    );
    let affinity_routing_key = format!(
        "{}:{}:{}:{}",
        ctx.upstream_model,
        ctx.public_model,
        normalize_bucket(ctx.source_client),
        short_hash16(&usk)
    );
    let zen_session_id = format!("ses_{}", short_hash16(&usk));
    let tools_epoch_id = short_hash16(&format!(
        "{}:{}",
        request.model,
        request
            .tools
            .as_ref()
            .map(|tools| serde_json::to_string(tools).unwrap_or_default())
            .unwrap_or_default()
    ));
    IcpIdentity {
        usk,
        icp_scope,
        prefix_4k_hash: shape.prefix_4k_hash,
        prefix_32k_hash: shape.prefix_32k_hash,
        tools_epoch_id,
        affinity_routing_key,
        zen_session_id,
    }
}

pub fn compute_icp_identity_from_body(body: &Value, ctx: &UskContext<'_>) -> Option<IcpIdentity> {
    let request = serde_json::from_value::<ChatRequest>(body.clone()).ok()?;
    Some(compute_icp_identity(&request, ctx))
}

pub fn api_key_id_for_cache(api_key: &str) -> String {
    short_hash16(api_key)
}

pub fn affinity_key_from_identity(
    identity: &IcpIdentity,
    path: &str,
    client_bucket: &str,
) -> String {
    format!(
        "{}:{}:{}",
        identity.affinity_routing_key, path, client_bucket
    )
}

pub fn apply_prompt_cache_key(body: &mut Value, identity: &IcpIdentity, flags: &CcpFlags) {
    if !flags.prompt_cache_key {
        return;
    }
    let Some(object) = body.as_object_mut() else {
        return;
    };
    object.insert(
        "prompt_cache_key".to_string(),
        Value::String(identity.usk.clone()),
    );
}

fn icp_scope_for_request(
    request: &ChatRequest,
    estimated_tokens: u64,
    prefix_32k_hash: u64,
) -> String {
    if estimated_tokens < 10_000 {
        return "normal".to_string();
    }
    let tools_hash = request
        .tools
        .as_ref()
        .map(|tools| short_hash16(&serde_json::to_string(tools).unwrap_or_default()))
        .unwrap_or_else(|| "none".to_string());
    let choice_hash = request
        .tool_choice
        .as_ref()
        .map(|choice| short_hash16(&serde_json::to_string(choice).unwrap_or_default()))
        .unwrap_or_else(|| "none".to_string());
    format!("icp:p32k:{prefix_32k_hash:016x}:tools{tools_hash}:choice{choice_hash}")
}

fn normalize_bucket(source_client: &str) -> &str {
    let trimmed = source_client.trim();
    if trimmed.is_empty() {
        "unknown"
    } else {
        trimmed
    }
}

fn short_hash16(input: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::Message;
    use serde_json::json;

    #[test]
    fn usk_stable_for_same_request() {
        let request = ChatRequest {
            model: "deepseek-v4-flash".into(),
            messages: vec![Message {
                role: "user".into(),
                content: json!("hello"),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            stream: Some(true),
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
        };
        let ctx = UskContext {
            api_key_id: "key-a",
            public_model: "deepseek-v4-flash",
            upstream_model: "deepseek-v4-flash-free",
            source_client: "claude-code",
        };
        let a = compute_icp_identity(&request, &ctx);
        let b = compute_icp_identity(&request, &ctx);
        assert_eq!(a.usk, b.usk);
        assert_eq!(a.affinity_routing_key, b.affinity_routing_key);
    }

    #[test]
    fn usk_changes_when_prefix_changes() {
        let ctx = UskContext {
            api_key_id: "key-a",
            public_model: "deepseek-v4-flash",
            upstream_model: "deepseek-v4-flash-free",
            source_client: "claude-code",
        };
        let mut request = ChatRequest {
            model: "deepseek-v4-flash".into(),
            messages: vec![Message {
                role: "user".into(),
                content: json!("hello"),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            stream: Some(true),
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
        };
        let first = compute_icp_identity(&request, &ctx).usk;
        request.messages.push(Message {
            role: "user".into(),
            content: json!("a".repeat(50_000)),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });
        let second = compute_icp_identity(&request, &ctx).usk;
        assert_ne!(first, second);
    }
}
