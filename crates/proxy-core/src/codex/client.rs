use crate::{
    auth::AuthManager,
    codex::translate::ResponsesRequest,
    config::{CodexConfig, CodexTransport},
    error::{ProxyError, Result},
};
use async_stream::try_stream;
use bytes::Bytes;
use futures_util::{SinkExt, Stream, StreamExt};
use http::StatusCode;
use serde_json::{json, Value};
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest, handshake::client::Request as WebSocketRequest,
        ClientRequestBuilder, Message,
    },
    WebSocketStream,
};
use tracing::{debug, info, warn};

pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>;

const WEBSOCKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const WEBSOCKET_FIRST_EVENT_TIMEOUT: Duration = Duration::from_secs(5);
const WEBSOCKET_FAILURE_COOLDOWN: Duration = Duration::from_secs(120);
const WEBSOCKET_READ_IDLE_WARN_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodexTransportMethod {
    HttpSse,
    WebSocket,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CodexTransportStatus {
    pub current_method: Option<CodexTransportMethod>,
    pub websocket_cooldown_remaining: Option<Duration>,
}

pub struct CodexResponse {
    pub body: ByteStream,
    pub status: StatusCode,
}

#[derive(Clone)]
pub struct CodexClient {
    http: reqwest::Client,
    config: CodexConfig,
    auth: AuthManager,
    transport_state: Arc<TransportState>,
}

impl CodexClient {
    pub fn new(config: CodexConfig, auth: AuthManager) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()?;
        Ok(Self {
            http,
            config,
            auth,
            transport_state: Arc::new(TransportState::default()),
        })
    }

    pub fn transport_status(&self) -> CodexTransportStatus {
        CodexTransportStatus {
            current_method: self.transport_state.current_method(),
            websocket_cooldown_remaining: self.transport_state.websocket_cooldown_remaining(),
        }
    }

    pub async fn post(
        &self,
        body: &ResponsesRequest,
        session_id: Option<&str>,
    ) -> Result<CodexResponse> {
        let auth = self.auth.get_auth().await?;
        let mut response = self
            .post_with_access(body, session_id, &auth.access, auth.account_id.as_deref())
            .await;
        if matches!(&response, Err(ProxyError::Upstream { status, .. }) if *status == StatusCode::UNAUTHORIZED)
        {
            warn!("codex returned 401; forcing token refresh");
            let auth = self.auth.force_refresh().await?;
            response = self
                .post_with_access(body, session_id, &auth.access, auth.account_id.as_deref())
                .await;
        }
        response
    }

    async fn post_with_access(
        &self,
        body: &ResponsesRequest,
        session_id: Option<&str>,
        access_token: &str,
        account_id: Option<&str>,
    ) -> Result<CodexResponse> {
        match self.config.transport {
            CodexTransport::Http => {
                self.post_http(body, session_id, access_token, account_id)
                    .await
            }
            CodexTransport::WebSocket => {
                self.post_websocket(body, session_id, access_token, account_id)
                    .await
            }
            CodexTransport::Auto => {
                if let Some(remaining) = self.transport_state.websocket_cooldown_remaining() {
                    debug!(
                        cooldown_ms = remaining.as_millis(),
                        "skipping Codex websocket during fallback cooldown"
                    );
                    return self
                        .post_http(body, session_id, access_token, account_id)
                        .await;
                }

                match self
                    .post_websocket(body, session_id, access_token, account_id)
                    .await
                {
                    Ok(response) => {
                        self.transport_state.record_websocket_success();
                        Ok(response)
                    }
                    Err(err) => {
                        self.transport_state
                            .record_websocket_failure(WEBSOCKET_FAILURE_COOLDOWN);
                        warn!(
                            error = %err,
                            cooldown_ms = WEBSOCKET_FAILURE_COOLDOWN.as_millis(),
                            "codex websocket setup failed; falling back to HTTP SSE"
                        );
                        self.post_http(body, session_id, access_token, account_id)
                            .await
                    }
                }
            }
        }
    }

    async fn post_http(
        &self,
        body: &ResponsesRequest,
        session_id: Option<&str>,
        access_token: &str,
        account_id: Option<&str>,
    ) -> Result<CodexResponse> {
        info!(
            model = %body.model,
            input_items = body.input.len(),
            tool_count = body.tools.as_ref().map_or(0, Vec::len),
            "posting Codex HTTP request"
        );
        let request = self
            .http
            .post(&self.config.base_url)
            .headers(self.headers(access_token, account_id, session_id)?)
            .json(body);
        let response = tokio::time::timeout(
            Duration::from_millis(self.config.header_timeout_ms),
            request.send(),
        )
        .await
        .map_err(|_| {
            ProxyError::Transport("timed out waiting for Codex response headers".into())
        })??;
        let status =
            StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        info!(%status, "received Codex HTTP response headers");
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
                "Codex HTTP request failed"
            );
            return Err(ProxyError::Upstream {
                status,
                body,
                retry_after,
            });
        }
        self.transport_state
            .record_method(CodexTransportMethod::HttpSse);
        let stream = response
            .bytes_stream()
            .map(|item| item.map_err(ProxyError::from));
        Ok(CodexResponse {
            body: Box::pin(stream),
            status,
        })
    }

    async fn post_websocket(
        &self,
        body: &ResponsesRequest,
        session_id: Option<&str>,
        access_token: &str,
        account_id: Option<&str>,
    ) -> Result<CodexResponse> {
        let ws_url = websocket_url(&self.config.base_url)?;
        let request =
            build_websocket_request(&ws_url, &self.config, access_token, account_id, session_id)?;
        let (mut socket, _) =
            tokio::time::timeout(WEBSOCKET_CONNECT_TIMEOUT, connect_async(request))
                .await
                .map_err(|_| ProxyError::Transport("timed out opening Codex websocket".into()))?
                .map_err(|err| {
                    ProxyError::Transport(format!("Codex websocket setup failed: {err}"))
                })?;
        info!(model = %body.model, input_items = body.input.len(), "Codex websocket connected");
        let payload = websocket_create_payload(body);
        socket
            .send(Message::Text(payload.into()))
            .await
            .map_err(|err| ProxyError::Transport(format!("Codex websocket send failed: {err}")))?;

        let first = read_first_websocket_event(&mut socket).await?;
        let session_id_for_log = session_id.map(str::to_owned);
        log_websocket_frame(&first, 0, session_id_for_log.as_deref());
        reject_websocket_error(&first)?;
        self.transport_state
            .record_method(CodexTransportMethod::WebSocket);
        let stream = try_stream! {
            let mut frame_index = 0_u64;
            if let Some(bytes) = websocket_message_to_bytes(first) {
                yield bytes;
            }
            let mut frame_count = 1_u64;
            loop {
                match tokio::time::timeout(WEBSOCKET_READ_IDLE_WARN_INTERVAL, socket.next()).await {
                    Ok(Some(Ok(message))) => {
                            frame_index += 1;
                            frame_count += 1;
                            log_websocket_frame(&message, frame_index, session_id_for_log.as_deref());
                            if let Some(bytes) = websocket_message_to_bytes(message) {
                                yield bytes;
                            }
                    }
                    Ok(Some(Err(err))) => Err(ProxyError::Transport(format!(
                            "Codex websocket read failed: {err}"
                        )))?,
                    Ok(None) => {
                        info!(
                            session_id = session_id_for_log.as_deref().unwrap_or("none"),
                            frame_count,
                            "Codex websocket stream ended"
                        );
                        break;
                    }
                    Err(_) => {
                        warn!(
                            session_id = session_id_for_log.as_deref().unwrap_or("none"),
                            idle_ms = WEBSOCKET_READ_IDLE_WARN_INTERVAL.as_millis(),
                            frame_count,
                            "Codex websocket stream idle while waiting for next frame"
                        );
                    }
                }
            }
        };
        Ok(CodexResponse {
            body: Box::pin(stream),
            status: StatusCode::OK,
        })
    }

    fn headers(
        &self,
        access_token: &str,
        account_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<reqwest::header::HeaderMap> {
        let mut headers = reqwest::header::HeaderMap::new();
        insert_static(
            &mut headers,
            reqwest::header::CONTENT_TYPE,
            "application/json",
        )?;
        insert_static(&mut headers, reqwest::header::ACCEPT, "text/event-stream")?;
        insert_static(
            &mut headers,
            reqwest::header::AUTHORIZATION,
            &format!("Bearer {access_token}"),
        )?;
        insert_static(&mut headers, "openai-beta", "responses=experimental")?;
        insert_static(&mut headers, "originator", &self.config.originator)?;
        insert_static(
            &mut headers,
            reqwest::header::USER_AGENT,
            &self.config.user_agent,
        )?;
        if let Some(account_id) = account_id {
            insert_static(&mut headers, "ChatGPT-Account-Id", account_id)?;
        }
        if let Some(session_id) = session_id {
            insert_static(&mut headers, "session_id", session_id)?;
            insert_static(&mut headers, "x-client-request-id", session_id)?;
            insert_static(
                &mut headers,
                "x-codex-window-id",
                &format!("{session_id}:0"),
            )?;
        }
        Ok(headers)
    }
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

#[derive(Debug, Default)]
struct TransportState {
    websocket_disabled_until: Mutex<Option<Instant>>,
    current_method: Mutex<Option<CodexTransportMethod>>,
}

impl TransportState {
    fn websocket_cooldown_remaining(&self) -> Option<Duration> {
        let mut disabled_until = self
            .websocket_disabled_until
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let until = (*disabled_until)?;
        let now = Instant::now();
        if until > now {
            Some(until.saturating_duration_since(now))
        } else {
            *disabled_until = None;
            None
        }
    }

    fn record_websocket_failure(&self, cooldown: Duration) {
        let mut disabled_until = self
            .websocket_disabled_until
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *disabled_until = Some(Instant::now() + cooldown);
    }

    fn record_websocket_success(&self) {
        let mut disabled_until = self
            .websocket_disabled_until
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *disabled_until = None;
    }

    fn current_method(&self) -> Option<CodexTransportMethod> {
        *self
            .current_method
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn record_method(&self, method: CodexTransportMethod) {
        let mut current_method = self
            .current_method
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *current_method = Some(method);
    }
}

fn insert_static<K>(headers: &mut reqwest::header::HeaderMap, name: K, value: &str) -> Result<()>
where
    K: reqwest::header::IntoHeaderName,
{
    let value = reqwest::header::HeaderValue::from_str(value).map_err(|err| {
        ProxyError::Config(format!("invalid header value for upstream request: {err}"))
    })?;
    headers.insert(name, value);
    Ok(())
}

fn websocket_url(url: &str) -> Result<String> {
    if let Some(rest) = url.strip_prefix("https://") {
        Ok(format!("wss://{rest}"))
    } else if let Some(rest) = url.strip_prefix("http://") {
        Ok(format!("ws://{rest}"))
    } else {
        Err(ProxyError::Config(format!(
            "Codex base URL must be http(s): {url}"
        )))
    }
}

fn build_websocket_request(
    ws_url: &str,
    config: &CodexConfig,
    access_token: &str,
    account_id: Option<&str>,
    session_id: Option<&str>,
) -> Result<WebSocketRequest> {
    let uri = ws_url
        .parse::<http::Uri>()
        .map_err(|err| ProxyError::Transport(format!("bad websocket URL: {err}")))?;
    let mut request = ClientRequestBuilder::new(uri)
        .with_header("authorization", format!("Bearer {access_token}"))
        .with_header("openai-beta", "responses=experimental")
        .with_header("originator", config.originator.as_str())
        .with_header("user-agent", config.user_agent.as_str());
    if let Some(account_id) = account_id {
        request = request.with_header("ChatGPT-Account-Id", account_id);
    }
    if let Some(session_id) = session_id {
        request = request
            .with_header("session_id", session_id)
            .with_header("x-client-request-id", session_id)
            .with_header("x-codex-window-id", format!("{session_id}:0"));
    }
    request
        .into_client_request()
        .map_err(|err| ProxyError::Transport(format!("bad websocket request: {err}")))
}

fn websocket_create_payload(body: &ResponsesRequest) -> String {
    let mut value = serde_json::to_value(body).unwrap_or_else(|_| json!({}));
    if let Some(object) = value.as_object_mut() {
        object.insert("type".into(), json!("response.create"));
        object.remove("stream");
    }
    value.to_string()
}

async fn read_first_websocket_event<S>(socket: &mut WebSocketStream<S>) -> Result<Message>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    tokio::time::timeout(WEBSOCKET_FIRST_EVENT_TIMEOUT, async {
        loop {
            match socket.next().await {
                Some(Ok(message @ Message::Text(_))) | Some(Ok(message @ Message::Binary(_))) => {
                    return Ok(message);
                }
                Some(Ok(Message::Close(frame))) => {
                    let detail = frame
                        .map(|frame| format!(": {} {}", frame.code, frame.reason))
                        .unwrap_or_default();
                    return Err(ProxyError::Transport(format!(
                        "Codex websocket closed before first event{detail}"
                    )));
                }
                Some(Ok(_)) => {}
                Some(Err(err)) => {
                    return Err(ProxyError::Transport(format!(
                        "Codex websocket read failed before first event: {err}"
                    )));
                }
                None => {
                    return Err(ProxyError::Transport(
                        "Codex websocket ended before first event".into(),
                    ));
                }
            }
        }
    })
    .await
    .map_err(|_| {
        ProxyError::Transport("timed out waiting for first Codex websocket event".into())
    })?
}

fn websocket_message_to_bytes(message: Message) -> Option<Bytes> {
    match message {
        Message::Text(text) => Some(sse_data_event(text.as_str())),
        Message::Binary(bytes) => match String::from_utf8(bytes.to_vec()) {
            Ok(text) => Some(sse_data_event(&text)),
            Err(_) => Some(Bytes::copy_from_slice(bytes.as_ref())),
        },
        Message::Close(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => None,
    }
}

fn reject_websocket_error(message: &Message) -> Result<()> {
    let Some(value) = websocket_json_value(message) else {
        return Ok(());
    };
    let has_error_type = value.get("type").and_then(Value::as_str) == Some("error");
    let has_error_status = value
        .get("status")
        .and_then(Value::as_u64)
        .is_some_and(|status| status >= 400);
    if !has_error_type && !has_error_status {
        return Ok(());
    }

    let status = value
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
        .and_then(|status| StatusCode::from_u16(status).ok())
        .unwrap_or(StatusCode::BAD_GATEWAY);
    let error_type = value
        .pointer("/error/type")
        .or_else(|| value.pointer("/error/code"))
        .and_then(Value::as_str)
        .unwrap_or("upstream_error");
    let message = value
        .pointer("/error/message")
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("Codex websocket returned an error event");

    Err(ProxyError::Upstream {
        status,
        body: format!("{error_type}: {message}"),
        retry_after: None,
    })
}

fn websocket_json_value(message: &Message) -> Option<Value> {
    match message {
        Message::Text(text) => serde_json::from_str(text).ok(),
        Message::Binary(bytes) => std::str::from_utf8(bytes)
            .ok()
            .and_then(|text| serde_json::from_str(text).ok()),
        Message::Close(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => None,
    }
}

fn log_websocket_frame(message: &Message, frame_index: u64, session_id: Option<&str>) {
    let level_is_info = frame_index == 0 || matches!(message, Message::Close(_));
    match message {
        Message::Text(text) => {
            let text = text.as_str();
            let newline_count = text.matches('\n').count();
            if level_is_info {
                info!(
                    session_id = session_id.unwrap_or("none"),
                    frame_index,
                    frame_kind = "text",
                    byte_len = text.len(),
                    newline_count,
                    line_count = sse_line_count(text),
                    preview = %truncate_for_log_escaped(text, 240),
                    "received Codex websocket frame"
                );
            } else {
                debug!(
                    session_id = session_id.unwrap_or("none"),
                    frame_index,
                    frame_kind = "text",
                    byte_len = text.len(),
                    newline_count,
                    line_count = sse_line_count(text),
                    preview = %truncate_for_log_escaped(text, 240),
                    "received Codex websocket frame"
                );
            }
        }
        Message::Binary(bytes) => {
            if level_is_info {
                info!(
                    session_id = session_id.unwrap_or("none"),
                    frame_index,
                    frame_kind = "binary",
                    byte_len = bytes.len(),
                    utf8 = std::str::from_utf8(bytes).is_ok(),
                    "received Codex websocket frame"
                );
            } else {
                debug!(
                    session_id = session_id.unwrap_or("none"),
                    frame_index,
                    frame_kind = "binary",
                    byte_len = bytes.len(),
                    utf8 = std::str::from_utf8(bytes).is_ok(),
                    "received Codex websocket frame"
                );
            }
        }
        Message::Close(frame) => {
            info!(
                session_id = session_id.unwrap_or("none"),
                frame_index,
                frame_kind = "close",
                code = frame.as_ref().map(|frame| frame.code.to_string()),
                reason = frame.as_ref().map(|frame| frame.reason.as_ref()),
                "received Codex websocket frame"
            );
        }
        Message::Ping(bytes) | Message::Pong(bytes) => {
            debug!(
                session_id = session_id.unwrap_or("none"),
                frame_index,
                frame_kind = if matches!(message, Message::Ping(_)) {
                    "ping"
                } else {
                    "pong"
                },
                byte_len = bytes.len(),
                "received Codex websocket frame"
            );
        }
        Message::Frame(_) => {
            debug!(
                session_id = session_id.unwrap_or("none"),
                frame_index,
                frame_kind = "raw",
                "received Codex websocket frame"
            );
        }
    }
}

fn sse_data_event(text: &str) -> Bytes {
    let mut out = String::new();
    for line in text.lines() {
        out.push_str("data: ");
        out.push_str(line);
        out.push('\n');
    }
    if text.is_empty() {
        out.push_str("data: \n");
    }
    out.push('\n');
    Bytes::from(out)
}

fn sse_line_count(text: &str) -> usize {
    if text.is_empty() {
        1
    } else {
        text.lines().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn websocket_cooldown_tracks_failures_and_successes() {
        let state = TransportState::default();
        assert!(state.websocket_cooldown_remaining().is_none());

        state.record_websocket_failure(Duration::from_secs(60));
        assert!(state.websocket_cooldown_remaining().is_some());

        state.record_websocket_success();
        assert!(state.websocket_cooldown_remaining().is_none());
    }

    #[test]
    fn transport_state_tracks_current_method() {
        let state = TransportState::default();
        assert_eq!(state.current_method(), None);

        state.record_method(CodexTransportMethod::HttpSse);
        assert_eq!(state.current_method(), Some(CodexTransportMethod::HttpSse));

        state.record_method(CodexTransportMethod::WebSocket);
        assert_eq!(
            state.current_method(),
            Some(CodexTransportMethod::WebSocket)
        );
    }

    #[test]
    fn websocket_cooldown_expires() {
        let state = TransportState::default();
        state.record_websocket_failure(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(5));
        assert!(state.websocket_cooldown_remaining().is_none());
    }

    #[test]
    fn websocket_request_includes_handshake_and_codex_headers() {
        let config = CodexConfig::default();
        let request = build_websocket_request(
            "wss://example.test/backend-api/codex/responses",
            &config,
            "access-token",
            Some("account-id"),
            Some("session-id"),
        )
        .unwrap();
        let headers = request.headers();

        assert!(headers.contains_key("Sec-WebSocket-Key"));
        assert_eq!(headers.get("Connection").unwrap(), "Upgrade");
        assert_eq!(headers.get("Upgrade").unwrap(), "websocket");
        assert_eq!(headers.get("authorization").unwrap(), "Bearer access-token");
        assert_eq!(headers.get("ChatGPT-Account-Id").unwrap(), "account-id");
        assert_eq!(headers.get("x-client-request-id").unwrap(), "session-id");
    }

    #[test]
    fn websocket_create_payload_uses_top_level_response_create_event() {
        let payload = websocket_create_payload(&minimal_response_request());
        let value = serde_json::from_str::<Value>(&payload).unwrap();

        assert_eq!(value["type"], "response.create");
        assert_eq!(value["model"], "gpt-5.5");
        assert_eq!(value["store"], false);
        assert_eq!(value["input"][0]["role"], "user");
        assert!(value.get("request").is_none());
        assert!(value.get("stream").is_none());
    }

    #[test]
    fn websocket_text_frame_with_pretty_json_is_valid_sse_data() {
        let bytes = websocket_message_to_bytes(Message::Text(
            "{\n  \"type\": \"response.completed\"\n}".into(),
        ))
        .unwrap();

        assert_eq!(
            bytes,
            Bytes::from_static(b"data: {\ndata:   \"type\": \"response.completed\"\ndata: }\n\n")
        );
    }

    #[test]
    fn websocket_error_frame_is_rejected_before_stream_commit() {
        let err = reject_websocket_error(&Message::Text(
            json!({
                "type": "error",
                "status": 400,
                "error": {
                    "type": "invalid_request_error",
                    "message": "Expected a 'response.create' message as the first websocket event."
                }
            })
            .to_string()
            .into(),
        ))
        .unwrap_err();

        match err {
            ProxyError::Upstream { status, body, .. } => {
                assert_eq!(status, StatusCode::BAD_REQUEST);
                assert!(body.contains("invalid_request_error"));
                assert!(body.contains("response.create"));
            }
            other => panic!("expected upstream error, got {other:?}"),
        }
    }

    fn minimal_response_request() -> ResponsesRequest {
        ResponsesRequest {
            model: "gpt-5.5".into(),
            input: vec![json!({
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "hi"}],
            })],
            store: false,
            instructions: None,
            tools: None,
            tool_choice: None,
            reasoning: None,
            include: None,
            text: None,
            stream: true,
        }
    }
}
