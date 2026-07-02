use crate::{
    anthropic::{
        response,
        schema::{AnthropicContentBlock, AnthropicResponse, AnthropicTool, AnthropicUsage},
    },
    codex::client::ByteStream,
    error::Result,
};
use async_stream::try_stream;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    pin::Pin,
};
use tracing::{debug, info, warn};
use uuid::Uuid;

pub fn translate_stream(
    upstream: ByteStream,
    model: String,
    tool_catalog: ToolCatalog,
    request_id: Option<String>,
) -> Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>> {
    Box::pin(try_stream! {
        let message_id = format!("msg_{}", Uuid::new_v4().simple());
        let mut reducer = CodexReducer::new(message_id.clone(), model.clone(), tool_catalog, request_id.clone());
        yield response::message_start(&message_id, &model);
        let mut parser = SseParser::default();
        let mut event_index = 0_u64;
        futures_util::pin_mut!(upstream);
        while let Some(chunk) = upstream.next().await {
            for event in parser.push(&chunk?, request_id.as_deref()) {
                event_index += 1;
                let produced = reducer.process_event(&event);
                log_codex_event(&event, event_index, produced.len(), request_id.as_deref());
                for bytes in produced {
                    yield bytes;
                }
            }
        }
        for event in parser.finish(request_id.as_deref()) {
            event_index += 1;
            let produced = reducer.process_event(&event);
            log_codex_event(&event, event_index, produced.len(), request_id.as_deref());
            for bytes in produced {
                yield bytes;
            }
        }
        info!(
            request_id = request_id.as_deref().unwrap_or("untracked"),
            event_count = event_index,
            text_chars = reducer.text.chars().count(),
            tool_count = reducer.tool_blocks.len(),
            stopped = reducer.stopped,
            stop_reason = reducer.stop_reason.as_deref().unwrap_or("none"),
            "finished reducing Codex stream"
        );
        for bytes in reducer.finish_events() {
            yield bytes;
        }
    })
}

pub async fn accumulate_response(
    upstream: ByteStream,
    model: String,
    tool_catalog: ToolCatalog,
    request_id: Option<String>,
) -> Result<AnthropicResponse> {
    let message_id = format!("msg_{}", Uuid::new_v4().simple());
    let mut reducer = CodexReducer::new(
        message_id.clone(),
        model.clone(),
        tool_catalog,
        request_id.clone(),
    );
    let mut parser = SseParser::default();
    futures_util::pin_mut!(upstream);
    while let Some(chunk) = upstream.next().await {
        for event in parser.push(&chunk?, request_id.as_deref()) {
            let _ = reducer.process_event(&event);
        }
    }
    for event in parser.finish(request_id.as_deref()) {
        let _ = reducer.process_event(&event);
    }
    Ok(reducer.finish_response())
}

#[derive(Debug, Clone, Default)]
pub struct ToolCatalog {
    tools: BTreeMap<String, ToolSpec>,
}

impl ToolCatalog {
    pub fn from_anthropic_tools(tools: Option<&[AnthropicTool]>) -> Self {
        let mut catalog = Self::default();
        let Some(tools) = tools else {
            return catalog;
        };
        for tool in tools {
            catalog.tools.insert(
                tool.name.clone(),
                ToolSpec {
                    required: schema_string_set(tool.input_schema.as_ref(), "required"),
                    properties: schema_property_set(tool.input_schema.as_ref()),
                },
            );
        }
        catalog
    }

    fn validate(&self, tool_name: &str, arguments: &Value) -> ToolValidation {
        let input_keys = value_key_set(arguments);
        let Some(spec) = self.tools.get(tool_name) else {
            return ToolValidation {
                known: false,
                required: BTreeSet::new(),
                properties: BTreeSet::new(),
                input_keys,
                missing_required: BTreeSet::new(),
                extra_input_keys: BTreeSet::new(),
            };
        };
        let missing_required = spec
            .required
            .difference(&input_keys)
            .cloned()
            .collect::<BTreeSet<_>>();
        let extra_input_keys = if spec.properties.is_empty() {
            BTreeSet::new()
        } else {
            input_keys
                .difference(&spec.properties)
                .cloned()
                .collect::<BTreeSet<_>>()
        };
        ToolValidation {
            known: true,
            required: spec.required.clone(),
            properties: spec.properties.clone(),
            input_keys,
            missing_required,
            extra_input_keys,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct ToolSpec {
    required: BTreeSet<String>,
    properties: BTreeSet<String>,
}

#[derive(Debug)]
struct ToolValidation {
    known: bool,
    required: BTreeSet<String>,
    properties: BTreeSet<String>,
    input_keys: BTreeSet<String>,
    missing_required: BTreeSet<String>,
    extra_input_keys: BTreeSet<String>,
}

#[derive(Debug, Default)]
struct SseParser {
    buffer: String,
}

impl SseParser {
    fn push(&mut self, chunk: &[u8], request_id: Option<&str>) -> Vec<Value> {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
        let mut events = Vec::new();
        while let Some(idx) = self.buffer.find("\n\n") {
            let raw = self.buffer[..idx].to_string();
            self.buffer.drain(..idx + 2);
            if let Some(value) = parse_sse_event(&raw, request_id) {
                events.push(value);
            }
        }
        events
    }

    fn finish(&mut self, request_id: Option<&str>) -> Vec<Value> {
        if self.buffer.trim().is_empty() {
            return Vec::new();
        }
        let raw = std::mem::take(&mut self.buffer);
        parse_sse_event(&raw, request_id).into_iter().collect()
    }
}

fn parse_sse_event(raw: &str, request_id: Option<&str>) -> Option<Value> {
    let data_lines = raw
        .lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
        .collect::<Vec<_>>();
    let data = data_lines.join("\n");
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    match serde_json::from_str(&data) {
        Ok(value) => Some(value),
        Err(err) => {
            let raw_line_count = raw.lines().count();
            let data_line_count = data_lines.len();
            let first_unprefixed_line = first_unprefixed_sse_line(raw).unwrap_or_default();
            warn!(
                request_id = request_id.unwrap_or("untracked"),
                error = %err,
                raw_len = raw.len(),
                data_len = data.len(),
                raw_line_count,
                data_line_count,
                first_unprefixed_line = %truncate_for_log_escaped(first_unprefixed_line, 240),
                event_preview = %truncate_for_log_escaped(&data, 1_000),
                raw_event_preview = %truncate_for_log_escaped(raw, 1_000),
                "failed to parse upstream SSE JSON event"
            );
            None
        }
    }
}

fn log_codex_event(
    event: &Value,
    event_index: u64,
    produced_chunk_count: usize,
    request_id: Option<&str>,
) {
    let typ = event_type(event);
    let should_log_info = event_index <= 8 || is_completion_event(event);
    if should_log_info {
        info!(
            request_id = request_id.unwrap_or("untracked"),
            event_index,
            event_type = typ,
            produced_chunk_count,
            text_delta_chars = text_delta_len(event),
            function_call_count = function_call_count(event),
            completion = is_completion_event(event),
            response_status = response_status(event).unwrap_or("none"),
            "processed Codex stream event"
        );
    } else {
        debug!(
            request_id = request_id.unwrap_or("untracked"),
            event_index,
            event_type = typ,
            produced_chunk_count,
            text_delta_chars = text_delta_len(event),
            function_call_count = function_call_count(event),
            completion = is_completion_event(event),
            response_status = response_status(event).unwrap_or("none"),
            "processed Codex stream event"
        );
    }
}

fn event_type(event: &Value) -> &str {
    event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("missing")
}

fn text_delta_len(event: &Value) -> usize {
    event
        .get("delta")
        .or_else(|| event.get("text"))
        .and_then(Value::as_str)
        .map(str::chars)
        .map(Iterator::count)
        .unwrap_or_default()
}

fn function_call_count(event: &Value) -> usize {
    let direct = event
        .get("item")
        .or_else(|| event.get("output_item"))
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|typ| typ == "function_call") as usize;
    let snapshot = event
        .pointer("/response/output")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
                .count()
        })
        .unwrap_or_default();
    direct + snapshot
}

fn response_status(event: &Value) -> Option<&str> {
    event
        .pointer("/response/status")
        .or_else(|| event.get("status"))
        .and_then(Value::as_str)
}

#[derive(Debug)]
struct CodexReducer {
    message_id: String,
    model: String,
    tool_catalog: ToolCatalog,
    request_id: Option<String>,
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
    fn new(
        message_id: String,
        model: String,
        tool_catalog: ToolCatalog,
        request_id: Option<String>,
    ) -> Self {
        Self {
            message_id,
            model,
            tool_catalog,
            request_id,
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
        if let Some(delta) = extract_text_delta(event, self.text.is_empty()) {
            self.push_text(&mut out, &delta);
        }
        for tool in extract_function_calls(event, self.request_id.as_deref()) {
            if self
                .tool_blocks
                .iter()
                .any(|block| block.id.as_deref() == Some(tool.call_id.as_str()))
            {
                continue;
            }
            let validation = self.tool_catalog.validate(&tool.name, &tool.arguments);
            if !validation.known {
                warn!(
                    request_id = self.request_id.as_deref().unwrap_or("untracked"),
                    call_id = %tool.call_id,
                    tool_name = %tool.name,
                    input_kind = value_kind(&tool.arguments),
                    input_keys = %join_set(&validation.input_keys),
                    raw_arguments = %truncate_for_log(&tool.arguments.to_string(), 1_000),
                    "upstream requested a tool that Claude Code did not offer; skipping tool_use"
                );
                self.push_text(
                    &mut out,
                    &format!(
                        "Skipped unsupported upstream tool call `{}` because Claude Code did not offer that tool.",
                        tool.name
                    ),
                );
                continue;
            }
            if !validation.missing_required.is_empty() {
                warn!(
                    request_id = self.request_id.as_deref().unwrap_or("untracked"),
                    call_id = %tool.call_id,
                    tool_name = %tool.name,
                    required = %join_set(&validation.required),
                    properties = %join_set(&validation.properties),
                    missing_required = %join_set(&validation.missing_required),
                    input_kind = value_kind(&tool.arguments),
                    input_keys = %join_set(&validation.input_keys),
                    raw_arguments = %truncate_for_log(&tool.arguments.to_string(), 1_000),
                    "upstream tool call is missing required Claude Code input keys; skipping tool_use"
                );
                self.push_text(
                    &mut out,
                    &format!(
                        "Skipped invalid upstream tool call `{}` because required input keys were missing: {}.",
                        tool.name,
                        join_set(&validation.missing_required)
                    ),
                );
                continue;
            }
            let index = self.next_index;
            self.next_index += 1;
            if self.text_open {
                out.push(response::content_block_stop(self.text_index));
                self.text_open = false;
            }
            info!(
                request_id = self.request_id.as_deref().unwrap_or("untracked"),
                call_id = %tool.call_id,
                tool_name = %tool.name,
                input_kind = value_kind(&tool.arguments),
                input_keys = %join_set(&validation.input_keys),
                required = %join_set(&validation.required),
                properties = %join_set(&validation.properties),
                extra_input_keys = %join_set(&validation.extra_input_keys),
                "emitting Claude tool_use"
            );
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
                    "input": {}
                }),
            ));
            let arguments_json = tool.arguments.to_string();
            if arguments_json != "{}" {
                out.push(response::content_block_delta(
                    index,
                    json!({
                        "type": "input_json_delta",
                        "partial_json": arguments_json
                    }),
                ));
            }
            out.push(response::content_block_stop(index));
            self.tool_blocks.push(block);
        }
        if let Some(usage) = extract_usage(event) {
            self.usage = usage;
        }
        if is_completion_event(event) {
            self.stop_reason =
                Some(extract_stop_reason(event).unwrap_or_else(|| self.default_stop_reason()));
            self.stopped = true;
        }
        out
    }

    fn push_text(&mut self, out: &mut Vec<Bytes>, delta: &str) {
        if !self.text_open {
            self.text_open = true;
            self.text_index = self.next_index;
            self.next_index += 1;
            out.push(response::content_block_start(
                self.text_index,
                json!({ "type": "text", "text": "" }),
            ));
        }
        self.text.push_str(delta);
        out.push(response::content_block_delta(
            self.text_index,
            json!({ "type": "text_delta", "text": delta }),
        ));
    }

    fn finish_events(&mut self) -> Vec<Bytes> {
        let mut out = Vec::new();
        if self.text_open {
            out.push(response::content_block_stop(self.text_index));
            self.text_open = false;
        }
        let stop_reason = self
            .stop_reason
            .clone()
            .unwrap_or_else(|| self.default_stop_reason());
        out.push(response::message_delta(
            Some(stop_reason.as_str()),
            self.usage.clone(),
        ));
        out.push(response::message_stop());
        out
    }

    fn finish_response(mut self) -> AnthropicResponse {
        let mut content = Vec::new();
        let stop_reason = self
            .stop_reason
            .take()
            .unwrap_or_else(|| self.default_stop_reason());
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
            stop_reason: Some(stop_reason),
            stop_sequence: None,
            usage: self.usage,
        }
    }

    fn default_stop_reason(&self) -> String {
        if self.tool_blocks.is_empty() {
            "end_turn".into()
        } else {
            "tool_use".into()
        }
    }
}

#[derive(Debug)]
struct ToolCall {
    call_id: String,
    name: String,
    arguments: Value,
}

fn extract_text_delta(event: &Value, allow_snapshot: bool) -> Option<String> {
    let typ = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if typ.contains("reasoning") {
        return None;
    }
    if typ.contains("output_text.delta") || typ.contains("text.delta") {
        return event
            .get("delta")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
    }
    if typ.contains("message.delta") {
        return event
            .get("text")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
    }
    if allow_snapshot {
        response_snapshot_text(event)
    } else {
        None
    }
}

fn response_snapshot_text(event: &Value) -> Option<String> {
    let output = event
        .pointer("/response/output")
        .and_then(Value::as_array)?;
    let mut text = String::new();
    for item in output {
        let Some(content) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in content {
            if part.get("type").and_then(Value::as_str) == Some("output_text") {
                if let Some(part_text) = part.get("text").and_then(Value::as_str) {
                    text.push_str(part_text);
                }
            }
        }
    }
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn extract_function_calls(event: &Value, request_id: Option<&str>) -> Vec<ToolCall> {
    let typ = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if typ.contains("output_item.added") {
        return Vec::new();
    }

    let mut out = Vec::new();
    if let Some(item) = event.get("item").or_else(|| event.get("output_item")) {
        if let Some(tool) = tool_call_from_item(item, request_id) {
            out.push(tool);
        }
    }
    if let Some(output) = event.pointer("/response/output").and_then(Value::as_array) {
        for item in output {
            if let Some(tool) = tool_call_from_item(item, request_id) {
                out.push(tool);
            }
        }
    }
    out
}

fn tool_call_from_item(item: &Value, request_id: Option<&str>) -> Option<ToolCall> {
    let item_type = item.get("type").and_then(Value::as_str)?;
    if item_type != "function_call" {
        return None;
    }
    if item
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status == "in_progress")
        && item
            .get("arguments")
            .and_then(Value::as_str)
            .map_or(true, str::is_empty)
    {
        return None;
    }
    let call_id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("tool_call")
        .to_string();
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("tool")
        .to_string();
    let arguments = sanitize_tool_arguments(
        &name,
        parse_tool_arguments(item.get("arguments"), &call_id, &name, request_id),
        &call_id,
        request_id,
    );
    Some(ToolCall {
        call_id,
        name,
        arguments,
    })
}

fn parse_tool_arguments(
    arguments: Option<&Value>,
    call_id: &str,
    name: &str,
    request_id: Option<&str>,
) -> Value {
    let Some(arguments) = arguments else {
        return json!({});
    };
    if let Some(raw) = arguments.as_str() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return json!({});
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(value) if value.is_object() => value,
            Ok(value) => {
                warn!(
                    request_id = request_id.unwrap_or("untracked"),
                    call_id = %call_id,
                    tool_name = %name,
                    argument_kind = value_kind(&value),
                    raw_arguments = %truncate_for_log(trimmed, 1_000),
                    "upstream function_call arguments parsed to a non-object; replacing with empty object"
                );
                json!({})
            }
            Err(err) => {
                warn!(
                    request_id = request_id.unwrap_or("untracked"),
                    call_id = %call_id,
                    tool_name = %name,
                    error = %err,
                    raw_arguments = %truncate_for_log(trimmed, 1_000),
                    "upstream function_call arguments are invalid JSON; replacing with empty object"
                );
                json!({})
            }
        }
    } else if arguments.is_object() {
        arguments.clone()
    } else {
        warn!(
            request_id = request_id.unwrap_or("untracked"),
            call_id = %call_id,
            tool_name = %name,
            argument_kind = value_kind(arguments),
            raw_arguments = %truncate_for_log(&arguments.to_string(), 1_000),
            "upstream function_call arguments are not an object; replacing with empty object"
        );
        json!({})
    }
}

fn sanitize_tool_arguments(
    name: &str,
    mut arguments: Value,
    call_id: &str,
    request_id: Option<&str>,
) -> Value {
    if name == "Read" {
        if let Some(object) = arguments.as_object_mut() {
            if object.get("pages").and_then(Value::as_str) == Some("") {
                object.remove("pages");
                info!(
                    request_id = request_id.unwrap_or("untracked"),
                    call_id = %call_id,
                    tool_name = %name,
                    "removed empty Read.pages argument before emitting Claude tool_use"
                );
            }
        }
    }
    arguments
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn value_key_set(value: &Value) -> BTreeSet<String> {
    value
        .as_object()
        .map(|object| object.keys().cloned().collect::<BTreeSet<_>>())
        .unwrap_or_default()
}

fn schema_string_set(schema: Option<&Value>, key: &str) -> BTreeSet<String> {
    schema
        .and_then(|schema| schema.get(key))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn schema_property_set(schema: Option<&Value>) -> BTreeSet<String> {
    schema
        .and_then(|schema| schema.get("properties"))
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default()
}

fn join_set(values: &BTreeSet<String>) -> String {
    values.iter().cloned().collect::<Vec<_>>().join("|")
}

fn extract_usage(event: &Value) -> Option<AnthropicUsage> {
    let usage = event
        .get("usage")
        .or_else(|| event.pointer("/response/usage"))?;
    let input = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached = usage
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Some(AnthropicUsage {
        input_tokens: input.saturating_sub(cached),
        output_tokens: output,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: cached,
    })
}

fn is_completion_event(event: &Value) -> bool {
    let typ = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    typ.contains("completed")
        || typ.contains("done")
        || event.get("response").is_some_and(|response| {
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
    Some(
        match reason {
            "max_output_tokens" | "max_tokens" => "max_tokens",
            "tool_calls" | "function_call" => "tool_use",
            _ => "end_turn",
        }
        .to_string(),
    )
}

fn truncate_for_log(value: &str, max_chars: usize) -> String {
    let mut out = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        out.push_str("...[truncated]");
    }
    out
}

fn truncate_for_log_escaped(value: &str, max_chars: usize) -> String {
    truncate_for_log(&escape_for_log(value), max_chars)
}

fn escape_for_log(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn first_unprefixed_sse_line(raw: &str) -> Option<&str> {
    raw.lines().find(|line| {
        !line.is_empty()
            && !line.starts_with(':')
            && !line.starts_with("data:")
            && !line.starts_with("event:")
            && !line.starts_with("id:")
            && !line.starts_with("retry:")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{stream, StreamExt};

    fn tool_catalog(name: &str, required: &[&str], properties: &[&str]) -> ToolCatalog {
        let properties = properties
            .iter()
            .map(|property| (property.to_string(), json!({ "type": "string" })))
            .collect::<serde_json::Map<_, _>>();
        let tool = AnthropicTool {
            name: name.into(),
            description: Some(format!("{name} tool")),
            input_schema: Some(json!({
                "type": "object",
                "properties": properties,
                "required": required
            })),
            extra: Default::default(),
        };
        ToolCatalog::from_anthropic_tools(Some(&[tool]))
    }

    fn read_tool_catalog(required: &[&str]) -> ToolCatalog {
        tool_catalog("Read", required, &["path", "pages"])
    }

    #[tokio::test]
    async fn accumulates_text_delta() {
        let chunks = vec![Ok(Bytes::from(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n\
             data: {\"type\":\"response.completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}\n\n",
        ))];
        let response = accumulate_response(
            Box::pin(stream::iter(chunks)),
            "gpt-5.4".into(),
            ToolCatalog::default(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(response.content[0].text.as_deref(), Some("hi"));
        assert_eq!(response.usage.input_tokens, 2);
    }

    #[tokio::test]
    async fn completed_snapshot_does_not_duplicate_text_delta() {
        let chunks = vec![Ok(Bytes::from(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n\
             data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}]}],\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n",
        ))];
        let response = accumulate_response(
            Box::pin(stream::iter(chunks)),
            "gpt-5.4".into(),
            ToolCatalog::default(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(response.content[0].text.as_deref(), Some("hi"));
    }

    #[tokio::test]
    async fn completed_snapshot_supplies_text_without_deltas() {
        let chunks = vec![Ok(Bytes::from(
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"snapshot\"}]}],\"usage\":{\"input_tokens\":4,\"output_tokens\":1}}}\n\n",
        ))];
        let response = accumulate_response(
            Box::pin(stream::iter(chunks)),
            "gpt-5.4".into(),
            ToolCatalog::default(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(response.content[0].text.as_deref(), Some("snapshot"));
    }

    #[tokio::test]
    async fn completed_snapshot_supplies_function_call() {
        let chunks = vec![Ok(Bytes::from(
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"Read\",\"arguments\":\"{\\\"path\\\":\\\"Cargo.toml\\\"}\"}],\"usage\":{\"input_tokens\":4,\"output_tokens\":1}}}\n\n",
        ))];
        let response = accumulate_response(
            Box::pin(stream::iter(chunks)),
            "gpt-5.4".into(),
            read_tool_catalog(&["path"]),
            None,
        )
        .await
        .unwrap();
        assert_eq!(response.content[0].kind, "tool_use");
        assert_eq!(response.content[0].id.as_deref(), Some("call_1"));
        assert_eq!(response.content[0].name.as_deref(), Some("Read"));
        assert_eq!(response.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(
            response.content[0].input.as_ref().unwrap()["path"],
            "Cargo.toml"
        );
    }

    #[tokio::test]
    async fn streaming_function_call_uses_input_json_delta() {
        let chunks = vec![Ok(Bytes::from(
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"Read\",\"arguments\":\"{\\\"path\\\":\\\"Cargo.toml\\\"}\"}],\"usage\":{\"input_tokens\":4,\"output_tokens\":1}}}\n\n",
        ))];
        let stream = translate_stream(
            Box::pin(stream::iter(chunks)),
            "gpt-5.4".into(),
            read_tool_catalog(&["path"]),
            None,
        );
        let body = stream
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|chunk| String::from_utf8(chunk.unwrap().to_vec()).unwrap())
            .collect::<String>();

        assert!(body.contains("\"input\":{}"));
        assert!(body.contains("\"type\":\"input_json_delta\""));
        assert!(body.contains("\\\"path\\\":\\\"Cargo.toml\\\""));
    }

    #[tokio::test]
    async fn read_pages_empty_string_is_removed() {
        let chunks = vec![Ok(Bytes::from(
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"Read\",\"arguments\":\"{\\\"path\\\":\\\"Cargo.toml\\\",\\\"pages\\\":\\\"\\\"}\"}],\"usage\":{\"input_tokens\":4,\"output_tokens\":1}}}\n\n",
        ))];
        let response = accumulate_response(
            Box::pin(stream::iter(chunks)),
            "gpt-5.4".into(),
            read_tool_catalog(&["path"]),
            None,
        )
        .await
        .unwrap();

        let input = response.content[0].input.as_ref().unwrap();
        assert_eq!(input["path"], "Cargo.toml");
        assert!(input.get("pages").is_none());
    }

    #[tokio::test]
    async fn offered_task_create_is_forwarded() {
        let chunks = vec![Ok(Bytes::from(
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"TaskCreate\",\"arguments\":\"{\\\"subject\\\":\\\"Fix proxy\\\",\\\"description\\\":\\\"Investigate logs\\\",\\\"activeForm\\\":\\\"Fixing proxy\\\"}\"}],\"usage\":{\"input_tokens\":4,\"output_tokens\":1}}}\n\n",
        ))];
        let response = accumulate_response(
            Box::pin(stream::iter(chunks)),
            "gpt-5.4".into(),
            tool_catalog(
                "TaskCreate",
                &["subject"],
                &["subject", "description", "activeForm"],
            ),
            None,
        )
        .await
        .unwrap();

        assert_eq!(response.content[0].kind, "tool_use");
        assert_eq!(response.content[0].name.as_deref(), Some("TaskCreate"));
        assert_eq!(
            response.content[0].input.as_ref().unwrap()["subject"],
            "Fix proxy"
        );
    }

    #[tokio::test]
    async fn malformed_function_call_arguments_are_replaced_with_empty_object() {
        let chunks = vec![Ok(Bytes::from(
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"Read\",\"arguments\":\"not json\"}],\"usage\":{\"input_tokens\":4,\"output_tokens\":1}}}\n\n",
        ))];
        let response = accumulate_response(
            Box::pin(stream::iter(chunks)),
            "gpt-5.4".into(),
            read_tool_catalog(&[]),
            None,
        )
        .await
        .unwrap();
        assert_eq!(response.content[0].kind, "tool_use");
        assert_eq!(response.content[0].name.as_deref(), Some("Read"));
        assert_eq!(response.content[0].input.as_ref().unwrap(), &json!({}));
    }

    #[tokio::test]
    async fn unoffered_function_call_is_not_forwarded_to_claude() {
        let chunks = vec![Ok(Bytes::from(
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"TaskCreate\",\"arguments\":\"{\\\"subject\\\":\\\"hi\\\"}\"}],\"usage\":{\"input_tokens\":4,\"output_tokens\":1}}}\n\n",
        ))];
        let response = accumulate_response(
            Box::pin(stream::iter(chunks)),
            "gpt-5.4".into(),
            read_tool_catalog(&["path"]),
            Some("req_test".into()),
        )
        .await
        .unwrap();
        assert_eq!(response.content[0].kind, "text");
        assert!(response
            .content
            .first()
            .and_then(|block| block.text.as_deref())
            .unwrap()
            .contains("Skipped unsupported upstream tool call `TaskCreate`"));
        assert_eq!(response.stop_reason.as_deref(), Some("end_turn"));
    }

    #[tokio::test]
    async fn cached_input_tokens_are_exposed_as_cache_reads() {
        let chunks = vec![Ok(Bytes::from(
            "data: {\"type\":\"response.completed\",\"usage\":{\"input_tokens\":100,\"output_tokens\":5,\"input_tokens_details\":{\"cached_tokens\":25}}}\n\n",
        ))];
        let response = accumulate_response(
            Box::pin(stream::iter(chunks)),
            "gpt-5.4".into(),
            ToolCatalog::default(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(response.usage.input_tokens, 75);
        assert_eq!(response.usage.cache_read_input_tokens, 25);
        assert_eq!(response.usage.output_tokens, 5);
    }
}
