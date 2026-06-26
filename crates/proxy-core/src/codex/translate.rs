use crate::{
    anthropic::schema::{AnthropicRequest, AnthropicTool},
    error::{ProxyError, Result},
    model::ResolvedModel,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const TOOL_ROUTING_INSTRUCTION: &str = "When calling tools, only call functions explicitly listed in this request's tools array. Match each tool's input schema exactly, and omit optional fields when their value would be an empty string.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesRequest {
    pub model: String,
    pub input: Vec<Value>,
    pub store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<Value>,
    pub stream: bool,
}

pub fn translate_request(
    request: &AnthropicRequest,
    resolved: &ResolvedModel,
    _session_id: Option<&str>,
) -> Result<ResponsesRequest> {
    let mut instruction_parts = Vec::new();
    if let Some(system) = &request.system {
        push_instruction_part(&mut instruction_parts, system);
    }
    instruction_parts.push(TOOL_ROUTING_INSTRUCTION.to_string());

    let input = build_input(&request.messages)?;
    let instructions = (!instruction_parts.is_empty()).then(|| instruction_parts.join("\n\n"));
    let tools = request
        .tools
        .as_ref()
        .map(|tools| translate_tools(tools))
        .transpose()?;
    let tool_choice = Some(translate_tool_choice(request.tool_choice.as_ref()));
    let reasoning = reasoning_from_request(request);
    let include = reasoning
        .as_ref()
        .map(|_| vec!["reasoning.encrypted_content".to_string()]);
    let text = text_format_from_request(request);
    Ok(ResponsesRequest {
        model: resolved.upstream_model.clone(),
        input,
        store: false,
        instructions,
        tools,
        tool_choice,
        reasoning,
        include,
        text,
        stream: true,
    })
}

fn push_instruction_part(parts: &mut Vec<String>, system: &Value) {
    let text = system_to_instructions(system);
    if !text.is_empty() {
        parts.push(text);
    }
}

fn system_to_instructions(system: &Value) -> String {
    match system {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                item.as_str().map(ToOwned::to_owned).or_else(|| {
                    item.get("text")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        other => other.to_string(),
    }
}

fn build_input(messages: &[crate::anthropic::schema::AnthropicMessage]) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for message in messages {
        match message.role.as_str() {
            "user" => push_user_input_items(&mut out, &message.content),
            "assistant" => push_assistant_input_items(&mut out, &message.content),
            "system" => push_developer_input_items(&mut out, &message.content),
            other => {
                return Err(ProxyError::InvalidRequest(format!(
                    "unsupported message role \"{other}\""
                )));
            }
        }
    }
    Ok(out)
}

fn push_user_input_items(out: &mut Vec<Value>, content: &Value) {
    let mut parts = Vec::new();
    for block in normalized_blocks(content) {
        let kind = block.get("type").and_then(Value::as_str).unwrap_or("text");
        match kind {
            "text" => parts.push(json!({
                "type": "input_text",
                "text": block.get("text").and_then(Value::as_str).unwrap_or_default()
            })),
            "image" => {
                if let Some(image) = block_to_image_part(&block) {
                    parts.push(image);
                }
            }
            "tool_result" => {
                flush_message(out, "user", &mut parts);
                out.push(json!({
                    "type": "function_call_output",
                    "call_id": block.get("tool_use_id").and_then(Value::as_str).unwrap_or("tool_call"),
                    "output": tool_result_output(&block),
                }));
            }
            _ => parts.push(json!({ "type": "input_text", "text": block.to_string() })),
        }
    }
    flush_message(out, "user", &mut parts);
}

fn push_assistant_input_items(out: &mut Vec<Value>, content: &Value) {
    let mut parts = Vec::new();
    for block in normalized_blocks(content) {
        let kind = block.get("type").and_then(Value::as_str).unwrap_or("text");
        match kind {
            "text" => parts.push(json!({
                "type": "output_text",
                "text": block.get("text").and_then(Value::as_str).unwrap_or_default()
            })),
            "tool_use" => {
                flush_message(out, "assistant", &mut parts);
                out.push(json!({
                    "type": "function_call",
                    "call_id": block.get("id").and_then(Value::as_str).unwrap_or("tool_call"),
                    "name": block.get("name").and_then(Value::as_str).unwrap_or("tool"),
                    "arguments": assistant_tool_arguments(&block),
                }));
            }
            _ => parts.push(json!({ "type": "output_text", "text": block.to_string() })),
        }
    }
    flush_message(out, "assistant", &mut parts);
}

fn push_developer_input_items(out: &mut Vec<Value>, content: &Value) {
    let parts = normalized_blocks(content)
        .into_iter()
        .filter_map(|block| {
            (block.get("type").and_then(Value::as_str).unwrap_or("text") == "text").then(|| {
                json!({
                    "type": "input_text",
                    "text": block.get("text").and_then(Value::as_str).unwrap_or_default()
                })
            })
        })
        .collect::<Vec<_>>();
    if !parts.is_empty() {
        out.push(json!({ "type": "message", "role": "developer", "content": parts }));
    }
}

fn normalized_blocks(content: &Value) -> Vec<Value> {
    match content {
        Value::String(text) => vec![json!({ "type": "text", "text": text })],
        Value::Array(items) => items.clone(),
        other => vec![json!({ "type": "text", "text": other.to_string() })],
    }
}

fn assistant_tool_arguments(block: &Value) -> String {
    block
        .get("input")
        .filter(|input| input.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}))
        .to_string()
}

fn flush_message(out: &mut Vec<Value>, role: &str, parts: &mut Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    out.push(json!({
        "type": "message",
        "role": role,
        "content": std::mem::take(parts),
    }));
}

fn block_to_image_part(block: &Value) -> Option<Value> {
    let source = block.get("source")?;
    match source.get("type").and_then(Value::as_str) {
        Some("base64") => {
            let media = source
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("image/png");
            let data = source.get("data").and_then(Value::as_str)?;
            Some(json!({
                "type": "input_image",
                "image_url": format!("data:{media};base64,{data}")
            }))
        }
        Some("url") => source.get("url").and_then(Value::as_str).map(|url| {
            json!({
                "type": "input_image",
                "image_url": url
            })
        }),
        _ => None,
    }
}

fn tool_result_output(block: &Value) -> String {
    match block.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item.get("type").and_then(Value::as_str) {
                Some("text") => item
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                Some("image") => {
                    "[image omitted: Codex function_call_output accepts text only]".to_string()
                }
                _ => item.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn translate_tools(tools: &[AnthropicTool]) -> Result<Vec<Value>> {
    let mut out = Vec::with_capacity(tools.len());
    for tool in tools {
        if is_anthropic_web_search_tool(tool) {
            out.push(translate_web_search_tool(tool));
            continue;
        }
        out.push(json!({
            "type": "function",
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.input_schema.clone().unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            "strict": false
        }));
    }
    Ok(out)
}

fn is_anthropic_web_search_tool(tool: &AnthropicTool) -> bool {
    tool.extra
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.starts_with("web_search_"))
        || tool.name.starts_with("web_search_")
}

fn translate_web_search_tool(tool: &AnthropicTool) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("type".into(), json!("web_search"));
    out.insert("external_web_access".into(), json!(false));
    out.insert("search_content_types".into(), json!(["text", "image"]));

    let mut filters = serde_json::Map::new();
    if let Some(allowed) = tool
        .extra
        .get("allowed_domains")
        .filter(|value| value.as_array().is_some_and(|items| !items.is_empty()))
    {
        filters.insert("allowed_domains".into(), allowed.clone());
    }
    if let Some(blocked) = tool
        .extra
        .get("blocked_domains")
        .filter(|value| value.as_array().is_some_and(|items| !items.is_empty()))
    {
        filters.insert("blocked_domains".into(), blocked.clone());
    }
    if !filters.is_empty() {
        out.insert("filters".into(), Value::Object(filters));
    }

    Value::Object(out)
}

fn translate_tool_choice(choice: Option<&Value>) -> Value {
    let Some(choice) = choice else {
        return json!("auto");
    };
    let choice_type = choice.get("type").and_then(Value::as_str);
    match choice_type {
        Some("auto") => json!("auto"),
        Some("none") => json!("none"),
        Some("any") => json!("required"),
        Some("tool") => choice
            .get("name")
            .and_then(Value::as_str)
            .map(|name| {
                if name == "web_search" || name.starts_with("web_search_") {
                    json!({ "type": "web_search" })
                } else {
                    json!({ "type": "function", "name": name })
                }
            })
            .unwrap_or_else(|| json!("required")),
        Some("web_search") => json!({ "type": "web_search" }),
        Some(kind) if kind.starts_with("web_search_") => json!({ "type": "web_search" }),
        _ => json!("auto"),
    }
}

fn reasoning_from_request(request: &AnthropicRequest) -> Option<Value> {
    let effort = request
        .output_config
        .as_ref()
        .and_then(|value| value.get("effort"))
        .and_then(Value::as_str)
        .and_then(map_effort)
        .or_else(|| {
            request
                .thinking
                .as_ref()
                .and_then(|value| value.get("budget_tokens"))
                .and_then(Value::as_u64)
                .and_then(map_thinking_budget)
        });
    effort.map(|effort| json!({ "effort": effort }))
}

fn map_effort(effort: &str) -> Option<&'static str> {
    match effort {
        "auto" => None,
        "max" | "ultracode" => Some("xhigh"),
        "none" => Some("none"),
        "minimal" => Some("minimal"),
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" => Some("xhigh"),
        _ => None,
    }
}

fn map_thinking_budget(budget: u64) -> Option<&'static str> {
    match budget {
        0 => Some("none"),
        1..=4_096 => Some("low"),
        4_097..=32_768 => Some("medium"),
        _ => Some("high"),
    }
}

fn text_format_from_request(request: &AnthropicRequest) -> Option<Value> {
    let format = request
        .output_config
        .as_ref()
        .and_then(|value| value.get("format"))
        .filter(|format| format.get("type").and_then(Value::as_str) == Some("json_schema"))?;

    Some(json!({
        "format": {
            "type": "json_schema",
            "name": format.get("name").and_then(Value::as_str).unwrap_or("response"),
            "schema": normalize_strict_json_schema(format.get("schema").cloned().unwrap_or_else(|| json!({}))),
            "strict": true
        }
    }))
}

fn normalize_strict_json_schema(schema: Value) -> Value {
    match schema {
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(normalize_strict_json_schema)
                .collect(),
        ),
        Value::Object(mut object) => {
            for value in object.values_mut() {
                *value = normalize_strict_json_schema(std::mem::take(value));
            }
            if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                object.insert(
                    "required".into(),
                    Value::Array(properties.keys().cloned().map(Value::String).collect()),
                );
            }
            Value::Object(object)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Provider, model::ResolvedModel};

    fn resolved() -> ResolvedModel {
        ResolvedModel {
            provider: Provider::Codex,
            requested: "gpt-5.4".into(),
            public_id: "gpt-5.4".into(),
            upstream_model: "gpt-5.4".into(),
            service_tier: None,
            context_window: 272_000,
        }
    }

    #[test]
    fn hoists_system_messages_to_instructions() {
        let req = AnthropicRequest {
            model: "gpt-5.4".into(),
            max_tokens: Some(100),
            temperature: None,
            top_p: None,
            stream: Some(true),
            system: Some(json!("top-level instructions")),
            messages: vec![
                crate::anthropic::schema::AnthropicMessage {
                    role: "system".into(),
                    content: json!([
                        {"type": "text", "text": "message instructions"},
                        {"type": "text", "text": "more instructions"}
                    ]),
                    extra: Default::default(),
                },
                crate::anthropic::schema::AnthropicMessage {
                    role: "user".into(),
                    content: json!("hello"),
                    extra: Default::default(),
                },
            ],
            tools: None,
            tool_choice: None,
            metadata: None,
            output_config: None,
            thinking: None,
            extra: Default::default(),
        };

        let translated = translate_request(&req, &resolved(), None).unwrap();

        let instructions = translated.instructions.as_deref().unwrap();
        assert!(instructions.starts_with("top-level instructions"));
        assert!(instructions.contains(TOOL_ROUTING_INSTRUCTION));
        assert_eq!(translated.input.len(), 2);
        assert_eq!(
            translated.input[0],
            json!({
                "type": "message",
                "role": "developer",
                "content": [
                    {"type": "input_text", "text": "message instructions"},
                    {"type": "input_text", "text": "more instructions"}
                ]
            })
        );
        assert_eq!(translated.input[1]["role"], "user");
    }

    #[test]
    fn serializes_store_false_for_codex() {
        let req = AnthropicRequest {
            model: "gpt-5.4".into(),
            max_tokens: Some(100),
            temperature: None,
            top_p: None,
            stream: Some(true),
            system: None,
            messages: vec![crate::anthropic::schema::AnthropicMessage {
                role: "user".into(),
                content: json!("hi"),
                extra: Default::default(),
            }],
            tools: None,
            tool_choice: None,
            metadata: None,
            output_config: None,
            thinking: None,
            extra: Default::default(),
        };

        let translated = translate_request(&req, &resolved(), None).unwrap();
        let serialized = serde_json::to_value(translated).unwrap();

        assert_eq!(serialized["store"], false);
    }

    #[test]
    fn omits_unsupported_codex_request_fields() {
        let req = AnthropicRequest {
            model: "gpt-5.4".into(),
            max_tokens: Some(123),
            temperature: Some(0.2),
            top_p: Some(0.9),
            stream: Some(true),
            system: None,
            messages: vec![crate::anthropic::schema::AnthropicMessage {
                role: "user".into(),
                content: json!("hi"),
                extra: Default::default(),
            }],
            tools: None,
            tool_choice: None,
            metadata: Some(json!({ "session": "s1" })),
            output_config: None,
            thinking: None,
            extra: Default::default(),
        };

        let translated = translate_request(&req, &resolved(), Some("session-id")).unwrap();
        let serialized = serde_json::to_value(translated).unwrap();

        assert!(serialized.get("max_tokens").is_none());
        assert!(serialized.get("max_output_tokens").is_none());
        assert!(serialized.get("temperature").is_none());
        assert!(serialized.get("top_p").is_none());
        assert!(serialized.get("metadata").is_none());
        assert!(serialized.get("prompt_cache_key").is_none());
        assert!(serialized.get("service_tier").is_none());
        assert!(serialized.get("parallel_tool_calls").is_none());
        assert!(serialized.get("text").is_none());
    }

    #[test]
    fn translates_tool_result_images_to_placeholder() {
        let req = AnthropicRequest {
            model: "gpt-5.4".into(),
            max_tokens: Some(100),
            temperature: None,
            top_p: None,
            stream: Some(true),
            system: None,
            messages: vec![crate::anthropic::schema::AnthropicMessage {
                role: "user".into(),
                content: json!([{
                    "type": "tool_result",
                    "tool_use_id": "toolu_1",
                    "content": [{"type": "image", "source": {"type": "base64", "data": "abc"}}]
                }]),
                extra: Default::default(),
            }],
            tools: None,
            tool_choice: None,
            metadata: None,
            output_config: None,
            thinking: None,
            extra: Default::default(),
        };
        let translated = translate_request(&req, &resolved(), Some("s")).unwrap();
        assert_eq!(translated.input[0]["type"], "function_call_output");
        assert_eq!(translated.input[0]["call_id"], "toolu_1");
        let output = translated.input[0]["output"].as_str().unwrap();
        assert!(output.contains("image omitted"));
    }

    #[test]
    fn assistant_tool_use_history_uses_object_arguments() {
        let req = AnthropicRequest {
            model: "gpt-5.4".into(),
            max_tokens: Some(100),
            temperature: None,
            top_p: None,
            stream: Some(true),
            system: None,
            messages: vec![crate::anthropic::schema::AnthropicMessage {
                role: "assistant".into(),
                content: json!([
                    {
                        "type": "tool_use",
                        "id": "toolu_1",
                        "name": "Read",
                        "input": {"path": "Cargo.toml"}
                    },
                    {
                        "type": "tool_use",
                        "id": "toolu_2",
                        "name": "NoArgs",
                        "input": null
                    }
                ]),
                extra: Default::default(),
            }],
            tools: None,
            tool_choice: None,
            metadata: None,
            output_config: None,
            thinking: None,
            extra: Default::default(),
        };
        let translated = translate_request(&req, &resolved(), Some("s")).unwrap();

        assert_eq!(translated.input[0]["type"], "function_call");
        assert_eq!(
            translated.input[0]["arguments"],
            json!("{\"path\":\"Cargo.toml\"}")
        );
        assert_eq!(translated.input[1]["type"], "function_call");
        assert_eq!(translated.input[1]["arguments"], json!("{}"));
    }

    #[test]
    fn translates_tool_choice_and_codex_defaults() {
        let req = AnthropicRequest {
            model: "gpt-5.4".into(),
            max_tokens: Some(100),
            temperature: Some(0.2),
            top_p: Some(0.9),
            stream: Some(true),
            system: None,
            messages: vec![crate::anthropic::schema::AnthropicMessage {
                role: "user".into(),
                content: json!("hello"),
                extra: Default::default(),
            }],
            tools: Some(vec![AnthropicTool {
                name: "Read".into(),
                description: Some("Read a file".into()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    },
                    "required": ["path"]
                })),
                extra: Default::default(),
            }]),
            tool_choice: Some(json!({ "type": "tool", "name": "Read" })),
            metadata: Some(json!({ "session": "s1" })),
            output_config: None,
            thinking: None,
            extra: Default::default(),
        };

        let translated = translate_request(&req, &resolved(), Some("s")).unwrap();
        let serialized = serde_json::to_value(&translated).unwrap();

        assert_eq!(
            translated.tool_choice.as_ref().unwrap(),
            &json!({ "type": "function", "name": "Read" })
        );
        assert!(serialized.get("parallel_tool_calls").is_none());
        assert!(serialized.get("prompt_cache_key").is_none());
        assert!(serialized.get("text").is_none());
        assert!(serialized.get("temperature").is_none());
        assert!(serialized.get("top_p").is_none());
        assert!(serialized.get("metadata").is_none());
    }

    #[test]
    fn translates_reasoning_effort_values() {
        let mut req = AnthropicRequest {
            model: "gpt-5.4".into(),
            max_tokens: Some(100),
            temperature: None,
            top_p: None,
            stream: Some(true),
            system: None,
            messages: vec![crate::anthropic::schema::AnthropicMessage {
                role: "user".into(),
                content: json!("hello"),
                extra: Default::default(),
            }],
            tools: None,
            tool_choice: None,
            metadata: None,
            output_config: Some(json!({ "effort": "max" })),
            thinking: None,
            extra: Default::default(),
        };

        let translated = translate_request(&req, &resolved(), None).unwrap();
        assert_eq!(translated.reasoning.as_ref().unwrap()["effort"], "xhigh");
        assert_eq!(
            translated.include.as_ref().unwrap(),
            &vec!["reasoning.encrypted_content".to_string()]
        );

        req.output_config = Some(json!({ "effort": "auto" }));
        let translated = translate_request(&req, &resolved(), None).unwrap();
        assert!(translated.reasoning.is_none());
        assert!(translated.include.is_none());

        req.output_config = None;
        req.thinking = Some(json!({ "type": "enabled", "budget_tokens": 4096 }));
        let translated = translate_request(&req, &resolved(), None).unwrap();
        assert_eq!(translated.reasoning.as_ref().unwrap()["effort"], "low");
    }

    #[test]
    fn translates_anthropic_web_search_tool() {
        let web_search_extra = json!({
            "type": "web_search_20250305",
            "max_uses": 5,
            "allowed_domains": ["example.com"],
            "blocked_domains": ["blocked.example"],
            "user_location": {
                "type": "approximate",
                "city": "Melbourne",
                "region": "Victoria",
                "country": "AU",
                "timezone": "Australia/Melbourne"
            }
        })
        .as_object()
        .unwrap()
        .clone();
        let req = AnthropicRequest {
            model: "gpt-5.4".into(),
            max_tokens: Some(100),
            temperature: None,
            top_p: None,
            stream: Some(true),
            system: None,
            messages: vec![crate::anthropic::schema::AnthropicMessage {
                role: "user".into(),
                content: json!("latest news"),
                extra: Default::default(),
            }],
            tools: Some(vec![AnthropicTool {
                name: "web_search".into(),
                description: None,
                input_schema: None,
                extra: web_search_extra,
            }]),
            tool_choice: Some(json!({ "type": "tool", "name": "web_search" })),
            metadata: None,
            output_config: None,
            thinking: None,
            extra: Default::default(),
        };

        let translated = translate_request(&req, &resolved(), None).unwrap();
        let tool = &translated.tools.as_ref().unwrap()[0];
        assert_eq!(tool["type"], "web_search");
        assert_eq!(tool["external_web_access"], false);
        assert_eq!(tool["search_content_types"], json!(["text", "image"]));
        assert_eq!(tool["filters"]["allowed_domains"][0], "example.com");
        assert_eq!(tool["filters"]["blocked_domains"][0], "blocked.example");
        assert!(tool.get("user_location").is_none());
        assert!(tool.get("search_context_size").is_none());
        assert_eq!(
            translated.tool_choice.as_ref().unwrap(),
            &json!({ "type": "web_search" })
        );
        assert!(tool.get("max_uses").is_none());
    }

    #[test]
    fn serializes_reference_codex_field_set() {
        let req = AnthropicRequest {
            model: "gpt-5.4".into(),
            max_tokens: Some(100),
            temperature: Some(0.2),
            top_p: Some(0.9),
            stream: Some(true),
            system: Some(json!("be concise")),
            messages: vec![crate::anthropic::schema::AnthropicMessage {
                role: "user".into(),
                content: json!("hello"),
                extra: Default::default(),
            }],
            tools: Some(vec![AnthropicTool {
                name: "lookup_weather".into(),
                description: Some("Look up the weather".into()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "city": { "type": "string" }
                    },
                    "required": ["city"]
                })),
                extra: Default::default(),
            }]),
            tool_choice: Some(json!({ "type": "tool", "name": "lookup_weather" })),
            metadata: Some(json!({ "session": "s1" })),
            output_config: Some(json!({
                "effort": "high",
                "format": {
                    "type": "json_schema",
                    "name": "weather_response",
                    "schema": {
                        "type": "object",
                        "properties": {
                            "forecast": { "type": "string" }
                        },
                        "required": ["forecast"]
                    }
                }
            })),
            thinking: None,
            extra: Default::default(),
        };

        let translated = translate_request(&req, &resolved(), None).unwrap();
        let serialized = serde_json::to_value(translated).unwrap();
        let mut keys = serialized
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();

        assert_eq!(
            keys,
            vec![
                "include",
                "input",
                "instructions",
                "model",
                "reasoning",
                "store",
                "stream",
                "text",
                "tool_choice",
                "tools",
            ]
        );
    }

    #[test]
    fn normalizes_json_schema_format_for_strict_codex_output() {
        let req = AnthropicRequest {
            model: "gpt-5.4".into(),
            max_tokens: Some(100),
            temperature: None,
            top_p: None,
            stream: Some(true),
            system: None,
            messages: vec![crate::anthropic::schema::AnthropicMessage {
                role: "user".into(),
                content: json!("hello"),
                extra: Default::default(),
            }],
            tools: None,
            tool_choice: None,
            metadata: None,
            output_config: Some(json!({
                "format": {
                    "type": "json_schema",
                    "schema": {
                        "type": "object",
                        "properties": {
                            "answer": { "type": "string" },
                            "confidence": { "type": "number" }
                        },
                        "required": ["answer"]
                    }
                }
            })),
            thinking: None,
            extra: Default::default(),
        };

        let translated = translate_request(&req, &resolved(), None).unwrap();
        let text = translated.text.as_ref().unwrap();
        assert_eq!(text["format"]["type"], "json_schema");
        assert_eq!(text["format"]["name"], "response");
        assert_eq!(text["format"]["strict"], true);
        assert_eq!(
            text["format"]["schema"]["required"],
            json!(["answer", "confidence"])
        );
    }
}
