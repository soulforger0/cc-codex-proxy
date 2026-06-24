use crate::{
    anthropic::{
        response,
        schema::{AnthropicContentBlock, AnthropicResponse, AnthropicUsage},
    },
    codex::client::ByteStream,
    error::Result,
};
use async_stream::try_stream;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde_json::{json, Value};
use std::pin::Pin;
use uuid::Uuid;

pub fn translate_stream(
    upstream: ByteStream,
    model: String,
) -> Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>> {
    Box::pin(try_stream! {
        let message_id = format!("msg_{}", Uuid::new_v4().simple());
        let mut reducer = CodexReducer::new(message_id.clone(), model.clone());
        yield response::message_start(&message_id, &model);
        let mut parser = SseParser::default();
        futures_util::pin_mut!(upstream);
        while let Some(chunk) = upstream.next().await {
            for event in parser.push(&chunk?) {
                for bytes in reducer.process_event(&event) {
                    yield bytes;
                }
            }
        }
        for event in parser.finish() {
            for bytes in reducer.process_event(&event) {
                yield bytes;
            }
        }
        for bytes in reducer.finish_events() {
            yield bytes;
        }
    })
}

pub async fn accumulate_response(upstream: ByteStream, model: String) -> Result<AnthropicResponse> {
    let message_id = format!("msg_{}", Uuid::new_v4().simple());
    let mut reducer = CodexReducer::new(message_id.clone(), model.clone());
    let mut parser = SseParser::default();
    futures_util::pin_mut!(upstream);
    while let Some(chunk) = upstream.next().await {
        for event in parser.push(&chunk?) {
            let _ = reducer.process_event(&event);
        }
    }
    for event in parser.finish() {
        let _ = reducer.process_event(&event);
    }
    Ok(reducer.finish_response())
}

#[derive(Debug, Default)]
struct SseParser {
    buffer: String,
}

impl SseParser {
    fn push(&mut self, chunk: &[u8]) -> Vec<Value> {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
        let mut events = Vec::new();
        while let Some(idx) = self.buffer.find("\n\n") {
            let raw = self.buffer[..idx].to_string();
            self.buffer.drain(..idx + 2);
            if let Some(value) = parse_sse_event(&raw) {
                events.push(value);
            }
        }
        events
    }

    fn finish(&mut self) -> Vec<Value> {
        if self.buffer.trim().is_empty() {
            return Vec::new();
        }
        let raw = std::mem::take(&mut self.buffer);
        parse_sse_event(&raw).into_iter().collect()
    }
}

fn parse_sse_event(raw: &str) -> Option<Value> {
    let data = raw
        .lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    serde_json::from_str(&data).ok()
}

#[derive(Debug)]
struct CodexReducer {
    message_id: String,
    model: String,
    text_open: bool,
    text_index: usize,
    next_index: usize,
    text: String,
    tool_blocks: Vec<AnthropicContentBlock>,
    usage: AnthropicUsage,
    stop_reason: Option<String>,
    stopped: bool,
}

impl CodexReducer {
    fn new(message_id: String, model: String) -> Self {
        Self {
            message_id,
            model,
            text_open: false,
            text_index: 0,
            next_index: 0,
            text: String::new(),
            tool_blocks: Vec::new(),
            usage: AnthropicUsage {
                input_tokens: 0,
                output_tokens: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            stop_reason: None,
            stopped: false,
        }
    }

    fn process_event(&mut self, event: &Value) -> Vec<Bytes> {
        let mut out = Vec::new();
        if let Some(delta) = extract_text_delta(event) {
            if !self.text_open {
                self.text_open = true;
                self.text_index = self.next_index;
                self.next_index += 1;
                out.push(response::content_block_start(
                    self.text_index,
                    json!({ "type": "text", "text": "" }),
                ));
            }
            self.text.push_str(&delta);
            out.push(response::content_block_delta(
                self.text_index,
                json!({ "type": "text_delta", "text": delta }),
            ));
        }
        if let Some(tool) = extract_function_call(event) {
            let index = self.next_index;
            self.next_index += 1;
            let block = AnthropicContentBlock {
                kind: "tool_use".into(),
                text: None,
                id: Some(tool.call_id.clone()),
                name: Some(tool.name.clone()),
                input: Some(tool.arguments.clone()),
            };
            out.push(response::content_block_start(
                index,
                json!({
                    "type": "tool_use",
                    "id": tool.call_id,
                    "name": tool.name,
                    "input": tool.arguments
                }),
            ));
            out.push(response::content_block_stop(index));
            self.tool_blocks.push(block);
        }
        if let Some(usage) = extract_usage(event) {
            self.usage = usage;
        }
        if is_completion_event(event) {
            self.stop_reason = Some(extract_stop_reason(event).unwrap_or_else(|| "end_turn".into()));
            self.stopped = true;
        }
        out
    }

    fn finish_events(&mut self) -> Vec<Bytes> {
        let mut out = Vec::new();
        if self.text_open {
            out.push(response::content_block_stop(self.text_index));
            self.text_open = false;
        }
        out.push(response::message_delta(self.stop_reason.as_deref(), self.usage.clone()));
        out.push(response::message_stop());
        out
    }

    fn finish_response(mut self) -> AnthropicResponse {
        let mut content = Vec::new();
        if !self.text.is_empty() {
            content.push(AnthropicContentBlock {
                kind: "text".into(),
                text: Some(self.text),
                id: None,
                name: None,
                input: None,
            });
        }
        content.extend(self.tool_blocks);
        AnthropicResponse {
            id: self.message_id,
            kind: "message".into(),
            role: "assistant".into(),
            model: self.model,
            content,
            stop_reason: self.stop_reason.take().or_else(|| Some("end_turn".into())),
            stop_sequence: None,
            usage: self.usage,
        }
    }
}

#[derive(Debug)]
struct ToolCall {
    call_id: String,
    name: String,
    arguments: Value,
}

fn extract_text_delta(event: &Value) -> Option<String> {
    let typ = event.get("type").and_then(Value::as_str).unwrap_or_default();
    if typ.contains("reasoning") {
        return None;
    }
    if typ.contains("output_text.delta") || typ.contains("text.delta") {
        return event.get("delta").and_then(Value::as_str).map(ToOwned::to_owned);
    }
    event
        .pointer("/response/output/0/content/0/text")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            if typ.contains("message.delta") {
                event.get("text").and_then(Value::as_str).map(ToOwned::to_owned)
            } else {
                None
            }
        })
}

fn extract_function_call(event: &Value) -> Option<ToolCall> {
    let item = event.get("item").or_else(|| event.get("output_item")).unwrap_or(event);
    let item_type = item.get("type").and_then(Value::as_str)?;
    if item_type != "function_call" {
        return None;
    }
    let call_id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("tool_call")
        .to_string();
    let name = item.get("name").and_then(Value::as_str).unwrap_or("tool").to_string();
    let arguments = item
        .get("arguments")
        .and_then(Value::as_str)
        .and_then(|raw| serde_json::from_str(raw).ok())
        .or_else(|| item.get("arguments").cloned())
        .unwrap_or_else(|| json!({}));
    Some(ToolCall {
        call_id,
        name,
        arguments,
    })
}

fn extract_usage(event: &Value) -> Option<AnthropicUsage> {
    let usage = event.get("usage").or_else(|| event.pointer("/response/usage"))?;
    let input = usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
    let output = usage.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
    let cached = usage
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Some(AnthropicUsage {
        input_tokens: input,
        output_tokens: output,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: cached,
    })
}

fn is_completion_event(event: &Value) -> bool {
    let typ = event.get("type").and_then(Value::as_str).unwrap_or_default();
    typ.contains("completed") || typ.contains("done") || event.get("response").is_some_and(|response| {
        response
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| status == "completed")
    })
}

fn extract_stop_reason(event: &Value) -> Option<String> {
    let reason = event
        .pointer("/response/incomplete_details/reason")
        .or_else(|| event.pointer("/response/status_details/reason"))
        .or_else(|| event.get("stop_reason"))
        .and_then(Value::as_str)?;
    Some(match reason {
        "max_output_tokens" | "max_tokens" => "max_tokens",
        "tool_calls" | "function_call" => "tool_use",
        _ => "end_turn",
    }
    .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;

    #[tokio::test]
    async fn accumulates_text_delta() {
        let chunks = vec![Ok(Bytes::from(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n\
             data: {\"type\":\"response.completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}\n\n",
        ))];
        let response = accumulate_response(Box::pin(stream::iter(chunks)), "gpt-5.4".into())
            .await
            .unwrap();
        assert_eq!(response.content[0].text.as_deref(), Some("hi"));
        assert_eq!(response.usage.input_tokens, 2);
    }
}

