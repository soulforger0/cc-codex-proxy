use crate::{
    anthropic::schema::AnthropicTool,
    anthropic::{response::response_json, schema::AnthropicRequest, tokens::estimate_input_tokens},
    auth::AuthManager,
    codex::{
        client::CodexClient,
        stream::{accumulate_response, translate_stream},
        translate::{translate_request, ResponsesRequest},
    },
    config::{AppConfig, AppPaths},
    error::{ProxyError, Result},
    model::ModelRegistry,
};
use axum::{
    body::Body,
    extract::{Json, State},
    http::{header, HeaderMap, Response, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use std::{net::SocketAddr, sync::Arc};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    config: AppConfig,
    paths: AppPaths,
    auth: AuthManager,
    codex: CodexClient,
    registry: ModelRegistry,
}

pub struct ServerHandle {
    pub addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl ServerHandle {
    pub async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let _ = self.task.await;
    }
}

pub async fn serve(config: AppConfig, paths: AppPaths, auth: AuthManager) -> Result<ServerHandle> {
    let registry = ModelRegistry::load_or_create(&paths.model_profiles_file)?;
    let codex = CodexClient::new(config.codex.clone(), auth.clone())?;
    let state = Arc::new(AppState {
        config: config.clone(),
        paths,
        auth,
        codex,
        registry,
    });

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/admin/status", get(admin_status))
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
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
    let auth = state.auth.status().await?;
    Ok(Json(json!({
        "ok": true,
        "port": state.config.port,
        "configDir": state.paths.config_dir,
        "logsDir": state.paths.logs_dir,
        "models": state.registry.supported_models(),
        "auth": auth.map(|auth| json!({
            "accountId": auth.account_id,
            "expiresAtMs": auth.expires_at_ms,
            "storage": state.auth.storage_label(),
        }))
    })))
}

async fn count_tokens(
    State(state): State<Arc<AppState>>,
    Json(request): Json<AnthropicRequest>,
) -> Result<Json<serde_json::Value>> {
    let _ = state.registry.resolve(&request.model)?;
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
    let session_id = headers
        .get("x-claude-code-session-id")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    info!(
        %request_id,
        model = %request.model,
        stream = request.wants_stream(),
        message_count = request.messages.len(),
        tool_count = request.tools.as_ref().map_or(0, Vec::len),
        session_present = session_id.is_some(),
        tools = %summarize_anthropic_tools(request.tools.as_deref()),
        "received Anthropic messages request"
    );
    let resolved = match state.registry.resolve(&request.model) {
        Ok(resolved) => resolved,
        Err(err) => {
            warn!(%request_id, error = %err, "failed to resolve requested model");
            return Err(err);
        }
    };
    let translated = translate_request(&request, &resolved, session_id.as_deref())?;
    info!(
        %request_id,
        upstream_model = %resolved.upstream_model,
        input_items = translated.input.len(),
        codex_tool_count = translated.tools.as_ref().map_or(0, Vec::len),
        codex_body_keys = %codex_request_keys(&translated),
        "translated request for Codex"
    );
    let upstream = match state.codex.post(&translated, session_id.as_deref()).await {
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
        let stream = translate_stream(upstream.body, request.model.clone(), Some(request_id));
        let body = Body::from_stream(stream);
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(body)
            .map_err(|err| {
                ProxyError::Transport(format!("failed to build streaming response: {err}"))
            })
    } else {
        let response =
            accumulate_response(upstream.body, request.model.clone(), Some(request_id)).await?;
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(response_json(response).to_string()))
            .map_err(|err| ProxyError::Transport(format!("failed to build response: {err}")))
    }
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
