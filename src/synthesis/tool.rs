use crate::protocol::types::{ChatRequest, OpenAITool, OpenAIToolFunction, ToolCall, ToolFunction};
use rand::Rng;
use regex::Regex;
use serde_json::Value;

fn rand_id() -> String {
    let mut rng = rand::thread_rng();
    let n: u64 = rng.gen();
    format!("call_{:016x}", n)
}

fn extract_text_content(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => {
            if s.is_empty() {
                None
            } else {
                Some(s.clone())
            }
        }
        Value::Array(arr) => {
            let texts: Vec<&str> = arr
                .iter()
                .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                .filter(|s| !s.is_empty())
                .collect();
            if texts.is_empty() {
                None
            } else {
                Some(texts.join("\n"))
            }
        }
        _ => content.as_str().filter(|s| !s.is_empty()).map(String::from),
    }
}

fn get_user_prompt(body: &ChatRequest) -> Option<String> {
    body.messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .and_then(|m| extract_text_content(&m.content))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn synthesize_tool_call(body: &ChatRequest) -> Option<ToolCall> {
    let prompt = get_user_prompt(body)?;
    let tools = body.tools.as_ref()?;
    let tool = choose_requested_tool(tools, &prompt)?;
    let input = synthesize_tool_input(&tool.function, &prompt);

    Some(ToolCall {
        id: Some(rand_id()),
        call_type: "function".to_string(),
        function: ToolFunction {
            name: tool.function.name.clone(),
            arguments: serde_json::to_string(&input).unwrap_or_default(),
        },
        index: Some(0),
    })
}

pub fn complete_tool_call(call: &ToolCall, body: &ChatRequest) -> ToolCall {
    let prompt = get_user_prompt(body).unwrap_or_default();
    let tools = body.tools.as_ref();

    let mut args: serde_json::Map<String, Value> =
        serde_json::from_str(&call.function.arguments).unwrap_or_default();

    let schema = if let Some(tools) = tools {
        tool_schema_for_call(tools, &call.function.name)
    } else {
        tool_schema_for_call(&[], &call.function.name)
    };

    let required: Vec<String> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let properties = schema.get("properties");

    for key in &required {
        if !args.contains_key(key) {
            let prop = properties.and_then(|p| p.get(key));
            let value = infer_value(key, prop, &prompt);
            args.insert(key.clone(), Value::String(value));
        }
    }

    ToolCall {
        id: call.id.clone(),
        call_type: call.call_type.clone(),
        function: ToolFunction {
            name: call.function.name.clone(),
            arguments: serde_json::to_string(&args).unwrap_or_default(),
        },
        index: call.index,
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn choose_requested_tool<'a>(tools: &'a [OpenAITool], prompt: &str) -> Option<&'a OpenAITool> {
    let prompt_lower = prompt.to_lowercase();
    tools
        .iter()
        .find(|tool| prompt_lower.contains(&tool.function.name.to_lowercase()))
        .or_else(|| tools.first())
}

fn tool_schema_for_call(tools: &[OpenAITool], name: &str) -> Value {
    let name_lower = name.to_lowercase();
    for tool in tools {
        if tool.function.name.to_lowercase() == name_lower {
            if let Some(ref params) = tool.function.parameters {
                return params.clone();
            }
        }
    }
    default_schema_for_name(&name_lower)
}

fn default_schema_for_name(name: &str) -> Value {
    match name {
        "read" => serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "The path to the file to read" }
            },
            "required": ["file_path"]
        }),
        "bash" => serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The command to execute" }
            },
            "required": ["command"]
        }),
        "task" => serde_json::json!({
            "type": "object",
            "properties": {
                "description": { "type": "string", "description": "Short description of the task" },
                "prompt": { "type": "string", "description": "The prompt for the subtask" },
                "subagent_type": { "type": "string", "description": "Type of subagent" }
            },
            "required": ["description", "prompt", "subagent_type"]
        }),
        _ => serde_json::json!({ "type": "object", "properties": {} }),
    }
}

fn synthesize_tool_input(func: &OpenAIToolFunction, prompt: &str) -> Value {
    let schema = func
        .parameters
        .as_ref()
        .cloned()
        .unwrap_or_else(|| default_schema_for_name(&func.name.to_lowercase()));

    let mut input = serde_json::Map::new();

    let required: Vec<String> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let properties = schema.get("properties");

    for key in &required {
        let prop = properties.and_then(|p| p.get(key));
        let value = infer_value(key, prop, prompt);
        input.insert(key.clone(), Value::String(value));
    }

    Value::Object(input)
}

fn infer_value(key: &str, _prop: Option<&Value>, prompt: &str) -> String {
    match key {
        "file_path" => infer_file_path(prompt),
        "command" => infer_command(prompt),
        "subagent_type" => infer_agent_type(prompt),
        "description" => infer_description(prompt),
        "prompt" => infer_prompt(prompt),
        _ => prompt.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Parameter inference functions
// ---------------------------------------------------------------------------

fn infer_file_path(prompt: &str) -> String {
    let quoted_re = Regex::new(r#"(?i)read\s+"([^"]+)""#).unwrap();
    if let Some(cap) = quoted_re.captures(prompt) {
        return cap[1].to_string();
    }
    let word_re = Regex::new(r"(?i)\bread\s+(\S+(?:\.\w+)?)").unwrap();
    if let Some(cap) = word_re.captures(prompt) {
        return cap[1].to_string();
    }
    "package.json".to_string()
}

fn infer_command(prompt: &str) -> String {
    let quoted_re = Regex::new(r#"(?i)run\s*:?\s*"([^"]+)""#).unwrap();
    if let Some(cap) = quoted_re.captures(prompt) {
        return cap[1].to_string();
    }
    let unquoted_re = Regex::new(r"(?i)run\s*:\s*(.+?)(?:\n|$)").unwrap();
    if let Some(cap) = unquoted_re.captures(prompt) {
        let cmd = cap[1].trim().to_string();
        if !cmd.is_empty() {
            return cmd;
        }
    }
    "pwd".to_string()
}

fn infer_agent_type(_prompt: &str) -> String {
    "general-purpose".to_string()
}

fn infer_description(prompt: &str) -> String {
    let first_line = prompt.lines().next().unwrap_or("").trim();
    if first_line.len() > 5 {
        return first_line.to_string();
    }
    "Execute task from user request".to_string()
}

fn infer_prompt(prompt: &str) -> String {
    if prompt.is_empty() {
        return "Complete the assigned task".to_string();
    }
    prompt.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::Message;

    fn make_tool(name: &str) -> OpenAITool {
        OpenAITool {
            tool_type: "function".to_string(),
            function: OpenAIToolFunction {
                name: name.to_string(),
                description: None,
                parameters: None,
            },
        }
    }

    fn make_chat_request(prompt: &str, tools: Option<Vec<OpenAITool>>) -> ChatRequest {
        ChatRequest {
            model: "big-pickle".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: Value::String(prompt.to_string()),
                tool_calls: None,
            }],
            stream: Some(false),
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools,
            tool_choice: None,
        }
    }

    #[test]
    fn test_extract_string_content() {
        let content = Value::String("hello world".to_string());
        assert_eq!(
            extract_text_content(&content),
            Some("hello world".to_string())
        );
    }

    #[test]
    fn test_extract_empty_string() {
        let content = Value::String("".to_string());
        assert_eq!(extract_text_content(&content), None);
    }

    #[test]
    fn test_extract_array_content() {
        let content = serde_json::json!([
            {"type": "text", "text": "Hello"},
            {"type": "text", "text": "World"}
        ]);
        assert_eq!(
            extract_text_content(&content),
            Some("Hello\nWorld".to_string())
        );
    }

    #[test]
    fn test_choose_tool_by_name_case_insensitive() {
        let tools = vec![make_tool("Read"), make_tool("Bash")];
        let result = choose_requested_tool(&tools, "please read package.json");
        assert!(result.is_some());
        assert_eq!(result.unwrap().function.name, "Read");
    }

    #[test]
    fn test_choose_tool_fallback_to_first() {
        let tools = vec![make_tool("Read"), make_tool("Bash")];
        let result = choose_requested_tool(&tools, "do something unknown");
        assert!(result.is_some());
        assert_eq!(result.unwrap().function.name, "Read");
    }

    #[test]
    fn test_choose_tool_empty_list() {
        let tools: Vec<OpenAITool> = vec![];
        let result = choose_requested_tool(&tools, "read file");
        assert!(result.is_none());
    }

    #[test]
    fn test_infer_file_path_quoted() {
        assert_eq!(infer_file_path("read \"package.json\""), "package.json");
    }

    #[test]
    fn test_infer_file_path_unquoted() {
        assert_eq!(infer_file_path("Read package.json"), "package.json");
    }

    #[test]
    fn test_infer_file_path_case_insensitive() {
        assert_eq!(infer_file_path("READ Cargo.toml please"), "Cargo.toml");
    }

    #[test]
    fn test_infer_file_path_default() {
        assert_eq!(infer_file_path("do something"), "package.json");
    }

    #[test]
    fn test_infer_command_quoted() {
        assert_eq!(infer_command("run \"ls -la\""), "ls -la");
    }

    #[test]
    fn test_infer_command_unquoted_with_colon() {
        assert_eq!(infer_command("Run: ls -la"), "ls -la");
    }

    #[test]
    fn test_infer_command_unquoted_no_colon() {
        assert_eq!(infer_command("run: pwd"), "pwd");
    }

    #[test]
    fn test_infer_command_default() {
        assert_eq!(infer_command("do something"), "pwd");
    }

    #[test]
    fn test_infer_agent_type_default() {
        assert_eq!(infer_agent_type("anything"), "general-purpose");
    }

    #[test]
    fn test_infer_description_first_line() {
        assert_eq!(
            infer_description("Read the config file and report back\nDo more stuff"),
            "Read the config file and report back"
        );
    }

    #[test]
    fn test_infer_description_short() {
        assert_eq!(infer_description("Hi"), "Execute task from user request");
    }

    #[test]
    fn test_infer_prompt_full_text() {
        assert_eq!(infer_prompt("Please read the file"), "Please read the file");
    }

    #[test]
    fn test_infer_prompt_empty() {
        assert_eq!(infer_prompt(""), "Complete the assigned task");
    }

    #[test]
    fn test_tool_schema_read_fallback() {
        let tools: Vec<OpenAITool> = vec![];
        let schema = tool_schema_for_call(&tools, "Read");
        assert_eq!(schema["required"][0], "file_path");
        assert!(schema["properties"]["file_path"].is_object());
    }

    #[test]
    fn test_tool_schema_bash_fallback() {
        let tools: Vec<OpenAITool> = vec![];
        let schema = tool_schema_for_call(&tools, "bash");
        assert_eq!(schema["required"][0], "command");
    }

    #[test]
    fn test_tool_schema_task_fallback() {
        let tools: Vec<OpenAITool> = vec![];
        let schema = tool_schema_for_call(&tools, "Task");
        let required: Vec<String> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(required.contains(&"description".to_string()));
        assert!(required.contains(&"prompt".to_string()));
        assert!(required.contains(&"subagent_type".to_string()));
    }

    #[test]
    fn test_tool_schema_unknown_fallback() {
        let tools: Vec<OpenAITool> = vec![];
        let schema = tool_schema_for_call(&tools, "unknown_tool");
        assert!(schema["properties"].as_object().unwrap().is_empty());
    }

    #[test]
    fn test_synthesize_read_input() {
        let func = OpenAIToolFunction {
            name: "Read".to_string(),
            description: None,
            parameters: None,
        };
        let input = synthesize_tool_input(&func, "read package.json");
        assert_eq!(input["file_path"], "package.json");
    }

    #[test]
    fn test_synthesize_bash_input() {
        let func = OpenAIToolFunction {
            name: "Bash".to_string(),
            description: None,
            parameters: None,
        };
        let input = synthesize_tool_input(&func, "run: ls -la");
        assert_eq!(input["command"], "ls -la");
    }

    #[test]
    fn test_synthesize_task_input() {
        let func = OpenAIToolFunction {
            name: "Task".to_string(),
            description: None,
            parameters: None,
        };
        let input = synthesize_tool_input(&func, "Read config and report");
        assert!(input["description"].is_string());
        assert!(input["prompt"].is_string());
        assert_eq!(input["subagent_type"], "general-purpose");
    }

    #[test]
    fn test_complete_tool_call_fills_missing() {
        let tools = vec![make_tool("Read")];
        let body = make_chat_request("read Cargo.toml", Some(tools));
        let partial = ToolCall {
            id: Some("call_test".to_string()),
            call_type: "function".to_string(),
            function: ToolFunction {
                name: "Read".to_string(),
                arguments: "{}".to_string(),
            },
            index: Some(0),
        };
        let completed = complete_tool_call(&partial, &body);
        let args: Value = serde_json::from_str(&completed.function.arguments).unwrap();
        assert_eq!(args["file_path"], "Cargo.toml");
    }

    #[test]
    fn test_complete_tool_call_keeps_existing() {
        let tools = vec![make_tool("Read")];
        let body = make_chat_request("read something", Some(tools));
        let partial = ToolCall {
            id: Some("call_test".to_string()),
            call_type: "function".to_string(),
            function: ToolFunction {
                name: "Read".to_string(),
                arguments: "{\"file_path\":\"existing.txt\"}".to_string(),
            },
            index: Some(0),
        };
        let completed = complete_tool_call(&partial, &body);
        let args: Value = serde_json::from_str(&completed.function.arguments).unwrap();
        assert_eq!(args["file_path"], "existing.txt");
    }

    #[test]
    fn test_synthesize_tool_call_no_tools() {
        let body = make_chat_request("read file", None);
        assert!(synthesize_tool_call(&body).is_none());
    }

    #[test]
    fn test_synthesize_tool_call_with_tools() {
        let tools = vec![make_tool("Read")];
        let body = make_chat_request("read package.json", Some(tools));
        let result = synthesize_tool_call(&body);
        assert!(result.is_some());
        let call = result.unwrap();
        assert_eq!(call.function.name, "Read");
        assert!(call.id.is_some());
        let args: Value = serde_json::from_str(&call.function.arguments).unwrap();
        assert_eq!(args["file_path"], "package.json");
    }
}
