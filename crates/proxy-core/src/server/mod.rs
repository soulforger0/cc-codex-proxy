use crate::{
    anthropic::{
        response::{message_delta, message_start, message_stop, response_json},
        schema::{AnthropicRequest, AnthropicResponse, AnthropicTool, AnthropicUsage},
        tokens::estimate_input_tokens,
    },
    auth::AuthManager,
    codex::{
        client::{CodexClient, CodexTransportMethod},
        stream::{accumulate_response, translate_stream, ToolCatalog},
        translate::{translate_request, ResponsesRequest},
    },
    config::{AppConfig, AppPaths, CodexTransport, CustomOpenAIProtocol, Provider},
    custom_openai::{
        accumulate_chat_response, translate_chat_request, translate_chat_stream,
        translate_responses_request, CustomOpenAIClient,
    },
    deepseek::DeepSeekClient,
    error::{ProxyError, Result},
    model::ModelRegistry,
    routing::RouteManager,
};
use async_stream::try_stream;
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Json, State},
    http::{header, HeaderMap, Response, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use uuid::Uuid;

const SSE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const SSE_HEARTBEAT_COMMENT: &[u8] = b": heartbeat\n\n";
const TRANSCRIPT_TOKEN_DROP_MIN: u64 = 32_768;

#[derive(Clone)]
struct AppState {
    config: AppConfig,
    paths: AppPaths,
    auth: AuthManager,
    codex: CodexClient,
    deepseek: DeepSeekClient,
    custom_openai: CustomOpenAIClient,
    registry: ModelRegistry,
    routes: RouteManager,
    codex_sessions: CodexSessionManager,
}

#[derive(Clone)]
struct CodexSessionManager {
    namespace: String,
    sessions: Arc<Mutex<HashMap<String, CodexSessionState>>>,
}

#[derive(Debug, Default)]
struct CodexSessionState {
    generation: u64,
    initialized: bool,
    last_message_count: usize,
    last_input_tokens: u64,
}

#[derive(Debug)]
struct CodexSessionResolution {
    upstream_session_id: Option<String>,
    generation: Option<u64>,
    reset_reason: Option<CodexSessionResetReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexSessionResetReason {
    ClaudeClearCommand,
    TranscriptShrink,
}

impl CodexSessionResetReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeClearCommand => "claude-clear-command",
            Self::TranscriptShrink => "transcript-shrink",
        }
    }
}

impl CodexSessionManager {
    fn new() -> Self {
        Self {
            namespace: Uuid::new_v4().simple().to_string(),
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn resolve(
        &self,
        claude_session_id: Option<&str>,
        request: &AnthropicRequest,
    ) -> CodexSessionResolution {
        let Some(claude_session_id) = claude_session_id else {
            return CodexSessionResolution {
                upstream_session_id: None,
                generation: None,
                reset_reason: None,
            };
        };

        let clear_command = is_claude_clear_command_request(request);
        let message_count = request.messages.len();
        let input_tokens = estimate_input_tokens(request);
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = sessions
            .entry(claude_session_id.to_string())
            .or_insert_with(CodexSessionState::default);

        let reset_reason = if clear_command {
            state.generation = state.generation.saturating_add(1);
            Some(CodexSessionResetReason::ClaudeClearCommand)
        } else if state.initialized && transcript_shrank(state, message_count, input_tokens) {
            state.generation = state.generation.saturating_add(1);
            Some(CodexSessionResetReason::TranscriptShrink)
        } else {
            None
        };

        if clear_command {
            state.last_message_count = 0;
            state.last_input_tokens = 0;
        } else {
            state.last_message_count = message_count;
            state.last_input_tokens = input_tokens;
        }
        state.initialized = true;

        CodexSessionResolution {
            upstream_session_id: Some(
                self.upstream_session_id(claude_session_id, state.generation),
            ),
            generation: Some(state.generation),
            reset_reason,
        }
    }

    fn upstream_session_id(&self, claude_session_id: &str, generation: u64) -> String {
        let fragment = header_safe_session_fragment(claude_session_id);
        if generation == 0 {
            format!("ccp-{}-{fragment}", self.namespace)
        } else {
            format!("ccp-{}-{fragment}-g{generation}", self.namespace)
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RouteUpdateRequest {
    active_profile: String,
}

pub struct ServerHandle {
    pub addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
    shutdown_grace_period: Duration,
}

impl ServerHandle {
    pub async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        match tokio::time::timeout(self.shutdown_grace_period, &mut self.task).await {
            Ok(result) => {
                let _ = result;
            }
            Err(_) => {
                warn!(
                    timeout_ms = self.shutdown_grace_period.as_millis(),
                    "proxy server graceful shutdown timed out; aborting server task"
                );
                self.task.abort();
                let _ = self.task.await;
            }
        }
    }
}

pub async fn serve(config: AppConfig, paths: AppPaths, auth: AuthManager) -> Result<ServerHandle> {
    let registry = ModelRegistry::load_or_create(&paths.model_profiles_file)?;
    let routes =
        RouteManager::from_config_and_store(&config.routing, paths.route_pins_file.clone())?;
    let codex = CodexClient::new(config.codex.clone(), auth.clone())?;
    let deepseek =
        DeepSeekClient::new(config.deepseek.clone(), paths.deepseek_api_key_file.clone())?;
    let custom_openai = CustomOpenAIClient::new(
        config.custom_openai.clone(),
        paths.custom_openai_api_key_file.clone(),
    )?;
    let state = Arc::new(AppState {
        config: config.clone(),
        paths,
        auth,
        codex,
        deepseek,
        custom_openai,
        registry,
        routes,
        codex_sessions: CodexSessionManager::new(),
    });

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/admin/status", get(admin_status))
        .route("/admin/route", get(admin_route).put(update_admin_route))
        .route(
            "/v1/messages",
            post(messages).layer(DefaultBodyLimit::max(config.messages_body_limit_bytes)),
        )
        .route(
            "/v1/messages/count_tokens",
            post(count_tokens).layer(DefaultBodyLimit::max(config.messages_body_limit_bytes)),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = TcpListener::bind(("127.0.0.1", config.port)).await?;
    let addr = listener.local_addr()?;
    let (tx, rx) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        info!(%addr, "proxy server listening");
        let server = axum::serve(listener, app).with_graceful_shutdown(async {
            let _ = rx.await;
        });
        if let Err(err) = server.await {
            warn!(error = %err, "proxy server stopped with error");
        }
    });

    Ok(ServerHandle {
        addr,
        shutdown: Some(tx),
        task,
        shutdown_grace_period: Duration::from_millis(config.shutdown_grace_period_ms),
    })
}

async fn healthz() -> impl IntoResponse {
    Json(json!({ "ok": true }))
}

async fn admin_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    require_admin(&state, &headers)?;
    let route_status = state.routes.status().await?;
    let auth = if route_status.active_provider == Provider::Codex {
        state.auth.status().await?
    } else {
        None
    };
    Ok(Json(json!({
        "ok": true,
        "provider": route_status.active_provider.as_str(),
        "activeProvider": route_status.active_provider.as_str(),
        "activeProfile": route_status.active_profile,
        "sessionPolicy": route_status.session_policy,
        "pinnedSessionCount": route_status.pinned_session_count,
        "sessionPinTtlSeconds": route_status.session_pin_ttl_seconds,
        "maxPinnedSessions": route_status.max_pinned_sessions,
        "persistSessionPins": route_status.persist_session_pins,
        "routes": route_status.routes,
        "baseUrl": format!("http://127.0.0.1:{}", state.config.port),
        "publicModels": {
            "primary": state.config.claude.public_primary_model,
            "small": state.config.claude.public_small_model,
        },
        "port": state.config.port,
        "configDir": state.paths.config_dir,
        "logsDir": state.paths.logs_dir,
        "transport": transport_status_json(&state, route_status.active_provider),
        "models": state.registry.supported_models(route_status.active_provider),
        "deepseek": {
            "apiKey": state.deepseek.api_key_status(),
        },
        "customOpenAI": {
            "apiKey": state.custom_openai.api_key_status(),
            "baseUrlConfigured": state.custom_openai.base_url_configured(),
            "protocol": custom_openai_protocol_name(&state.custom_openai.protocol()),
        },
        "auth": auth.map(|auth| json!({
            "accountId": auth.account_id,
            "expiresAtMs": auth.expires_at_ms,
            "storage": state.auth.storage_label(),
        }))
    })))
}

async fn admin_route(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    require_admin(&state, &headers)?;
    let active = state.routes.active_route().await?;
    let status = state.routes.status().await?;
    Ok(Json(json!({
        "activeProfile": active.id,
        "activeProvider": active.provider.as_str(),
        "route": active,
        "sessionPolicy": status.session_policy,
        "pinnedSessionCount": status.pinned_session_count,
        "sessionPinTtlSeconds": status.session_pin_ttl_seconds,
        "maxPinnedSessions": status.max_pinned_sessions,
        "persistSessionPins": status.persist_session_pins,
    })))
}

async fn update_admin_route(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<RouteUpdateRequest>,
) -> Result<Json<serde_json::Value>> {
    require_admin(&state, &headers)?;
    let route = state
        .routes
        .set_active_profile(&request.active_profile)
        .await?;
    info!(
        active_profile = %route.id,
        provider = route.provider.as_str(),
        "active route updated"
    );
    Ok(Json(json!({
        "ok": true,
        "activeProfile": route.id,
        "activeProvider": route.provider.as_str(),
        "route": route,
    })))
}

async fn count_tokens(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<AnthropicRequest>,
) -> Result<Json<serde_json::Value>> {
    let session_id = claude_session_id(&headers);
    let route = state
        .routes
        .resolve_for_request(session_id.as_deref())
        .await?;
    let _ = state.registry.resolve_for_route(
        &route,
        &state.config.claude.public_primary_model,
        &state.config.claude.public_small_model,
        &request.model,
    )?;
    Ok(Json(
        json!({ "input_tokens": estimate_input_tokens(&request) }),
    ))
}

async fn messages(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<AnthropicRequest>,
) -> Result<Response<Body>> {
    let request_id = format!("req_{}", Uuid::new_v4().simple());
    let session_id = claude_session_id(&headers);
    let route = state
        .routes
        .resolve_for_request(session_id.as_deref())
        .await?;
    info!(
        %request_id,
        route = %route.id,
        provider = route.provider.as_str(),
        model = %request.model,
        stream = request.wants_stream(),
        message_count = request.messages.len(),
        tool_count = request.tools.as_ref().map_or(0, Vec::len),
        session_present = session_id.is_some(),
        tool_names = %summarize_anthropic_tool_names(request.tools.as_deref()),
        tools = %summarize_anthropic_tools(request.tools.as_deref()),
        "received Anthropic messages request"
    );
    let resolved = match state.registry.resolve_for_route(
        &route,
        &state.config.claude.public_primary_model,
        &state.config.claude.public_small_model,
        &request.model,
    ) {
        Ok(resolved) => resolved,
        Err(err) => {
            warn!(%request_id, error = %err, "failed to resolve requested model");
            return Err(err);
        }
    };
    match route.provider {
        Provider::Codex => {
            handle_codex_messages(state, request_id, session_id, request, resolved).await
        }
        Provider::DeepSeek => handle_deepseek_messages(state, request_id, request, resolved).await,
        Provider::CustomOpenAI => {
            handle_custom_openai_messages(state, request_id, session_id, request, resolved).await
        }
    }
}

async fn handle_codex_messages(
    state: Arc<AppState>,
    request_id: String,
    session_id: Option<String>,
    request: AnthropicRequest,
    resolved: crate::model::ResolvedModel,
) -> Result<Response<Body>> {
    let codex_session = state
        .codex_sessions
        .resolve(session_id.as_deref(), &request);
    if let Some(reason) = codex_session.reset_reason {
        info!(
            %request_id,
            reason = reason.as_str(),
            session_generation = codex_session.generation.unwrap_or(0),
            "started fresh Codex upstream session"
        );
        if reason == CodexSessionResetReason::ClaudeClearCommand {
            return empty_anthropic_response(&request);
        }
    }

    let tool_catalog = ToolCatalog::from_anthropic_tools(request.tools.as_deref());
    let upstream_session_id = codex_session.upstream_session_id.as_deref();
    let translated = translate_request(&request, &resolved, upstream_session_id)?;
    info!(
        %request_id,
        upstream_model = %resolved.upstream_model,
        input_items = translated.input.len(),
        codex_tool_count = translated.tools.as_ref().map_or(0, Vec::len),
        codex_body_keys = %codex_request_keys(&translated),
        upstream_session_present = upstream_session_id.is_some(),
        session_generation = codex_session.generation.unwrap_or(0),
        "translated request for Codex"
    );
    let upstream = match state.codex.post(&translated, upstream_session_id).await {
        Ok(upstream) => upstream,
        Err(err) => {
            warn!(%request_id, error = %err, "Codex upstream request failed");
            return Err(err);
        }
    };
    info!(
        %request_id,
        status = %upstream.status,
        "Codex upstream stream opened"
    );

    if request.wants_stream() {
        let stream = translate_stream(
            upstream.body,
            request.model.clone(),
            tool_catalog,
            Some(request_id),
        );
        sse_response(stream)
    } else {
        let response = accumulate_response(
            upstream.body,
            request.model.clone(),
            tool_catalog,
            Some(request_id),
        )
        .await?;
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(response_json(response).to_string()))
            .map_err(|err| ProxyError::Transport(format!("failed to build response: {err}")))
    }
}

async fn handle_deepseek_messages(
    state: Arc<AppState>,
    request_id: String,
    request: AnthropicRequest,
    resolved: crate::model::ResolvedModel,
) -> Result<Response<Body>> {
    let upstream = match state.deepseek.post(&request, &resolved).await {
        Ok(upstream) => upstream,
        Err(err) => {
            warn!(%request_id, error = %err, "DeepSeek upstream request failed");
            return Err(err);
        }
    };
    info!(
        %request_id,
        status = %upstream.status,
        upstream_model = %resolved.upstream_model,
        "DeepSeek upstream response opened"
    );

    if request.wants_stream() {
        sse_response(upstream.body)
    } else {
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from_stream(upstream.body))
            .map_err(|err| {
                ProxyError::Transport(format!("failed to build DeepSeek response: {err}"))
            })
    }
}

async fn handle_custom_openai_messages(
    state: Arc<AppState>,
    request_id: String,
    session_id: Option<String>,
    request: AnthropicRequest,
    resolved: crate::model::ResolvedModel,
) -> Result<Response<Body>> {
    match state.custom_openai.protocol() {
        CustomOpenAIProtocol::Responses => {
            handle_custom_openai_responses(state, request_id, session_id, request, resolved).await
        }
        CustomOpenAIProtocol::ChatCompletions => {
            handle_custom_openai_chat(state, request_id, request, resolved).await
        }
    }
}

async fn handle_custom_openai_responses(
    state: Arc<AppState>,
    request_id: String,
    session_id: Option<String>,
    request: AnthropicRequest,
    resolved: crate::model::ResolvedModel,
) -> Result<Response<Body>> {
    let tool_catalog = ToolCatalog::from_anthropic_tools(request.tools.as_deref());
    let translated = translate_responses_request(&request, &resolved, session_id.as_deref())?;
    info!(
        %request_id,
        upstream_model = %resolved.upstream_model,
        input_items = translated.input.len(),
        custom_openai_tool_count = translated.tools.as_ref().map_or(0, Vec::len),
        custom_openai_body_keys = %codex_request_keys(&translated),
        "translated request for custom OpenAI Responses"
    );
    let upstream = match state.custom_openai.post_responses(&translated).await {
        Ok(upstream) => upstream,
        Err(err) => {
            warn!(%request_id, error = %err, "custom OpenAI Responses upstream request failed");
            return Err(err);
        }
    };

    if request.wants_stream() {
        let stream = translate_stream(
            upstream.body,
            request.model.clone(),
            tool_catalog,
            Some(request_id),
        );
        sse_response(stream)
    } else {
        let response = accumulate_response(
            upstream.body,
            request.model.clone(),
            tool_catalog,
            Some(request_id),
        )
        .await?;
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(response_json(response).to_string()))
            .map_err(|err| {
                ProxyError::Transport(format!(
                    "failed to build custom OpenAI Responses response: {err}"
                ))
            })
    }
}

async fn handle_custom_openai_chat(
    state: Arc<AppState>,
    request_id: String,
    request: AnthropicRequest,
    resolved: crate::model::ResolvedModel,
) -> Result<Response<Body>> {
    let translated = translate_chat_request(&request, &resolved)?;
    let upstream = match state.custom_openai.post_chat(&translated).await {
        Ok(upstream) => upstream,
        Err(err) => {
            warn!(%request_id, error = %err, "custom OpenAI Chat Completions upstream request failed");
            return Err(err);
        }
    };
    info!(
        %request_id,
        status = %upstream.status,
        upstream_model = %resolved.upstream_model,
        "custom OpenAI Chat Completions upstream response opened"
    );

    if request.wants_stream() {
        let stream = translate_chat_stream(upstream.body, request.model.clone(), Some(request_id));
        sse_response(stream)
    } else {
        let response = accumulate_chat_response(upstream.body, request.model.clone()).await?;
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(response_json(response).to_string()))
            .map_err(|err| {
                ProxyError::Transport(format!(
                    "failed to build custom OpenAI chat response: {err}"
                ))
            })
    }
}

fn sse_response(
    stream: Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>,
) -> Result<Response<Body>> {
    let body = Body::from_stream(with_sse_heartbeats(stream, SSE_HEARTBEAT_INTERVAL));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache, no-transform")
        .header("x-accel-buffering", "no")
        .body(body)
        .map_err(|err| ProxyError::Transport(format!("failed to build streaming response: {err}")))
}

fn empty_anthropic_response(request: &AnthropicRequest) -> Result<Response<Body>> {
    let message_id = format!("msg_{}", Uuid::new_v4().simple());
    if request.wants_stream() {
        let events = vec![
            Ok(message_start(&message_id, &request.model)),
            Ok(message_delta(Some("end_turn"), empty_usage())),
            Ok(message_stop()),
        ];
        return sse_response(Box::pin(futures_util::stream::iter(events)));
    }

    let response = AnthropicResponse {
        id: message_id,
        kind: "message".into(),
        role: "assistant".into(),
        model: request.model.clone(),
        content: Vec::new(),
        stop_reason: Some("end_turn".into()),
        stop_sequence: None,
        usage: empty_usage(),
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(response_json(response).to_string()))
        .map_err(|err| ProxyError::Transport(format!("failed to build response: {err}")))
}

fn empty_usage() -> AnthropicUsage {
    AnthropicUsage {
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
    }
}

fn with_sse_heartbeats(
    stream: Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>,
    interval: Duration,
) -> Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>> {
    Box::pin(try_stream! {
        futures_util::pin_mut!(stream);
        loop {
            let sleep = tokio::time::sleep(interval);
            tokio::pin!(sleep);
            tokio::select! {
                item = stream.next() => {
                    match item {
                        Some(item) => yield item?,
                        None => break,
                    }
                }
                _ = &mut sleep => {
                    yield Bytes::from_static(SSE_HEARTBEAT_COMMENT);
                }
            }
        }
    })
}

fn claude_session_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-claude-code-session-id")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

fn transcript_shrank(state: &CodexSessionState, message_count: usize, input_tokens: u64) -> bool {
    if message_count < state.last_message_count {
        return true;
    }
    state.last_input_tokens > input_tokens
        && state.last_input_tokens - input_tokens >= TRANSCRIPT_TOKEN_DROP_MIN
        && input_tokens.saturating_mul(2) < state.last_input_tokens
}

fn is_claude_clear_command_request(request: &AnthropicRequest) -> bool {
    if request.messages.len() != 1 {
        return false;
    }
    let message = &request.messages[0];
    if message.role != "user" {
        return false;
    }
    anthropic_text_content(&message.content).is_some_and(|text| {
        let Some(command) = text.split_whitespace().next() else {
            return false;
        };
        matches!(command, "/clear" | "/reset" | "/new")
    })
}

fn anthropic_text_content(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let mut out = Vec::new();
            for item in items {
                if item.get("type").and_then(Value::as_str).unwrap_or("text") != "text" {
                    return None;
                }
                out.push(item.get("text").and_then(Value::as_str)?.to_string());
            }
            Some(out.join(""))
        }
        _ => None,
    }
}

fn header_safe_session_fragment(session_id: &str) -> String {
    let mut fragment = session_id
        .chars()
        .take(80)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    while fragment.ends_with('-') {
        fragment.pop();
    }
    if fragment.is_empty() {
        "session".into()
    } else {
        fragment
    }
}

fn summarize_anthropic_tool_names(tools: Option<&[AnthropicTool]>) -> String {
    let Some(tools) = tools else {
        return "none".into();
    };
    if tools.is_empty() {
        return "none".into();
    }
    tools
        .iter()
        .take(128)
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>()
        .join("|")
}

fn summarize_anthropic_tools(tools: Option<&[AnthropicTool]>) -> String {
    let Some(tools) = tools else {
        return "none".into();
    };
    if tools.is_empty() {
        return "none".into();
    }
    let mut summaries = tools
        .iter()
        .take(16)
        .map(|tool| {
            let required = schema_string_list(tool.input_schema.as_ref(), "required");
            let properties = tool
                .input_schema
                .as_ref()
                .and_then(|schema| schema.get("properties"))
                .and_then(Value::as_object)
                .map(|properties| {
                    properties
                        .keys()
                        .take(12)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("|")
                })
                .unwrap_or_default();
            format!(
                "{}(required=[{}],properties=[{}])",
                tool.name, required, properties
            )
        })
        .collect::<Vec<_>>();
    if tools.len() > summaries.len() {
        summaries.push(format!("...+{}", tools.len() - summaries.len()));
    }
    summaries.join(",")
}

fn schema_string_list(schema: Option<&Value>, key: &str) -> String {
    schema
        .and_then(|schema| schema.get(key))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .take(12)
                .collect::<Vec<_>>()
                .join("|")
        })
        .unwrap_or_default()
}

fn codex_request_keys(request: &ResponsesRequest) -> String {
    serde_json::to_value(request)
        .ok()
        .and_then(|value| {
            value.as_object().map(|object| {
                let mut keys = object.keys().cloned().collect::<Vec<_>>();
                keys.sort();
                keys.join(",")
            })
        })
        .unwrap_or_else(|| "unavailable".into())
}

fn configured_transport_name(transport: &CodexTransport) -> &'static str {
    match transport {
        CodexTransport::Auto => "auto",
        CodexTransport::Http => "http-sse",
        CodexTransport::WebSocket => "websocket",
    }
}

fn transport_status_json(state: &AppState, provider: Provider) -> serde_json::Value {
    match provider {
        Provider::Codex => {
            let transport = state.codex.transport_status();
            json!({
                "configured": configured_transport_name(&state.config.codex.transport),
                "currentMethod": transport.current_method.map(transport_method_name),
                "websocketCooldownMs": transport.websocket_cooldown_remaining.map(duration_millis_u64),
            })
        }
        Provider::DeepSeek => json!({
            "configured": "http-sse",
            "currentMethod": "http-sse",
            "websocketCooldownMs": null,
        }),
        Provider::CustomOpenAI => json!({
            "configured": custom_openai_protocol_name(&state.custom_openai.protocol()),
            "currentMethod": "http-sse",
            "websocketCooldownMs": null,
        }),
    }
}

fn custom_openai_protocol_name(protocol: &CustomOpenAIProtocol) -> &'static str {
    match protocol {
        CustomOpenAIProtocol::Responses => "responses",
        CustomOpenAIProtocol::ChatCompletions => "chat-completions",
    }
}

fn transport_method_name(method: CodexTransportMethod) -> &'static str {
    match method {
        CodexTransportMethod::HttpSse => "http-sse",
        CodexTransportMethod::WebSocket => "websocket",
    }
}

fn duration_millis_u64(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<()> {
    let supplied = headers
        .get("x-cc-codex-admin-token")
        .and_then(|value| value.to_str().ok());
    if supplied == Some(state.config.admin_token.as_str()) {
        Ok(())
    } else {
        Err(ProxyError::InvalidRequest(
            "missing or invalid admin token".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::schema::AnthropicMessage;
    use futures_util::{stream, StreamExt};

    fn boxed<S>(stream: S) -> Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>
    where
        S: Stream<Item = Result<Bytes>> + Send + 'static,
    {
        Box::pin(stream)
    }

    fn request_with_text(text: &str) -> AnthropicRequest {
        AnthropicRequest {
            model: "gpt-5.5".into(),
            max_tokens: Some(1),
            temperature: None,
            top_p: None,
            stream: None,
            system: None,
            messages: vec![AnthropicMessage {
                role: "user".into(),
                content: Value::String(text.into()),
                extra: serde_json::Map::new(),
            }],
            tools: None,
            tool_choice: None,
            metadata: None,
            output_config: None,
            thinking: None,
            extra: serde_json::Map::new(),
        }
    }

    #[test]
    fn recognizes_claude_new_conversation_commands() {
        for text in [
            "/clear",
            " /clear ",
            "/clear auth refactor",
            "/reset",
            "/reset labeled previous chat",
            "/new",
        ] {
            assert!(is_claude_clear_command_request(&request_with_text(text)));
        }

        for text in ["/compact", "please /clear", "", "   "] {
            assert!(!is_claude_clear_command_request(&request_with_text(text)));
        }
    }

    #[test]
    fn sse_response_sets_streaming_headers() {
        let response = sse_response(boxed(stream::empty())).expect("response");
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/event-stream; charset=utf-8"
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-cache, no-transform"
        );
        assert_eq!(response.headers().get("x-accel-buffering").unwrap(), "no");
    }

    #[tokio::test]
    async fn heartbeat_is_not_sent_immediately() {
        let mut stream = with_sse_heartbeats(boxed(stream::pending()), Duration::from_millis(50));

        let result = tokio::time::timeout(Duration::from_millis(10), stream.next()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn heartbeat_is_sent_after_idle_interval() {
        let mut stream = with_sse_heartbeats(boxed(stream::pending()), Duration::from_millis(10));

        let chunk = tokio::time::timeout(Duration::from_millis(50), stream.next())
            .await
            .expect("heartbeat should be emitted")
            .expect("stream should stay open")
            .expect("heartbeat should be ok");
        assert_eq!(chunk, Bytes::from_static(SSE_HEARTBEAT_COMMENT));
    }

    #[tokio::test]
    async fn heartbeats_repeat_during_long_idle_periods() {
        let mut stream = with_sse_heartbeats(boxed(stream::pending()), Duration::from_millis(10));

        for _ in 0..2 {
            let chunk = tokio::time::timeout(Duration::from_millis(50), stream.next())
                .await
                .expect("heartbeat should be emitted")
                .expect("stream should stay open")
                .expect("heartbeat should be ok");
            assert_eq!(chunk, Bytes::from_static(SSE_HEARTBEAT_COMMENT));
        }
    }

    #[tokio::test]
    async fn forwarded_chunks_reset_heartbeat_timer() {
        let event = Bytes::from_static(b"event: message_start\ndata: {}\n\n");
        let expected = event.clone();
        let source = stream::once(async move { Ok(event) }).chain(stream::pending());
        let mut stream = with_sse_heartbeats(boxed(source), Duration::from_millis(30));

        let first = stream
            .next()
            .await
            .expect("first item")
            .expect("ok first chunk");
        assert_eq!(first, expected);

        let result = tokio::time::timeout(Duration::from_millis(10), stream.next()).await;
        assert!(result.is_err());
    }
}
