use axum::http::HeaderMap;

use crate::protocol::types::{AnthropicRequest, ChatRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    ClaudeCode,
    Hermes,
    OpenClaw,
    CherryStudio,
    OpenAiSdk,
    AnthropicSdk,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientProfileSource {
    Header,
    UserAgent,
    Body,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientProfile {
    pub kind: ClientKind,
    pub source: ClientProfileSource,
}

impl ClientProfile {
    pub fn new(kind: ClientKind, source: ClientProfileSource) -> Self {
        Self { kind, source }
    }

    pub fn unknown() -> Self {
        Self::new(ClientKind::Unknown, ClientProfileSource::Unknown)
    }

    pub fn from_openai(headers: &HeaderMap, request: &ChatRequest) -> Self {
        Self::from_headers(headers).unwrap_or_else(|| {
            infer_openai_from_body(request).unwrap_or_else(ClientProfile::unknown)
        })
    }

    pub fn from_anthropic(headers: &HeaderMap, request: &AnthropicRequest) -> Self {
        Self::from_headers(headers).unwrap_or_else(|| {
            infer_anthropic_from_body(request).unwrap_or_else(ClientProfile::unknown)
        })
    }

    pub fn disables_thinking_for_tool_use(self) -> bool {
        matches!(self.kind, ClientKind::Hermes | ClientKind::OpenClaw)
    }

    pub fn preserves_stream_whitespace(self) -> bool {
        matches!(self.kind, ClientKind::ClaudeCode)
    }

    pub fn uses_compat_tool_history(self) -> bool {
        matches!(self.kind, ClientKind::Hermes | ClientKind::OpenClaw)
    }

    pub fn protects_recovery_safe_markers(self) -> bool {
        matches!(
            self.kind,
            ClientKind::ClaudeCode | ClientKind::Hermes | ClientKind::OpenClaw
        )
    }

    fn from_headers(headers: &HeaderMap) -> Option<Self> {
        if let Some(kind) = headers
            .get("x-fmc-client")
            .and_then(|value| value.to_str().ok())
            .and_then(parse_client_kind)
        {
            return Some(Self::new(kind, ClientProfileSource::Header));
        }

        for header in [
            "user-agent",
            "x-client-name",
            "anthropic-client",
            "x-stainless-package-version",
        ] {
            if let Some(kind) = headers
                .get(header)
                .and_then(|value| value.to_str().ok())
                .and_then(infer_client_kind_from_text)
            {
                return Some(Self::new(kind, ClientProfileSource::UserAgent));
            }
        }

        None
    }
}

fn parse_client_kind(value: &str) -> Option<ClientKind> {
    let normalized = normalize(value);
    match normalized.as_str() {
        "claudecode" | "claudecodecli" => Some(ClientKind::ClaudeCode),
        "hermes" => Some(ClientKind::Hermes),
        "openclaw" => Some(ClientKind::OpenClaw),
        "cherrystudio" | "cherry" => Some(ClientKind::CherryStudio),
        "openaisdk" | "openai" => Some(ClientKind::OpenAiSdk),
        "anthropicsdk" | "anthropic" => Some(ClientKind::AnthropicSdk),
        "unknown" => Some(ClientKind::Unknown),
        _ => None,
    }
}

fn infer_client_kind_from_text(value: &str) -> Option<ClientKind> {
    let normalized = normalize(value);
    if normalized.contains("claudecode") {
        return Some(ClientKind::ClaudeCode);
    }
    if normalized.contains("hermes") {
        return Some(ClientKind::Hermes);
    }
    if normalized.contains("openclaw") {
        return Some(ClientKind::OpenClaw);
    }
    if normalized.contains("cherrystudio") {
        return Some(ClientKind::CherryStudio);
    }
    if normalized.contains("anthropic") {
        return Some(ClientKind::AnthropicSdk);
    }
    if normalized.contains("openai") {
        return Some(ClientKind::OpenAiSdk);
    }
    None
}

fn infer_openai_from_body(request: &ChatRequest) -> Option<ClientProfile> {
    infer_from_message_values(request.messages.iter().map(|message| &message.content)).or_else(
        || {
            let tool_names = request
                .tools
                .as_ref()?
                .iter()
                .map(|tool| tool.function.name.as_str());
            infer_from_tool_names(tool_names)
        },
    )
}

fn infer_anthropic_from_body(request: &AnthropicRequest) -> Option<ClientProfile> {
    infer_from_message_values(
        request
            .system
            .iter()
            .chain(request.messages.iter().map(|message| &message.content)),
    )
    .or_else(|| {
        let tool_names = request
            .tools
            .as_ref()?
            .iter()
            .map(|tool| tool.name.as_str());
        infer_from_tool_names(tool_names)
    })
}

fn infer_from_message_values<'a>(
    values: impl Iterator<Item = &'a serde_json::Value>,
) -> Option<ClientProfile> {
    for value in values {
        let text = value_to_text(value);
        if infer_client_kind_from_text(&text) == Some(ClientKind::OpenClaw) {
            return Some(ClientProfile::new(
                ClientKind::OpenClaw,
                ClientProfileSource::Body,
            ));
        }
        if infer_client_kind_from_text(&text) == Some(ClientKind::Hermes) {
            return Some(ClientProfile::new(
                ClientKind::Hermes,
                ClientProfileSource::Body,
            ));
        }
    }
    None
}

fn value_to_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(items) => items
            .iter()
            .map(value_to_text)
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::Value::Object(map) => map
            .values()
            .map(value_to_text)
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn infer_from_tool_names<'a>(tool_names: impl Iterator<Item = &'a str>) -> Option<ClientProfile> {
    let names = tool_names.map(normalize).collect::<Vec<_>>();
    if names.iter().any(|name| {
        matches!(
            name.as_str(),
            "subagents"
                | "sessionsspawn"
                | "sessionssend"
                | "sessionsyield"
                | "sessionstatus"
                | "sessionsstatus"
                | "sessionshistory"
                | "sessionslist"
                | "memoryget"
                | "memorysearch"
                | "webfetch"
                | "websearch"
        ) || name.contains("openclaw")
    }) {
        return Some(ClientProfile::new(
            ClientKind::OpenClaw,
            ClientProfileSource::Body,
        ));
    }
    if names.iter().any(|name| name.contains("hermes")) {
        return Some(ClientProfile::new(
            ClientKind::Hermes,
            ClientProfileSource::Body,
        ));
    }
    if names.iter().any(|name| {
        matches!(
            name.as_str(),
            "task"
                | "bash"
                | "read"
                | "edit"
                | "multiedit"
                | "write"
                | "todowrite"
                | "grep"
                | "glob"
                | "ls"
        )
    }) {
        return Some(ClientProfile::new(
            ClientKind::ClaudeCode,
            ClientProfileSource::Body,
        ));
    }
    None
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::{
        AnthropicMessage, AnthropicRequest, Message, OpenAITool, OpenAIToolFunction, ToolDef,
        ToolInputSchema,
    };
    use serde_json::Value;

    fn openai_request_with_tool(name: &str) -> ChatRequest {
        ChatRequest {
            model: "deepseek-v4-flash".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: Value::String("use tool".to_string()),
                tool_calls: None,
                tool_call_id: None,
            }],
            stream: Some(true),
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: Some(vec![OpenAITool {
                tool_type: "function".to_string(),
                function: OpenAIToolFunction {
                    name: name.to_string(),
                    description: None,
                    parameters: None,
                },
            }]),
            tool_choice: None,
        }
    }

    #[test]
    fn explicit_header_wins_over_body_heuristic() {
        let mut headers = HeaderMap::new();
        headers.insert("x-fmc-client", "openclaw".parse().unwrap());

        let profile = ClientProfile::from_openai(&headers, &openai_request_with_tool("Task"));

        assert_eq!(profile.kind, ClientKind::OpenClaw);
        assert_eq!(profile.source, ClientProfileSource::Header);
    }

    #[test]
    fn claude_code_tool_names_are_body_heuristic() {
        let headers = HeaderMap::new();

        let profile = ClientProfile::from_openai(&headers, &openai_request_with_tool("TodoWrite"));

        assert_eq!(profile.kind, ClientKind::ClaudeCode);
        assert_eq!(profile.source, ClientProfileSource::Body);
    }

    #[test]
    fn openclaw_toolset_wins_over_claude_like_tool_names() {
        let mut request = openai_request_with_tool("read");
        request.messages[0].role = "system".to_string();
        request.messages[0].content =
            Value::String("You are a personal assistant running inside OpenClaw.".to_string());
        request.tools = Some(vec![
            OpenAITool {
                tool_type: "function".to_string(),
                function: OpenAIToolFunction {
                    name: "read".to_string(),
                    description: None,
                    parameters: None,
                },
            },
            OpenAITool {
                tool_type: "function".to_string(),
                function: OpenAIToolFunction {
                    name: "subagents".to_string(),
                    description: None,
                    parameters: None,
                },
            },
            OpenAITool {
                tool_type: "function".to_string(),
                function: OpenAIToolFunction {
                    name: "sessions_spawn".to_string(),
                    description: None,
                    parameters: None,
                },
            },
        ]);

        let profile = ClientProfile::from_openai(&HeaderMap::new(), &request);

        assert_eq!(profile.kind, ClientKind::OpenClaw);
        assert_eq!(profile.source, ClientProfileSource::Body);
    }

    #[test]
    fn openclaw_anthropic_toolset_is_body_heuristic() {
        let request = AnthropicRequest {
            model: "deepseek-v4-flash".to_string(),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: Value::String("use subagent".to_string()),
            }],
            stream: Some(true),
            max_tokens: 1024,
            temperature: None,
            system: Some(Value::String(
                "You are a personal assistant running inside OpenClaw.".to_string(),
            )),
            tools: Some(vec![
                ToolDef {
                    name: "read".to_string(),
                    description: String::new(),
                    input_schema: ToolInputSchema {
                        schema_type: "object".to_string(),
                        required: None,
                        properties: None,
                    },
                },
                ToolDef {
                    name: "subagents".to_string(),
                    description: String::new(),
                    input_schema: ToolInputSchema {
                        schema_type: "object".to_string(),
                        required: None,
                        properties: None,
                    },
                },
            ]),
            tool_choice: None,
        };

        let profile = ClientProfile::from_anthropic(&HeaderMap::new(), &request);

        assert_eq!(profile.kind, ClientKind::OpenClaw);
        assert_eq!(profile.source, ClientProfileSource::Body);
    }
}
