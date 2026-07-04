use serde_json::Value;

use crate::codex::translate::ResponsesRequest;

const IMAGE_TOKEN_ESTIMATE: u64 = 2_000;

/// Approximate token counter for Codex translated requests.
///
/// Claude Code uses `/v1/messages/count_tokens` for local compaction decisions.
/// This estimator is intentionally simple and monotonic, but counts the Codex
/// request shape that we actually send upstream instead of the raw Anthropic
/// compatibility wrapper.
pub fn count_translated_tokens(translated: &ResponsesRequest) -> u64 {
    let mut total = 0_u64;

    if let Some(instructions) = &translated.instructions {
        total += approx_token_count(instructions);
    }

    for item in &translated.input {
        total += count_input_item_tokens(item);
    }

    if let Some(tools) = &translated.tools {
        total += count_tool_tokens(tools);
    }

    total += translated.input.len() as u64 * 4;
    total += translated
        .tools
        .as_ref()
        .map_or(0, |tools| tools.len() as u64 * 4);
    total += approx_token_count(&translated.model);

    total.max(1)
}

fn count_input_item_tokens(item: &Value) -> u64 {
    match item.get("type").and_then(Value::as_str) {
        Some("message") => item
            .get("content")
            .and_then(Value::as_array)
            .map(|parts| parts.iter().map(count_content_part_tokens).sum())
            .unwrap_or_else(|| approx_token_count(&item.to_string())),
        Some("function_call") => {
            count_field_tokens(item, "name") + count_field_tokens(item, "arguments")
        }
        Some("function_call_output") => count_field_tokens(item, "output"),
        _ => approx_token_count(&item.to_string()),
    }
}

fn count_content_part_tokens(part: &Value) -> u64 {
    match part.get("type").and_then(Value::as_str) {
        Some("input_text") | Some("output_text") => count_field_tokens(part, "text"),
        Some("input_image") => IMAGE_TOKEN_ESTIMATE,
        _ => approx_token_count(&part.to_string()),
    }
}

fn count_tool_tokens(tools: &[Value]) -> u64 {
    tools
        .iter()
        .map(|tool| match tool.get("type").and_then(Value::as_str) {
            Some("function") => {
                count_field_tokens(tool, "name")
                    + count_field_tokens(tool, "description")
                    + tool
                        .get("parameters")
                        .map(|parameters| approx_token_count(&parameters.to_string()))
                        .unwrap_or(0)
            }
            Some("web_search") => 10,
            _ => approx_token_count(&tool.to_string()),
        })
        .sum()
}

fn count_field_tokens(value: &Value, field: &str) -> u64 {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(approx_token_count)
        .unwrap_or(0)
}

fn approx_token_count(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }

    let mut count = 0_u64;
    let mut in_word = false;
    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '-' || ch == '_' {
            if !in_word {
                count += 1;
                in_word = true;
            }
        } else {
            in_word = false;
            if !ch.is_whitespace() {
                count += 1;
            }
        }
    }

    count.max(1)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        anthropic::schema::{AnthropicMessage, AnthropicRequest, AnthropicTool},
        codex::translate::translate_request,
        config::Provider,
        model::ResolvedModel,
    };

    fn resolved() -> ResolvedModel {
        ResolvedModel {
            provider: Provider::Codex,
            requested: "gpt-5.5".into(),
            public_id: "gpt-5.5".into(),
            upstream_model: "gpt-5.5".into(),
            service_tier: None,
            context_window: 272_000,
        }
    }

    fn request(content: Value) -> AnthropicRequest {
        AnthropicRequest {
            model: "gpt-5.5".into(),
            max_tokens: Some(1),
            temperature: None,
            top_p: None,
            stream: Some(true),
            system: None,
            messages: vec![AnthropicMessage {
                role: "user".into(),
                content,
                extra: Default::default(),
            }],
            tools: None,
            tool_choice: None,
            metadata: None,
            output_config: None,
            thinking: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn simple_message_count_is_positive() {
        let translated = translate_request(&request(json!("hello")), &resolved(), None).unwrap();

        assert!(count_translated_tokens(&translated) > 0);
    }

    #[test]
    fn tool_schemas_contribute_to_count() {
        let mut with_tool = request(json!("use the tool"));
        with_tool.tools = Some(vec![AnthropicTool {
            name: "lookup_weather".into(),
            description: Some("Look up the weather for a city".into()),
            input_schema: Some(json!({
                "type": "object",
                "properties": {
                    "city": {"type": "string", "description": "City name"}
                },
                "required": ["city"]
            })),
            extra: Default::default(),
        }]);
        let without_tool =
            translate_request(&request(json!("use the tool")), &resolved(), None).unwrap();
        let with_tool = translate_request(&with_tool, &resolved(), None).unwrap();

        assert!(count_translated_tokens(&with_tool) > count_translated_tokens(&without_tool));
    }

    #[test]
    fn function_call_output_counts_output_text_not_wrapper_size() {
        let translated = translate_request(
            &request(json!([{
                "type": "tool_result",
                "tool_use_id": "call_1",
                "content": "alpha beta"
            }])),
            &resolved(),
            None,
        )
        .unwrap();

        let output_only = approx_token_count("alpha beta");
        let raw_wrapper = approx_token_count(&translated.input[0].to_string());

        assert_eq!(count_input_item_tokens(&translated.input[0]), output_only);
        assert!(raw_wrapper > output_only);
    }

    #[test]
    fn count_is_monotonic_for_longer_input() {
        let short = translate_request(&request(json!("hi")), &resolved(), None).unwrap();
        let long = translate_request(
            &request(json!("this is a much longer message with many words in it")),
            &resolved(),
            None,
        )
        .unwrap();

        assert!(count_translated_tokens(&long) >= count_translated_tokens(&short));
    }
}
