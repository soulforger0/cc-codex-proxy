use crate::{
    anthropic::schema::{AnthropicRequest, AnthropicTool},
    error::{ProxyError, Result},
    model::ResolvedModel,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesRequest {
    pub model: String,
    pub input: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    pub stream: bool,
}

pub fn translate_request(
    request: &AnthropicRequest,
    resolved: &ResolvedModel,
    session_id: Option<&str>,
) -> Result<ResponsesRequest> {
    let instructions = request.system.as_ref().map(system_to_instructions);
    let input = request
        .messages
        .iter()
        .map(|message| {
            Ok(json!({
                "type": "message",
                "role": map_role(&message.role)?,
                "content": content_to_codex_parts(&message.role, &message.content),
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    let tools = request
        .tools
        .as_ref()
        .map(|tools| translate_tools(tools))
        .transpose()?;
    let tool_choice = request.tool_choice.as_ref().and_then(translate_tool_choice);
    let reasoning = reasoning_from_request(request);
    let text = text_format_from_request(request);
    Ok(ResponsesRequest {
        model: resolved.upstream_model.clone(),
        input,
        instructions,
        tools,
        tool_choice,
        reasoning,
        text,
        max_output_tokens: request.max_tokens,
        temperature: request.temperature,
        top_p: request.top_p,
        metadata: request.metadata.clone(),
        service_tier: resolved.service_tier.clone(),
        prompt_cache_key: session_id.map(ToOwned::to_owned),
        stream: true,
    })
}

fn map_role(role: &str) -> Result<&'static str> {
    match role {
        "user" => Ok("user"),
        "assistant" => Ok("assistant"),
        "system" => Ok("system"),
        other => Err(ProxyError::InvalidRequest(format!(
            "unsupported message role \"{other}\""
        ))),
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

fn content_to_codex_parts(role: &str, content: &Value) -> Vec<Value> {
    match content {
        Value::String(text) => vec![text_part(role, text)],
        Value::Array(items) => items
            .iter()
            .flat_map(|item| block_to_codex_parts(role, item))
            .collect(),
        other => vec![text_part(role, &other.to_string())],
    }
}

fn block_to_codex_parts(role: &str, block: &Value) -> Vec<Value> {
    let kind = block.get("type").and_then(Value::as_str).unwrap_or("text");
    match kind {
        "text" => vec![text_part(
            role,
            block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )],
        "image" => block_to_image_part(block).into_iter().collect(),
        "tool_use" => vec![json!({
            "type": "function_call",
            "call_id": block.get("id").and_then(Value::as_str).unwrap_or("tool_call"),
            "name": block.get("name").and_then(Value::as_str).unwrap_or("tool"),
            "arguments": block.get("input").cloned().unwrap_or(Value::Null).to_string(),
        })],
        "tool_result" => vec![json!({
            "type": "function_call_output",
            "call_id": block.get("tool_use_id").and_then(Value::as_str).unwrap_or("tool_call"),
            "output": tool_result_output(block),
        })],
        _ => vec![text_part(role, &block.to_string())],
    }
}

fn text_part(role: &str, text: &str) -> Value {
    let part_type = if role == "assistant" {
        "output_text"
    } else {
        "input_text"
    };
    json!({ "type": part_type, "text": text })
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

    if let Some(context_size) = tool
        .extra
        .get("search_context_size")
        .and_then(Value::as_str)
        .filter(|value| matches!(*value, "low" | "medium" | "high"))
    {
        out.insert("search_context_size".into(), json!(context_size));
    }
    if let Some(location) = tool.extra.get("user_location") {
        out.insert("user_location".into(), location.clone());
    }

    let mut filters = serde_json::Map::new();
    if let Some(allowed) = tool.extra.get("allowed_domains") {
        filters.insert("allowed_domains".into(), allowed.clone());
    }
    if let Some(blocked) = tool.extra.get("blocked_domains") {
        filters.insert("blocked_domains".into(), blocked.clone());
    }
    if !filters.is_empty() {
        out.insert("filters".into(), Value::Object(filters));
    }

    Value::Object(out)
}

fn translate_tool_choice(choice: &Value) -> Option<Value> {
    let choice_type = choice.get("type").and_then(Value::as_str)?;
    match choice_type {
        "auto" => Some(json!("auto")),
        "none" => Some(json!("none")),
        "any" => Some(json!("required")),
        "tool" => choice.get("name").and_then(Value::as_str).map(|name| {
            if name == "web_search" {
                json!({ "type": "web_search" })
            } else {
                json!({ "type": "function", "name": name })
            }
        }),
        "web_search" => Some(json!({ "type": "web_search" })),
        kind if kind.starts_with("web_search_") => Some(json!({ "type": "web_search" })),
        _ => None,
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
    let format = request.output_config.as_ref()?.get("format")?;
    if format.get("type").and_then(Value::as_str) == Some("json_schema") {
        Some(json!({ "format": format }))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ResolvedModel;

    fn resolved() -> ResolvedModel {
        ResolvedModel {
            requested: "gpt-5.4".into(),
            public_id: "gpt-5.4".into(),
            upstream_model: "gpt-5.4".into(),
            service_tier: None,
            context_window: 272_000,
        }
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
        let output = translated.input[0]["content"][0]["output"]
            .as_str()
            .unwrap();
        assert!(output.contains("image omitted"));
    }

    #[test]
    fn translates_tool_choice_sampling_and_metadata() {
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

        assert_eq!(
            translated.tool_choice.as_ref().unwrap(),
            &json!({ "type": "function", "name": "Read" })
        );
        assert_eq!(translated.temperature, Some(0.2));
        assert_eq!(translated.top_p, Some(0.9));
        assert_eq!(translated.metadata.as_ref().unwrap()["session"], "s1");
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

        req.output_config = Some(json!({ "effort": "auto" }));
        let translated = translate_request(&req, &resolved(), None).unwrap();
        assert!(translated.reasoning.is_none());

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
        assert_eq!(tool["filters"]["allowed_domains"][0], "example.com");
        assert_eq!(tool["filters"]["blocked_domains"][0], "blocked.example");
        assert_eq!(tool["user_location"]["city"], "Melbourne");
        assert_eq!(
            translated.tool_choice.as_ref().unwrap(),
            &json!({ "type": "web_search" })
        );
        assert!(tool.get("max_uses").is_none());
    }
}
