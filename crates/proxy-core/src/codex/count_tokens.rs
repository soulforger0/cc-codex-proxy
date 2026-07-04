use serde_json::Value;

use crate::codex::translate::ResponsesRequest;

const REQUEST_OVERHEAD: u64 = 3;
const INPUT_ITEM_OVERHEAD: u64 = 4;
const CONTENT_PART_OVERHEAD: u64 = 2;
const TOOL_OVERHEAD: u64 = 4;
const STRUCTURED_OPTION_OVERHEAD: u64 = 2;
const IMAGE_TOKEN_ESTIMATE: u64 = 2_000;

/// Approximate token counter for Codex translated requests.
///
/// Claude Code uses `/v1/messages/count_tokens` for local compaction decisions.
/// This estimator is intentionally simple and monotonic, but counts the Codex
/// request shape that we actually send upstream instead of the raw Anthropic
/// compatibility wrapper.
pub fn count_translated_tokens(translated: &ResponsesRequest) -> u64 {
    let mut total = REQUEST_OVERHEAD + approx_token_count(&translated.model);

    if let Some(instructions) = &translated.instructions {
        total += approx_token_count(instructions);
    }

    for item in &translated.input {
        total += count_input_item_tokens(item);
    }

    if let Some(tools) = &translated.tools {
        total += count_tool_tokens(tools);
    }

    total += count_optional_value_tokens(translated.tool_choice.as_ref());
    total += count_optional_value_tokens(translated.reasoning.as_ref());
    total += translated
        .include
        .as_ref()
        .map(|include| {
            STRUCTURED_OPTION_OVERHEAD
                + include
                    .iter()
                    .map(|item| approx_token_count(item))
                    .sum::<u64>()
        })
        .unwrap_or(0);
    total += count_optional_value_tokens(translated.text.as_ref());

    total.max(1)
}

fn count_input_item_tokens(item: &Value) -> u64 {
    INPUT_ITEM_OVERHEAD
        + match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                count_field_tokens(item, "role")
                    + item
                        .get("content")
                        .and_then(Value::as_array)
                        .map(|parts| parts.iter().map(count_content_part_tokens).sum())
                        .unwrap_or_else(|| approx_token_count(&item.to_string()))
            }
            Some("function_call") => {
                count_field_tokens(item, "name") + count_field_tokens(item, "arguments")
            }
            Some("function_call_output") => count_field_tokens(item, "output"),
            _ => approx_token_count(&item.to_string()),
        }
}

fn count_content_part_tokens(part: &Value) -> u64 {
    CONTENT_PART_OVERHEAD
        + match part.get("type").and_then(Value::as_str) {
            Some("input_text") | Some("output_text") => count_field_tokens(part, "text"),
            Some("input_image") => IMAGE_TOKEN_ESTIMATE,
            _ => approx_token_count(&part.to_string()),
        }
}

fn count_tool_tokens(tools: &[Value]) -> u64 {
    tools
        .iter()
        .map(|tool| {
            TOOL_OVERHEAD
                + match tool.get("type").and_then(Value::as_str) {
                    Some("function") => {
                        count_field_tokens(tool, "name")
                            + count_field_tokens(tool, "description")
                            + count_value_tokens(tool.get("parameters"))
                            + count_value_tokens(tool.get("strict"))
                    }
                    Some("web_search") => {
                        count_value_tokens(tool.get("external_web_access"))
                            + count_value_tokens(tool.get("search_content_types"))
                            + count_value_tokens(tool.get("filters"))
                    }
                    _ => approx_token_count(&tool.to_string()),
                }
        })
        .sum()
}

fn count_optional_value_tokens(value: Option<&Value>) -> u64 {
    value
        .map(|value| STRUCTURED_OPTION_OVERHEAD + count_json_value_tokens(value))
        .unwrap_or(0)
}

fn count_value_tokens(value: Option<&Value>) -> u64 {
    value.map(count_json_value_tokens).unwrap_or(0)
}

fn count_json_value_tokens(value: &Value) -> u64 {
    match value {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Number(value) => approx_token_count(&value.to_string()),
        Value::String(value) => approx_token_count(value),
        Value::Array(items) => items.iter().map(count_json_value_tokens).sum(),
        Value::Object(object) => object
            .iter()
            .map(|(key, value)| approx_token_count(key) + count_json_value_tokens(value))
            .sum(),
    }
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
        codex::translate::{translate_request, ResponsesRequest},
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
    fn tool_choice_contributes_to_count() {
        let mut with_choice = request(json!("use the tool"));
        with_choice.tool_choice = Some(json!({"type": "tool", "name": "lookup_weather"}));

        let without_choice =
            translate_request(&request(json!("use the tool")), &resolved(), None).unwrap();
        let with_choice = translate_request(&with_choice, &resolved(), None).unwrap();

        assert!(count_translated_tokens(&with_choice) > count_translated_tokens(&without_choice));
    }

    #[test]
    fn reasoning_include_and_text_format_contribute_to_count() {
        let mut structured = request(json!("return structured output"));
        structured.output_config = Some(json!({
            "effort": "high",
            "format": {
                "type": "json_schema",
                "name": "weather_answer",
                "schema": {
                    "type": "object",
                    "properties": {
                        "summary": {"type": "string", "description": "Short summary"},
                        "temperature": {"type": "number"}
                    }
                }
            }
        }));

        let baseline = translate_request(
            &request(json!("return structured output")),
            &resolved(),
            None,
        )
        .unwrap();
        let structured = translate_request(&structured, &resolved(), None).unwrap();

        assert!(structured.reasoning.is_some());
        assert!(structured.include.is_some());
        assert!(structured.text.is_some());
        assert!(count_translated_tokens(&structured) > count_translated_tokens(&baseline));
    }

    #[test]
    fn web_search_filters_contribute_to_count() {
        let mut extra = serde_json::Map::new();
        extra.insert("type".into(), json!("web_search_20250305"));
        extra.insert("allowed_domains".into(), json!(["example.com"]));
        extra.insert("blocked_domains".into(), json!(["blocked.example"]));

        let mut with_web_search = request(json!("search"));
        with_web_search.tools = Some(vec![AnthropicTool {
            name: "web_search_20250305".into(),
            description: None,
            input_schema: None,
            extra,
        }]);

        let baseline = translate_request(&request(json!("search")), &resolved(), None).unwrap();
        let with_web_search = translate_request(&with_web_search, &resolved(), None).unwrap();

        assert!(count_translated_tokens(&with_web_search) > count_translated_tokens(&baseline));
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

        assert_eq!(
            count_input_item_tokens(&translated.input[0]),
            INPUT_ITEM_OVERHEAD + output_only
        );
        assert!(raw_wrapper > output_only);
    }

    #[test]
    fn image_count_uses_placeholder_not_base64_length() {
        let image_block = |data: &str| {
            json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": data
                }
            })
        };
        let short_image =
            translate_request(&request(json!([image_block("abc123")])), &resolved(), None).unwrap();
        let long_image = translate_request(
            &request(json!([image_block(&"a".repeat(10_000))])),
            &resolved(),
            None,
        )
        .unwrap();
        let two_images = translate_request(
            &request(json!([image_block("abc123"), image_block("def456")])),
            &resolved(),
            None,
        )
        .unwrap();

        assert_eq!(
            count_translated_tokens(&short_image),
            count_translated_tokens(&long_image)
        );
        assert!(count_translated_tokens(&two_images) > count_translated_tokens(&short_image));
    }

    #[test]
    fn unknown_translated_shapes_fall_back_to_serialized_count() {
        let baseline = ResponsesRequest {
            model: "gpt-5.5".into(),
            input: Vec::new(),
            store: false,
            instructions: None,
            tools: None,
            tool_choice: None,
            reasoning: None,
            include: None,
            text: None,
            stream: true,
        };
        let unknown = ResponsesRequest {
            input: vec![json!({
                "type": "custom_payload",
                "payload": "alpha beta gamma"
            })],
            ..baseline.clone()
        };

        assert!(count_translated_tokens(&unknown) > count_translated_tokens(&baseline));
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
