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
    pub reasoning: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
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
    let tools = request.tools.as_ref().map(|tools| translate_tools(tools)).transpose()?;
    let reasoning = reasoning_from_request(request);
    let text = text_format_from_request(request);
    Ok(ResponsesRequest {
        model: resolved.upstream_model.clone(),
        input,
        instructions,
        tools,
        reasoning,
        text,
        max_output_tokens: request.max_tokens,
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
        other => Err(ProxyError::InvalidRequest(format!("unsupported message role \"{other}\""))),
    }
}

fn system_to_instructions(system: &Value) -> String {
    match system {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                item.as_str()
                    .map(ToOwned::to_owned)
                    .or_else(|| item.get("text").and_then(Value::as_str).map(ToOwned::to_owned))
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        other => other.to_string(),
    }
}

fn content_to_codex_parts(role: &str, content: &Value) -> Vec<Value> {
    match content {
        Value::String(text) => vec![text_part(role, text)],
        Value::Array(items) => items.iter().flat_map(|item| block_to_codex_parts(role, item)).collect(),
        other => vec![text_part(role, &other.to_string())],
    }
}

fn block_to_codex_parts(role: &str, block: &Value) -> Vec<Value> {
    let kind = block.get("type").and_then(Value::as_str).unwrap_or("text");
    match kind {
        "text" => vec![text_part(
            role,
            block.get("text").and_then(Value::as_str).unwrap_or_default(),
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
    let part_type = if role == "assistant" { "output_text" } else { "input_text" };
    json!({ "type": part_type, "text": text })
}

fn block_to_image_part(block: &Value) -> Option<Value> {
    let source = block.get("source")?;
    match source.get("type").and_then(Value::as_str) {
        Some("base64") => {
            let media = source.get("media_type").and_then(Value::as_str).unwrap_or("image/png");
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
                Some("text") => item.get("text").and_then(Value::as_str).unwrap_or_default().to_string(),
                Some("image") => "[image omitted: Codex function_call_output accepts text only]".to_string(),
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
        if tool.name == "web_search_20250305" {
            out.push(json!({ "type": "web_search" }));
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

fn reasoning_from_request(request: &AnthropicRequest) -> Option<Value> {
    let effort = request
        .output_config
        .as_ref()
        .and_then(|value| value.get("effort"))
        .and_then(Value::as_str)
        .or_else(|| {
            request
                .thinking
                .as_ref()
                .and_then(|value| value.get("budget_tokens"))
                .and_then(Value::as_u64)
                .map(|budget| if budget > 40_000 { "high" } else { "medium" })
        });
    effort.map(|effort| {
        let mapped = match effort {
            "max" => "xhigh",
            "none" | "low" | "medium" | "high" | "xhigh" => effort,
            _ => "medium",
        };
        json!({ "effort": mapped })
    })
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
            metadata: None,
            output_config: None,
            thinking: None,
            extra: Default::default(),
        };
        let translated = translate_request(&req, &resolved(), Some("s")).unwrap();
        let output = translated.input[0]["content"][0]["output"].as_str().unwrap();
        assert!(output.contains("image omitted"));
    }
}

