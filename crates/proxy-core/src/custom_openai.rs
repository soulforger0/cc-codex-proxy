use crate::{
    anthropic::{
        response,
        schema::{
            AnthropicContentBlock, AnthropicRequest, AnthropicResponse, AnthropicTool,
            AnthropicUsage,
        },
    },
    canonical::{
        canonicalize_anthropic_tools, canonicalize_json_schema, is_hosted_web_search_tool,
    },
    codex::{
        client::ByteStream,
        translate::{translate_request, ResponsesRequest},
    },
    config::{CustomOpenAIConfig, CustomOpenAIProtocol, CUSTOM_OPENAI_API_KEY_ENV},
    error::{ProxyError, Result},
    http_client::{
        build_client, duration_from_millis, monitor_idle_stream, optional_duration_from_millis,
        HttpClientTuning,
    },
    model::ResolvedModel,
};
use async_stream::try_stream;
use bytes::{Bytes, BytesMut};
use futures_util::{Stream, StreamExt, TryStreamExt};
use http::StatusCode;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    env, fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    pin::Pin,
};
use tracing::{info, warn};
use uuid::Uuid;

pub struct CustomOpenAIResponse {
    pub body: ByteStream,
    pub status: StatusCode,
}

#[derive(Clone)]
pub struct CustomOpenAIClient {
    http: reqwest::Client,
    config: CustomOpenAIConfig,
    api_key_file: PathBuf,
}

impl CustomOpenAIClient {
    pub fn new(config: CustomOpenAIConfig, api_key_file: PathBuf) -> Result<Self> {
        let http = build_client(HttpClientTuning {
            connect_timeout_ms: config.connect_timeout_ms,
            pool_idle_timeout_ms: config.pool_idle_timeout_ms,
            pool_max_idle_per_host: config.pool_max_idle_per_host,
            tcp_keepalive_ms: config.tcp_keepalive_ms,
        })?;
        Ok(Self {
            http,
            config,
            api_key_file,
        })
    }

    pub fn api_key_status(&self) -> CustomOpenAIApiKeyStatus {
        api_key_status(&self.api_key_file)
    }

    pub fn protocol(&self) -> CustomOpenAIProtocol {
        self.config.protocol.clone()
    }

    pub fn base_url_configured(&self) -> bool {
        !self.config.base_url.trim().is_empty()
    }

    pub async fn post_responses(&self, body: &ResponsesRequest) -> Result<CustomOpenAIResponse> {
        let url = responses_url(&self.config.base_url)?;
        info!(
            model = %body.model,
            input_items = body.input.len(),
            tool_count = body.tools.as_ref().map_or(0, Vec::len),
            "posting custom OpenAI Responses request"
        );
        self.post_json(url, body, "Responses").await
    }

    pub async fn post_chat(&self, body: &ChatCompletionsRequest) -> Result<CustomOpenAIResponse> {
        let url = chat_completions_url(&self.config.base_url)?;
        info!(
            model = %body.model,
            message_count = body.messages.len(),
            stream = body.stream,
            tool_count = body.tools.as_ref().map_or(0, Vec::len),
            "posting custom OpenAI Chat Completions request"
        );
        self.post_json(url, body, "Chat Completions").await
    }

    async fn post_json<T>(&self, url: String, body: &T, label: &str) -> Result<CustomOpenAIResponse>
    where
        T: Serialize + ?Sized,
    {
        let response = tokio::time::timeout(
            duration_from_millis(self.config.header_timeout_ms),
            self.http
                .post(url)
                .headers(self.headers()?)
                .json(body)
                .send(),
        )
        .await
        .map_err(|_| {
            ProxyError::Transport(format!(
                "timed out waiting for custom OpenAI {label} response headers"
            ))
        })??;
        let status =
            StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        info!(%status, "received custom OpenAI response headers");
        if !status.is_success() {
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .map(ToOwned::to_owned);
            let body = response.text().await.unwrap_or_default();
            warn!(
                %status,
                retry_after = ?retry_after,
                upstream_body = %truncate_for_log(&body, 4_000),
                "custom OpenAI request failed"
            );
            return Err(ProxyError::Upstream {
                status,
                body,
                retry_after,
            });
        }
        Ok(CustomOpenAIResponse {
            body: monitor_idle_stream(
                response.bytes_stream().map_err(ProxyError::from),
                format!("custom OpenAI {label}"),
                None,
                duration_from_millis(self.config.stream_idle_warn_ms),
                optional_duration_from_millis(self.config.stream_idle_timeout_ms),
            ),
            status,
        })
    }

    fn headers(&self) -> Result<reqwest::header::HeaderMap> {
        let mut headers = reqwest::header::HeaderMap::new();
        insert_header(
            &mut headers,
            reqwest::header::CONTENT_TYPE,
            "application/json",
        )?;
        insert_header(&mut headers, reqwest::header::ACCEPT, "text/event-stream")?;
        insert_header(
            &mut headers,
            reqwest::header::USER_AGENT,
            &self.config.user_agent,
        )?;
        if let Some(api_key) = resolve_api_key(&self.api_key_file)? {
            insert_header(
                &mut headers,
                reqwest::header::AUTHORIZATION,
                &format!("Bearer {api_key}"),
            )?;
        }
        Ok(headers)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomOpenAIApiKeyStatus {
    pub configured: bool,
    pub source: Option<String>,
}

pub fn api_key_status(path: &Path) -> CustomOpenAIApiKeyStatus {
    if env_api_key().is_some() {
        return CustomOpenAIApiKeyStatus {
            configured: true,
            source: Some(CUSTOM_OPENAI_API_KEY_ENV.into()),
        };
    }
    match fs::read_to_string(path) {
        Ok(raw) if !raw.trim().is_empty() => CustomOpenAIApiKeyStatus {
            configured: true,
            source: Some("local api key file".into()),
        },
        _ => CustomOpenAIApiKeyStatus {
            configured: false,
            source: None,
        },
    }
}

pub fn store_api_key(path: &Path, api_key: &str) -> Result<()> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err(ProxyError::InvalidRequest(
            "Custom OpenAI API key cannot be empty".into(),
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    writeln!(file, "{api_key}")?;
    Ok(())
}

pub fn clear_api_key(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

fn resolve_api_key(path: &Path) -> Result<Option<String>> {
    if let Some(key) = env_api_key() {
        return Ok(Some(key));
    }
    match fs::read_to_string(path) {
        Ok(raw) => {
            let key = raw.trim().to_string();
            Ok((!key.is_empty()).then_some(key))
        }
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn env_api_key() -> Option<String> {
    env::var(CUSTOM_OPENAI_API_KEY_ENV)
        .ok()
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionsRequest {
    pub model: String,
    pub messages: Vec<Value>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<Value>,
}

pub fn translate_chat_request(
    request: &AnthropicRequest,
    resolved: &ResolvedModel,
) -> Result<ChatCompletionsRequest> {
    let mut messages = Vec::new();
    if let Some(system) = &request.system {
        let content = system_to_text(system);
        if !content.is_empty() {
            messages.push(json!({ "role": "system", "content": content }));
        }
    }
    for message in &request.messages {
        match message.role.as_str() {
            "user" => push_user_chat_messages(&mut messages, &message.content),
            "assistant" => push_assistant_chat_message(&mut messages, &message.content),
            "system" => {
                let content = content_to_text(&message.content);
                if !content.is_empty() {
                    messages.push(json!({ "role": "system", "content": content }));
                }
            }
            other => {
                return Err(ProxyError::InvalidRequest(format!(
                    "unsupported message role \"{other}\""
                )));
            }
        }
    }

    let tools = request
        .tools
        .as_ref()
        .map(|tools| translate_chat_tools(tools))
        .transpose()?
        .filter(|tools| !tools.is_empty());
    let tool_choice = if tools.is_some() {
        request.tool_choice.as_ref().map(translate_chat_tool_choice)
    } else {
        None
    };
    Ok(ChatCompletionsRequest {
        model: resolved.upstream_model.clone(),
        messages,
        stream: request.wants_stream(),
        tools,
        tool_choice,
        max_tokens: request.max_tokens,
        temperature: request.temperature,
        top_p: request.top_p,
        response_format: chat_response_format(request),
    })
}

pub fn translate_responses_request(
    request: &AnthropicRequest,
    resolved: &ResolvedModel,
    session_id: Option<&str>,
) -> Result<ResponsesRequest> {
    translate_request(request, resolved, session_id)
}

pub fn translate_chat_stream(
    upstream: ByteStream,
    model: String,
    request_id: Option<String>,
) -> Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>> {
    Box::pin(try_stream! {
        let message_id = format!("msg_{}", Uuid::new_v4().simple());
        let mut reducer = ChatStreamReducer::new(message_id.clone(), model.clone());
        yield response::message_start(&message_id, &model);
        let mut parser = SseParser::default();
        futures_util::pin_mut!(upstream);
        while let Some(chunk) = upstream.next().await {
            for event in parser.push(&chunk?, request_id.as_deref()) {
                for bytes in reducer.process_event(&event) {
                    yield bytes;
                }
            }
        }
        for event in parser.finish(request_id.as_deref()) {
            for bytes in reducer.process_event(&event) {
                yield bytes;
            }
        }
        for bytes in reducer.finish_events() {
            yield bytes;
        }
    })
}

pub async fn accumulate_chat_response(
    upstream: ByteStream,
    model: String,
) -> Result<AnthropicResponse> {
    let value = read_json_body(upstream).await?;
    chat_response_from_value(value, model)
}

async fn read_json_body(upstream: ByteStream) -> Result<Value> {
    futures_util::pin_mut!(upstream);
    let mut bytes = BytesMut::new();
    while let Some(chunk) = upstream.next().await {
        bytes.extend_from_slice(&chunk?);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn chat_response_from_value(value: Value, model: String) -> Result<AnthropicResponse> {
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| {
            ProxyError::Transport("custom OpenAI chat response had no choices".into())
        })?;
    let message = choice.get("message").ok_or_else(|| {
        ProxyError::Transport("custom OpenAI chat response had no message".into())
    })?;
    let mut content = Vec::new();
    if let Some(text) = message.get("content").and_then(Value::as_str) {
        if !text.is_empty() {
            content.push(AnthropicContentBlock {
                kind: "text".into(),
                text: Some(text.to_string()),
                id: None,
                name: None,
                input: None,
            });
        }
    }
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for tool_call in tool_calls {
            let function = tool_call.get("function").unwrap_or(&Value::Null);
            content.push(AnthropicContentBlock {
                kind: "tool_use".into(),
                text: None,
                id: Some(
                    tool_call
                        .get("id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                        .unwrap_or_else(|| format!("toolu_{}", Uuid::new_v4().simple())),
                ),
                name: Some(
                    function
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string(),
                ),
                input: Some(parse_tool_arguments(
                    function
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}"),
                )),
            });
        }
    }
    let usage = usage_from_value(value.get("usage"));
    let stop_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(map_chat_finish_reason);
    Ok(AnthropicResponse {
        id: format!("msg_{}", Uuid::new_v4().simple()),
        kind: "message".into(),
        role: "assistant".into(),
        model,
        content,
        stop_reason,
        stop_sequence: None,
        usage,
    })
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
            warn!(
                request_id = request_id.unwrap_or("untracked"),
                error = %err,
                event_preview = %truncate_for_log(&data, 1_000),
                "failed to parse custom OpenAI chat SSE JSON event"
            );
            None
        }
    }
}

struct ChatStreamReducer {
    message_id: String,
    model: String,
    next_block_index: usize,
    text_block_index: Option<usize>,
    tool_blocks: BTreeMap<usize, ToolStreamBlock>,
    usage: AnthropicUsage,
    stop_reason: Option<String>,
}

impl ChatStreamReducer {
    fn new(message_id: String, model: String) -> Self {
        Self {
            message_id,
            model,
            next_block_index: 0,
            text_block_index: None,
            tool_blocks: BTreeMap::new(),
            usage: AnthropicUsage {
                input_tokens: 0,
                output_tokens: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            stop_reason: None,
        }
    }

    fn process_event(&mut self, event: &Value) -> Vec<Bytes> {
        if let Some(usage) = event.get("usage") {
            self.usage = usage_from_value(Some(usage));
        }
        let mut out = Vec::new();
        let Some(choice) = event
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            return out;
        };
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.stop_reason = Some(map_chat_finish_reason(reason));
        }
        let Some(delta) = choice.get("delta") else {
            return out;
        };
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            if !text.is_empty() {
                let index = self.ensure_text_block(&mut out);
                out.push(response::content_block_delta(
                    index,
                    json!({ "type": "text_delta", "text": text }),
                ));
            }
        }
        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in tool_calls {
                self.process_tool_call_delta(call, &mut out);
            }
        }
        out
    }

    fn ensure_text_block(&mut self, out: &mut Vec<Bytes>) -> usize {
        if let Some(index) = self.text_block_index {
            return index;
        }
        let index = self.next_block_index;
        self.next_block_index += 1;
        self.text_block_index = Some(index);
        out.push(response::content_block_start(
            index,
            json!({ "type": "text", "text": "" }),
        ));
        index
    }

    fn process_tool_call_delta(&mut self, call: &Value, out: &mut Vec<Bytes>) {
        let upstream_index = call
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0);
        let function = call.get("function").unwrap_or(&Value::Null);
        let created = !self.tool_blocks.contains_key(&upstream_index);
        if created {
            let block_index = self.next_block_index;
            self.next_block_index += 1;
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| format!("toolu_{}", Uuid::new_v4().simple()));
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_string();
            self.tool_blocks.insert(
                upstream_index,
                ToolStreamBlock {
                    block_index,
                    _id: id.clone(),
                    _name: name.clone(),
                },
            );
            out.push(response::content_block_start(
                block_index,
                json!({ "type": "tool_use", "id": id, "name": name, "input": {} }),
            ));
        }
        let Some(block) = self.tool_blocks.get(&upstream_index) else {
            return;
        };
        if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
            if !arguments.is_empty() {
                out.push(response::content_block_delta(
                    block.block_index,
                    json!({ "type": "input_json_delta", "partial_json": arguments }),
                ));
            }
        }
    }

    fn finish_events(self) -> Vec<Bytes> {
        let mut out = Vec::new();
        if let Some(index) = self.text_block_index {
            out.push(response::content_block_stop(index));
        }
        for block in self.tool_blocks.values() {
            out.push(response::content_block_stop(block.block_index));
        }
        out.push(response::message_delta(
            self.stop_reason.as_deref(),
            self.usage,
        ));
        out.push(response::message_stop());
        let _ = (&self.message_id, &self.model);
        out
    }
}

struct ToolStreamBlock {
    block_index: usize,
    _id: String,
    _name: String,
}

fn push_user_chat_messages(messages: &mut Vec<Value>, content: &Value) {
    let mut parts = Vec::new();
    for block in normalized_blocks(content) {
        match block.get("type").and_then(Value::as_str).unwrap_or("text") {
            "text" => parts.push(json!({
                "type": "text",
                "text": block.get("text").and_then(Value::as_str).unwrap_or_default(),
            })),
            "image" => {
                if let Some(image) = block_to_chat_image(&block) {
                    parts.push(image);
                }
            }
            "tool_result" => {
                flush_user_parts(messages, &mut parts);
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": block.get("tool_use_id").and_then(Value::as_str).unwrap_or("tool_call"),
                    "content": tool_result_output(&block),
                }));
            }
            _ => parts.push(json!({ "type": "text", "text": block.to_string() })),
        }
    }
    flush_user_parts(messages, &mut parts);
}

fn push_assistant_chat_message(messages: &mut Vec<Value>, content: &Value) {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for block in normalized_blocks(content) {
        match block.get("type").and_then(Value::as_str).unwrap_or("text") {
            "text" => {
                if let Some(part) = block.get("text").and_then(Value::as_str) {
                    text.push_str(part);
                }
            }
            "tool_use" => tool_calls.push(json!({
                "id": block.get("id").and_then(Value::as_str).unwrap_or("tool_call"),
                "type": "function",
                "function": {
                    "name": block.get("name").and_then(Value::as_str).unwrap_or("tool"),
                    "arguments": block.get("input").filter(|input| input.is_object()).cloned().unwrap_or_else(|| json!({})).to_string(),
                }
            })),
            _ => text.push_str(&block.to_string()),
        }
    }
    let mut message = serde_json::Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert("content".into(), Value::String(text));
    if !tool_calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    messages.push(Value::Object(message));
}

fn flush_user_parts(messages: &mut Vec<Value>, parts: &mut Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    let content =
        if parts.len() == 1 && parts[0].get("type").and_then(Value::as_str) == Some("text") {
            parts[0]
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
                .into()
        } else {
            Value::Array(std::mem::take(parts))
        };
    messages.push(json!({ "role": "user", "content": content }));
    parts.clear();
}

fn normalized_blocks(content: &Value) -> Vec<Value> {
    match content {
        Value::String(text) => vec![json!({ "type": "text", "text": text })],
        Value::Array(items) => items.clone(),
        other => vec![json!({ "type": "text", "text": other.to_string() })],
    }
}

fn block_to_chat_image(block: &Value) -> Option<Value> {
    let source = block.get("source")?;
    match source.get("type").and_then(Value::as_str) {
        Some("base64") => {
            let media = source
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("image/png");
            let data = source.get("data").and_then(Value::as_str)?;
            Some(json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{media};base64,{data}") }
            }))
        }
        Some("url") => source.get("url").and_then(Value::as_str).map(|url| {
            json!({
                "type": "image_url",
                "image_url": { "url": url }
            })
        }),
        _ => None,
    }
}

fn tool_result_output(block: &Value) -> String {
    content_to_text(block.get("content").unwrap_or(&Value::Null))
}

fn content_to_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(ToOwned::to_owned)
                    .or_else(|| {
                        item.get("text")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                    })
                    .unwrap_or_else(|| item.to_string())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn system_to_text(system: &Value) -> String {
    content_to_text(system)
}

fn translate_chat_tools(tools: &[AnthropicTool]) -> Result<Vec<Value>> {
    let tools = canonicalize_anthropic_tools(tools.to_vec());
    let mut out = Vec::with_capacity(tools.len());
    for tool in &tools {
        if is_hosted_web_search_tool(tool) {
            continue;
        }
        let mut function = serde_json::Map::new();
        function.insert("name".into(), json!(tool.name.clone()));
        if let Some(description) = tool
            .description
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            function.insert("description".into(), json!(description));
        }
        function.insert(
            "parameters".into(),
            tool.input_schema
                .clone()
                .map(canonicalize_json_schema)
                .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
        );
        out.push(json!({
            "type": "function",
            "function": Value::Object(function),
        }));
    }
    Ok(dedupe_json_values(out))
}

fn translate_chat_tool_choice(choice: &Value) -> Value {
    match choice.get("type").and_then(Value::as_str) {
        Some("auto") => json!("auto"),
        Some("none") => json!("none"),
        Some("any") => json!("required"),
        Some("tool") => choice
            .get("name")
            .and_then(Value::as_str)
            .map(|name| json!({ "type": "function", "function": { "name": name } }))
            .unwrap_or_else(|| json!("required")),
        _ => json!("auto"),
    }
}

fn chat_response_format(request: &AnthropicRequest) -> Option<Value> {
    let format = request
        .output_config
        .as_ref()
        .and_then(|value| value.get("format"))
        .filter(|format| format.get("type").and_then(Value::as_str) == Some("json_schema"))?;
    Some(json!({
        "type": "json_schema",
        "json_schema": {
            "name": format.get("name").and_then(Value::as_str).unwrap_or("response"),
            "schema": format.get("schema").cloned().unwrap_or_else(|| json!({})),
            "strict": true,
        }
    }))
}

fn dedupe_json_values(values: Vec<Value>) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        let key = serde_json::to_string(&value).unwrap_or_else(|_| value.to_string());
        if seen.insert(key) {
            out.push(value);
        }
    }
    out
}

fn usage_from_value(value: Option<&Value>) -> AnthropicUsage {
    let input_tokens = value
        .and_then(|value| value.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = value
        .and_then(|value| value.get("completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    AnthropicUsage {
        input_tokens,
        output_tokens,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: value
            .and_then(|value| value.pointer("/prompt_tokens_details/cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}

fn map_chat_finish_reason(reason: &str) -> String {
    match reason {
        "stop" => "end_turn",
        "length" => "max_tokens",
        "tool_calls" | "function_call" => "tool_use",
        "content_filter" => "stop_sequence",
        other => other,
    }
    .to_string()
}

fn parse_tool_arguments(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| json!({}))
}

fn insert_header<K>(headers: &mut reqwest::header::HeaderMap, name: K, value: &str) -> Result<()>
where
    K: reqwest::header::IntoHeaderName,
{
    let value = reqwest::header::HeaderValue::from_str(value).map_err(|err| {
        ProxyError::Config(format!(
            "invalid header value for custom OpenAI request: {err}"
        ))
    })?;
    headers.insert(name, value);
    Ok(())
}

pub fn responses_url(base_url: &str) -> Result<String> {
    endpoint_url(base_url, "responses")
}

pub fn chat_completions_url(base_url: &str) -> Result<String> {
    endpoint_url(base_url, "chat/completions")
}

fn endpoint_url(base_url: &str, endpoint: &str) -> Result<String> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(ProxyError::Config(
            "custom OpenAI base URL is required; set --custom-openai-base-url or CCP_CUSTOM_OPENAI_BASE_URL".into(),
        ));
    }
    if endpoint == "responses" && base.ends_with("/responses") {
        return Ok(base.to_string());
    }
    if endpoint == "chat/completions" && base.ends_with("/chat/completions") {
        return Ok(base.to_string());
    }
    if base.ends_with("/v1") {
        return Ok(format!("{base}/{endpoint}"));
    }
    Ok(format!("{base}/v1/{endpoint}"))
}

fn truncate_for_log(value: &str, max_chars: usize) -> String {
    let mut out = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        out.push_str("...[truncated]");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Provider;

    fn anthropic_request() -> AnthropicRequest {
        AnthropicRequest {
            model: "gpt-5.4".into(),
            max_tokens: Some(100),
            temperature: Some(0.2),
            top_p: Some(0.9),
            stream: Some(false),
            system: Some(json!("be concise")),
            messages: vec![crate::anthropic::schema::AnthropicMessage {
                role: "user".into(),
                content: json!("hello"),
                extra: Default::default(),
            }],
            tools: Some(vec![AnthropicTool {
                name: "lookup".into(),
                description: Some("Lookup a value".into()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {"key": {"type": "string"}},
                    "required": ["key"]
                })),
                extra: Default::default(),
            }]),
            tool_choice: Some(json!({"type": "tool", "name": "lookup"})),
            metadata: None,
            output_config: None,
            thinking: None,
            extra: Default::default(),
        }
    }

    fn resolved() -> ResolvedModel {
        ResolvedModel {
            provider: Provider::CustomOpenAI,
            requested: "gpt-5.4".into(),
            public_id: "gpt-5.4".into(),
            upstream_model: "gpt-5.4".into(),
            service_tier: None,
            context_window: 272_000,
        }
    }

    #[test]
    fn endpoint_url_accepts_base_or_full_endpoint() {
        assert_eq!(
            responses_url("http://localhost:8000").unwrap(),
            "http://localhost:8000/v1/responses"
        );
        assert_eq!(
            responses_url("http://localhost:8000/v1").unwrap(),
            "http://localhost:8000/v1/responses"
        );
        assert_eq!(
            responses_url("http://localhost:8000/custom/responses").unwrap(),
            "http://localhost:8000/custom/responses"
        );
        assert_eq!(
            chat_completions_url("http://localhost:8000").unwrap(),
            "http://localhost:8000/v1/chat/completions"
        );
    }

    #[test]
    fn translates_chat_request_with_tools() {
        let translated = translate_chat_request(&anthropic_request(), &resolved()).unwrap();
        assert_eq!(translated.model, "gpt-5.4");
        assert_eq!(translated.messages[0]["role"], "system");
        assert_eq!(translated.messages[1]["role"], "user");
        assert_eq!(translated.tools.as_ref().unwrap()[0]["type"], "function");
        assert_eq!(
            translated.tool_choice.as_ref().unwrap(),
            &json!({"type": "function", "function": {"name": "lookup"}})
        );
    }

    #[test]
    fn chat_tool_translation_canonicalizes_and_omits_empty_descriptions() {
        let mut request = anthropic_request();
        let duplicate = request.tools.as_ref().unwrap()[0].clone();
        request.tools = Some(vec![
            AnthropicTool {
                name: "Write".into(),
                description: None,
                input_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "content": {"type": "string"},
                        "path": {"type": "string"}
                    },
                    "required": ["path", "content"]
                })),
                extra: Default::default(),
            },
            duplicate.clone(),
            duplicate,
        ]);

        let translated = translate_chat_request(&request, &resolved()).unwrap();
        let tools = translated.tools.as_ref().unwrap();

        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["function"]["name"], "Write");
        assert_eq!(
            tools[0]["function"]["parameters"]["required"],
            json!(["content", "path"])
        );
        assert!(tools[0]["function"].get("description").is_none());
        assert_eq!(tools[1]["function"]["name"], "lookup");
    }

    #[test]
    fn chat_response_maps_text_and_usage() {
        let response = chat_response_from_value(
            json!({
                "choices": [{
                    "message": {"role": "assistant", "content": "hello"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 3, "completion_tokens": 4}
            }),
            "gpt-5.4".into(),
        )
        .unwrap();
        assert_eq!(response.content[0].text.as_deref(), Some("hello"));
        assert_eq!(response.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(response.usage.input_tokens, 3);
        assert_eq!(response.usage.output_tokens, 4);
    }

    #[test]
    fn stores_custom_api_key_with_private_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("custom-openai-api-key");
        store_api_key(&path, "secret").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap().trim(), "secret");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}
