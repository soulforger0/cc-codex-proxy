use crate::{
    auth::AuthManager,
    codex::translate::ResponsesRequest,
    config::{
        codex_compat_version, compatible_openai_user_agent, CodexConfig, CodexTransport,
        CustomOpenAIConfig, DEFAULT_ORIGINATOR,
    },
    custom_openai::{resolve_api_key, responses_url},
    error::{ProxyError, Result},
    http_client::{
        build_client, duration_from_millis, monitor_idle_stream, optional_duration_from_millis,
        HttpClientTuning,
    },
};
use async_stream::try_stream;
use bytes::Bytes;
use futures_util::{SinkExt, Stream, StreamExt};
use http::StatusCode;
use serde_json::{json, Value};
use std::{
    path::PathBuf,
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

pub struct OpenAIResponsesResponse {
    pub body: ByteStream,
    pub status: StatusCode,
}

#[derive(Clone)]
pub struct OpenAIResponsesClient {
    http: reqwest::Client,
    config: OpenAIResponsesConfig,
    credentials: CredentialSource,
    transport_state: Arc<TransportState>,
}

#[derive(Clone)]
enum CredentialSource {
    ChatGpt(AuthManager),
    CustomBearer(PathBuf),
}

#[derive(Clone)]
struct OpenAIResponsesConfig {
    base_url: String,
    originator: String,
    user_agent: String,
    compat_version: String,
    transport: CodexTransport,
    header_timeout_ms: u64,
    connect_timeout_ms: u64,
    pool_idle_timeout_ms: u64,
    pool_max_idle_per_host: usize,
    tcp_keepalive_ms: u64,
    stream_idle_warn_ms: u64,
    stream_idle_timeout_ms: u64,
    provider_name: &'static str,
}

impl From<CodexConfig> for OpenAIResponsesConfig {
    fn from(config: CodexConfig) -> Self {
        Self {
            base_url: config.base_url,
            originator: DEFAULT_ORIGINATOR.into(),
            user_agent: compatible_openai_user_agent(),
            compat_version: codex_compat_version(),
            transport: config.transport,
            header_timeout_ms: config.header_timeout_ms,
            connect_timeout_ms: config.connect_timeout_ms,
            pool_idle_timeout_ms: config.pool_idle_timeout_ms,
            pool_max_idle_per_host: config.pool_max_idle_per_host,
            tcp_keepalive_ms: config.tcp_keepalive_ms,
            stream_idle_warn_ms: config.stream_idle_warn_ms,
            stream_idle_timeout_ms: config.stream_idle_timeout_ms,
            provider_name: "Codex",
        }
    }
}

impl OpenAIResponsesClient {
    pub fn new_codex(config: CodexConfig, auth: AuthManager) -> Result<Self> {
        Self::new(config.into(), CredentialSource::ChatGpt(auth))
    }

    pub fn new_custom(config: CustomOpenAIConfig, api_key_file: PathBuf) -> Result<Self> {
        let base_url = if config.base_url.trim().is_empty() {
            String::new()
        } else {
            responses_url(&config.base_url)?
        };
        Self::new(
            OpenAIResponsesConfig {
                base_url,
                originator: DEFAULT_ORIGINATOR.into(),
                user_agent: compatible_openai_user_agent(),
                compat_version: codex_compat_version(),
                transport: config.transport,
                header_timeout_ms: config.header_timeout_ms,
                connect_timeout_ms: config.connect_timeout_ms,
                pool_idle_timeout_ms: config.pool_idle_timeout_ms,
                pool_max_idle_per_host: config.pool_max_idle_per_host,
                tcp_keepalive_ms: config.tcp_keepalive_ms,
                stream_idle_warn_ms: config.stream_idle_warn_ms,
                stream_idle_timeout_ms: config.stream_idle_timeout_ms,
                provider_name: "custom OpenAI",
            },
            CredentialSource::CustomBearer(api_key_file),
        )
    }

    fn new(config: OpenAIResponsesConfig, credentials: CredentialSource) -> Result<Self> {
        let http = build_client(HttpClientTuning {
            connect_timeout_ms: config.connect_timeout_ms,
            pool_idle_timeout_ms: config.pool_idle_timeout_ms,
            pool_max_idle_per_host: config.pool_max_idle_per_host,
            tcp_keepalive_ms: config.tcp_keepalive_ms,
        })?;
        Ok(Self {
            http,
            config,
            credentials,
            transport_state: Arc::new(TransportState::default()),
        })
    }

    pub fn transport_status(&self) -> CodexTransportStatus {
        CodexTransportStatus {
            current_method: self.transport_state.current_method(),
            websocket_cooldown_remaining: self.transport_state.websocket_cooldown_remaining(),
        }
    }

    pub fn base_url_configured(&self) -> bool {
        !self.config.base_url.trim().is_empty()
    }

    pub async fn post(
        &self,
        body: &ResponsesRequest,
        session_id: Option<&str>,
    ) -> Result<OpenAIResponsesResponse> {
        if self.config.base_url.trim().is_empty() {
            return Err(ProxyError::Config(
                "custom OpenAI base URL is required; set --custom-openai-base-url or CCP_CUSTOM_OPENAI_BASE_URL".into(),
            ));
        }
        match &self.credentials {
            CredentialSource::ChatGpt(auth_manager) => {
                let auth = auth_manager.get_auth().await?;
                let mut response = self
                    .post_with_access(
                        body,
                        session_id,
                        Some(&auth.access),
                        auth.account_id.as_deref(),
                    )
                    .await;
                if matches!(&response, Err(ProxyError::Upstream { status, .. }) if *status == StatusCode::UNAUTHORIZED)
                {
                    warn!("Codex returned 401; forcing token refresh");
                    let auth = auth_manager.force_refresh().await?;
                    response = self
                        .post_with_access(
                            body,
                            session_id,
                            Some(&auth.access),
                            auth.account_id.as_deref(),
                        )
                        .await;
                }
                response
            }
            CredentialSource::CustomBearer(path) => {
                let token = resolve_api_key(path)?;
                self.post_with_access(body, session_id, token.as_deref(), None)
                    .await
            }
        }
    }

    async fn post_with_access(
        &self,
        body: &ResponsesRequest,
        session_id: Option<&str>,
        access_token: Option<&str>,
        account_id: Option<&str>,
    ) -> Result<OpenAIResponsesResponse> {
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
                        provider = self.config.provider_name,
                        "skipping OpenAI Responses websocket during fallback cooldown"
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
                            provider = self.config.provider_name,
                            "OpenAI Responses websocket setup failed; falling back to HTTP SSE"
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
        access_token: Option<&str>,
        account_id: Option<&str>,
    ) -> Result<OpenAIResponsesResponse> {
        info!(
            model = %body.model,
            input_items = body.input.len(),
            tool_count = body.tools.as_ref().map_or(0, Vec::len),
            provider = self.config.provider_name,
            "posting OpenAI Responses HTTP request"
        );
        let request = self
            .http
            .post(&self.config.base_url)
            .headers(self.headers(
                access_token,
                account_id,
                session_id,
                is_responses_lite_model(&body.model),
            )?)
            .json(body);
        let response = tokio::time::timeout(
            Duration::from_millis(self.config.header_timeout_ms),
            request.send(),
        )
        .await
        .map_err(|_| {
            ProxyError::Transport(format!(
                "timed out waiting for {} response headers",
                self.config.provider_name
            ))
        })??;
        let status =
            StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        info!(%status, provider = self.config.provider_name, "received OpenAI Responses HTTP headers");
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
                provider = self.config.provider_name,
                "OpenAI Responses HTTP request failed"
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
        let stream = monitor_idle_stream(
            stream,
            format!("{} HTTP", self.config.provider_name),
            session_id.map(ToOwned::to_owned),
            duration_from_millis(self.config.stream_idle_warn_ms),
            optional_duration_from_millis(self.config.stream_idle_timeout_ms),
        );
        Ok(OpenAIResponsesResponse {
            body: stream,
            status,
        })
    }

    async fn post_websocket(
        &self,
        body: &ResponsesRequest,
        session_id: Option<&str>,
        access_token: Option<&str>,
        account_id: Option<&str>,
    ) -> Result<OpenAIResponsesResponse> {
        let ws_url = websocket_url(&self.config.base_url)?;
        let request =
            build_websocket_request(&ws_url, &self.config, access_token, account_id, session_id)?;
        let (mut socket, _) =
            tokio::time::timeout(WEBSOCKET_CONNECT_TIMEOUT, connect_async(request))
                .await
                .map_err(|_| {
                    ProxyError::Transport(format!(
                        "timed out opening {} websocket",
                        self.config.provider_name
                    ))
                })?
                .map_err(|err| {
                    ProxyError::Transport(format!(
                        "{} websocket setup failed: {err}",
                        self.config.provider_name
                    ))
                })?;
        info!(model = %body.model, input_items = body.input.len(), provider = self.config.provider_name, "OpenAI Responses websocket connected");
        let payload = websocket_create_payload(body);
        socket.send(Message::Text(payload)).await.map_err(|err| {
            ProxyError::Transport(format!(
                "{} websocket send failed: {err}",
                self.config.provider_name
            ))
        })?;

        let initial = read_initial_websocket_events(&mut socket).await?;
        let provider_name = self.config.provider_name;
        let session_id_for_log = session_id.map(str::to_owned);
        for (index, message) in initial.iter().enumerate() {
            log_websocket_frame(message, index as u64, session_id_for_log.as_deref());
            reject_websocket_error(message)?;
        }
        self.transport_state
            .record_method(CodexTransportMethod::WebSocket);
        let stream = try_stream! {
            let mut frame_index = initial.len().saturating_sub(1) as u64;
            let mut initial_is_terminal = false;
            for message in initial {
                initial_is_terminal |= websocket_message_is_terminal(&message);
                if let Some(bytes) = websocket_message_to_bytes(message) {
                    yield bytes;
                }
            }
            let mut frame_count = frame_index + 1;
            if initial_is_terminal {
                info!(
                    session_id = session_id_for_log.as_deref().unwrap_or("none"),
                    frame_count,
                    "OpenAI Responses websocket response completed"
                );
                return;
            }
            loop {
                match tokio::time::timeout(WEBSOCKET_READ_IDLE_WARN_INTERVAL, socket.next()).await {
                    Ok(Some(Ok(message))) => {
                        frame_index += 1;
                        frame_count += 1;
                        log_websocket_frame(&message, frame_index, session_id_for_log.as_deref());
                        reject_websocket_error(&message)?;
                        let is_terminal = websocket_message_is_terminal(&message);
                        if let Some(bytes) = websocket_message_to_bytes(message) {
                            yield bytes;
                        }
                        if is_terminal {
                            info!(
                                session_id = session_id_for_log.as_deref().unwrap_or("none"),
                                frame_count,
                                provider = provider_name,
                                "OpenAI Responses websocket response completed"
                            );
                            break;
                        }
                    }
                    Ok(Some(Err(err))) => Err(ProxyError::Transport(format!(
                            "{provider_name} websocket read failed: {err}"
                        )))?,
                    Ok(None) => {
                        info!(
                            session_id = session_id_for_log.as_deref().unwrap_or("none"),
                            frame_count,
                            provider = provider_name,
                            "OpenAI Responses websocket stream ended"
                        );
                        break;
                    }
                    Err(_) => {
                        warn!(
                            session_id = session_id_for_log.as_deref().unwrap_or("none"),
                            idle_ms = WEBSOCKET_READ_IDLE_WARN_INTERVAL.as_millis(),
                            frame_count,
                            provider = provider_name,
                            "OpenAI Responses websocket stream idle while waiting for next frame"
                        );
                    }
                }
            }
        };
        Ok(OpenAIResponsesResponse {
            body: Box::pin(stream),
            status: StatusCode::OK,
        })
    }

    fn headers(
        &self,
        access_token: Option<&str>,
        account_id: Option<&str>,
        session_id: Option<&str>,
        responses_lite: bool,
    ) -> Result<reqwest::header::HeaderMap> {
        let mut headers = reqwest::header::HeaderMap::new();
        insert_static(
            &mut headers,
            reqwest::header::CONTENT_TYPE,
            "application/json",
        )?;
        insert_static(&mut headers, reqwest::header::ACCEPT, "text/event-stream")?;
        if let Some(access_token) = access_token {
            insert_static(
                &mut headers,
                reqwest::header::AUTHORIZATION,
                &format!("Bearer {access_token}"),
            )?;
        }
        insert_static(&mut headers, "originator", &self.config.originator)?;
        insert_static(&mut headers, "version", &self.config.compat_version)?;
        insert_static(
            &mut headers,
            reqwest::header::USER_AGENT,
            &self.config.user_agent,
        )?;
        if let Some(account_id) = account_id {
            insert_static(&mut headers, "ChatGPT-Account-Id", account_id)?;
        }
        if let Some(session_id) = session_id {
            insert_static(&mut headers, "session-id", session_id)?;
            insert_static(&mut headers, "thread-id", session_id)?;
            insert_static(&mut headers, "session_id", session_id)?;
            insert_static(&mut headers, "x-client-request-id", session_id)?;
            insert_static(
                &mut headers,
                "x-codex-window-id",
                &format!("{session_id}:0"),
            )?;
        }
        if responses_lite {
            insert_static(
                &mut headers,
                "x-openai-internal-codex-responses-lite",
                "true",
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
            "OpenAI Responses base URL must be http(s): {url}"
        )))
    }
}

fn build_websocket_request(
    ws_url: &str,
    config: &OpenAIResponsesConfig,
    access_token: Option<&str>,
    account_id: Option<&str>,
    session_id: Option<&str>,
) -> Result<WebSocketRequest> {
    let uri = ws_url
        .parse::<http::Uri>()
        .map_err(|err| ProxyError::Transport(format!("bad websocket URL: {err}")))?;
    let mut request = ClientRequestBuilder::new(uri)
        .with_header("openai-beta", "responses_websockets=2026-02-06")
        .with_header("originator", config.originator.as_str())
        .with_header("version", config.compat_version.as_str())
        .with_header("user-agent", config.user_agent.as_str());
    if let Some(access_token) = access_token {
        request = request.with_header("authorization", format!("Bearer {access_token}"));
    }
    if let Some(account_id) = account_id {
        request = request.with_header("ChatGPT-Account-Id", account_id);
    }
    if let Some(session_id) = session_id {
        request = request
            .with_header("session-id", session_id)
            .with_header("thread-id", session_id)
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
        if is_responses_lite_model(&body.model) {
            let metadata = object.entry("client_metadata").or_insert_with(|| json!({}));
            if let Some(metadata) = metadata.as_object_mut() {
                metadata.insert(
                    "ws_request_header_x_openai_internal_codex_responses_lite".into(),
                    json!("true"),
                );
            }
        }
    }
    value.to_string()
}

fn is_responses_lite_model(model: &str) -> bool {
    model.starts_with("gpt-5.6-")
}

async fn read_initial_websocket_events<S>(socket: &mut WebSocketStream<S>) -> Result<Vec<Message>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    tokio::time::timeout(WEBSOCKET_FIRST_EVENT_TIMEOUT, async {
        let mut buffered = Vec::new();
        loop {
            match socket.next().await {
                Some(Ok(message @ Message::Text(_))) | Some(Ok(message @ Message::Binary(_))) => {
                    reject_websocket_error(&message)?;
                    let model_response_started = websocket_model_response_started(&message);
                    buffered.push(message);
                    if model_response_started {
                        return Ok(buffered);
                    }
                }
                Some(Ok(Message::Close(frame))) => {
                    let detail = frame
                        .map(|frame| format!(": {} {}", frame.code, frame.reason))
                        .unwrap_or_default();
                    return Err(ProxyError::Transport(format!(
                        "OpenAI Responses websocket closed before first model event{detail}"
                    )));
                }
                Some(Ok(_)) => {}
                Some(Err(err)) => {
                    return Err(ProxyError::Transport(format!(
                        "OpenAI Responses websocket read failed before first model event: {err}"
                    )));
                }
                None => {
                    return Err(ProxyError::Transport(
                        "OpenAI Responses websocket ended before first model event".into(),
                    ));
                }
            }
        }
    })
    .await
    .map_err(|_| {
        ProxyError::Transport("timed out waiting for first OpenAI Responses model event".into())
    })?
}

fn websocket_model_response_started(message: &Message) -> bool {
    websocket_json_value(message)
        .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
        .is_some_and(|typ| {
            typ.starts_with("response.") && !typ.contains("rate_limit") && !typ.contains("metadata")
        })
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
        .unwrap_or("OpenAI Responses websocket returned an error event");

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

fn websocket_message_is_terminal(message: &Message) -> bool {
    websocket_json_value(message)
        .and_then(|value| {
            value
                .get("type")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .is_some_and(|typ| {
            matches!(
                typ.as_str(),
                "response.completed"
                    | "response.failed"
                    | "response.incomplete"
                    | "response.cancelled"
            )
        })
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
        let config = OpenAIResponsesConfig::from(CodexConfig::default());
        let request = build_websocket_request(
            "wss://example.test/backend-api/codex/responses",
            &config,
            Some("access-token"),
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
        assert_eq!(headers.get("session-id").unwrap(), "session-id");
        assert_eq!(headers.get("thread-id").unwrap(), "session-id");
        assert_eq!(
            headers.get("openai-beta").unwrap(),
            "responses_websockets=2026-02-06"
        );
        assert_eq!(
            headers.get("version").unwrap(),
            crate::config::DEFAULT_CODEX_COMPAT_VERSION
        );
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
    fn websocket_create_payload_carries_responses_lite_signal() {
        let mut request = minimal_response_request();
        request.model = "gpt-5.6-luna".into();
        request.client_metadata = Some(json!({"thread_id": "thread-1"}));
        let value = serde_json::from_str::<Value>(&websocket_create_payload(&request)).unwrap();

        assert_eq!(
            value["client_metadata"]["ws_request_header_x_openai_internal_codex_responses_lite"],
            "true"
        );
        assert_eq!(value["client_metadata"]["thread_id"], "thread-1");
    }

    #[test]
    fn initial_metadata_events_do_not_commit_websocket_stream() {
        assert!(!websocket_model_response_started(&Message::Text(
            json!({"type": "response.rate_limits"}).to_string()
        )));
        assert!(!websocket_model_response_started(&Message::Text(
            json!({"type": "response.metadata"}).to_string()
        )));
        assert!(websocket_model_response_started(&Message::Text(
            json!({"type": "response.created"}).to_string()
        )));
    }

    #[test]
    fn custom_http_headers_use_shared_contract_without_required_auth() {
        let client = OpenAIResponsesClient::new_custom(
            CustomOpenAIConfig {
                base_url: "https://example.test".into(),
                transport: CodexTransport::Http,
                ..Default::default()
            },
            PathBuf::from("/definitely/missing/custom-openai-key"),
        )
        .unwrap();
        let headers = client.headers(None, None, Some("session-1"), true).unwrap();

        assert!(headers.get(reqwest::header::AUTHORIZATION).is_none());
        assert!(headers.get("ChatGPT-Account-Id").is_none());
        assert_eq!(headers.get("originator").unwrap(), DEFAULT_ORIGINATOR);
        assert_eq!(
            headers
                .get("x-openai-internal-codex-responses-lite")
                .unwrap(),
            "true"
        );
        assert_eq!(headers.get("session-id").unwrap(), "session-1");
        assert_eq!(headers.get("thread-id").unwrap(), "session-1");
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
            .to_string(),
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

    #[test]
    fn websocket_terminal_detection_only_stops_on_response_terminal_events() {
        assert!(websocket_message_is_terminal(&Message::Text(
            json!({"type": "response.completed"}).to_string()
        )));
        assert!(websocket_message_is_terminal(&Message::Text(
            json!({"type": "response.failed"}).to_string()
        )));
        assert!(!websocket_message_is_terminal(&Message::Text(
            json!({"type": "response.output_text.done"}).to_string()
        )));
        assert!(!websocket_message_is_terminal(&Message::Text(
            json!({"type": "response.content_part.done"}).to_string()
        )));
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
            parallel_tool_calls: None,
            reasoning: None,
            include: None,
            text: None,
            service_tier: None,
            prompt_cache_key: None,
            client_metadata: None,
            stream: true,
        }
    }
}
