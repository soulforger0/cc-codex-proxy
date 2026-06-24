use crate::{
    anthropic::{
        response::response_json,
        schema::AnthropicRequest,
        tokens::estimate_input_tokens,
    },
    auth::AuthManager,
    codex::{
        client::CodexClient,
        stream::{accumulate_response, translate_stream},
        translate::translate_request,
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
use serde_json::json;
use std::{net::SocketAddr, sync::Arc};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

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

async fn admin_status(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Result<Json<serde_json::Value>> {
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
    Ok(Json(json!({ "input_tokens": estimate_input_tokens(&request) })))
}

async fn messages(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<AnthropicRequest>,
) -> Result<Response<Body>> {
    let session_id = headers
        .get("x-claude-code-session-id")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let resolved = state.registry.resolve(&request.model)?;
    let translated = translate_request(&request, &resolved, session_id.as_deref())?;
    let upstream = state.codex.post(&translated, session_id.as_deref()).await?;

    if request.wants_stream() {
        let stream = translate_stream(upstream.body, request.model.clone());
        let body = Body::from_stream(stream);
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(body)
            .map_err(|err| ProxyError::Transport(format!("failed to build streaming response: {err}")))
    } else {
        let response = accumulate_response(upstream.body, request.model.clone()).await?;
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(response_json(response).to_string()))
            .map_err(|err| ProxyError::Transport(format!("failed to build response: {err}")))
    }
}

fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<()> {
    let supplied = headers
        .get("x-cc-codex-admin-token")
        .and_then(|value| value.to_str().ok());
    if supplied == Some(state.config.admin_token.as_str()) {
        Ok(())
    } else {
        Err(ProxyError::InvalidRequest("missing or invalid admin token".into()))
    }
}

