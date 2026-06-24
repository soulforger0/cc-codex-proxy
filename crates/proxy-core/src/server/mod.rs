use crate::{
    anthropic::schema::AnthropicTool,
    anthropic::{response::response_json, schema::AnthropicRequest, tokens::estimate_input_tokens},
    auth::AuthManager,
    codex::{
        client::{CodexClient, CodexTransportMethod},
        stream::{accumulate_response, translate_stream, ToolCatalog},
        translate::{translate_request, ResponsesRequest},
    },
    config::{AppConfig, AppPaths, CodexTransport},
    error::{ProxyError, Result},
    model::ModelRegistry,
};
use async_stream::try_stream;
use axum::{
    body::Body,
    extract::{Json, State},
    http::{header, HeaderMap, Response, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde_json::{json, Value};
use std::{net::SocketAddr, pin::Pin, sync::Arc, time::Duration};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use uuid::Uuid;

const SSE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const SSE_HEARTBEAT_COMMENT: &[u8] = b": heartbeat\n\n";

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
    let transport = state.codex.transport_status();
    Ok(Json(json!({
        "ok": true,
        "port": state.config.port,
        "configDir": state.paths.config_dir,
        "logsDir": state.paths.logs_dir,
        "transport": {
            "configured": configured_transport_name(&state.config.codex.transport),
            "currentMethod": transport.current_method.map(transport_method_name),
            "websocketCooldownMs": transport.websocket_cooldown_remaining.map(duration_millis_u64),
        },
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
        tool_names = %summarize_anthropic_tool_names(request.tools.as_deref()),
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
    let tool_catalog = ToolCatalog::from_anthropic_tools(request.tools.as_deref());
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
        let stream = translate_stream(
            upstream.body,
            request.model.clone(),
            tool_catalog,
            Some(request_id),
        );
        let body = Body::from_stream(with_sse_heartbeats(stream, SSE_HEARTBEAT_INTERVAL));
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(body)
            .map_err(|err| {
                ProxyError::Transport(format!("failed to build streaming response: {err}"))
            })
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
    use futures_util::{stream, StreamExt};

    fn boxed<S>(stream: S) -> Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>
    where
        S: Stream<Item = Result<Bytes>> + Send + 'static,
    {
        Box::pin(stream)
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
            .expect("event should be forwarded")
            .expect("event should be ok");
        assert_eq!(first, expected);

        let early = tokio::time::timeout(Duration::from_millis(10), stream.next()).await;
        assert!(
            early.is_err(),
            "heartbeat should wait for a fresh idle interval"
        );

        let heartbeat = tokio::time::timeout(Duration::from_millis(50), stream.next())
            .await
            .expect("heartbeat should be emitted after reset interval")
            .expect("stream should stay open")
            .expect("heartbeat should be ok");
        assert_eq!(heartbeat, Bytes::from_static(SSE_HEARTBEAT_COMMENT));
    }

    #[tokio::test]
    async fn completed_stream_does_not_emit_trailing_heartbeat() {
        let mut stream = with_sse_heartbeats(boxed(stream::empty()), Duration::from_millis(10));

        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn stream_errors_are_forwarded() {
        let mut stream = with_sse_heartbeats(
            boxed(stream::iter(vec![Err(ProxyError::Transport(
                "boom".into(),
            ))])),
            Duration::from_millis(10),
        );

        let err = stream
            .next()
            .await
            .expect("error should be forwarded")
            .expect_err("item should be an error");
        assert!(err.to_string().contains("boom"));
    }

    #[tokio::test]
    async fn heartbeat_comment_is_inserted_between_complete_sse_frames() {
        let first =
            Bytes::from_static(b"event: message_start\ndata: {\"type\":\"message_start\"}\n\n");
        let second =
            Bytes::from_static(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        tx.send(Ok(first.clone())).await.unwrap();
        let source = tokio_stream::wrappers::ReceiverStream::new(rx);
        let mut stream = with_sse_heartbeats(boxed(source), Duration::from_millis(10));

        let first_chunk = stream
            .next()
            .await
            .expect("first frame should be forwarded")
            .expect("first frame should be ok");
        assert_eq!(first_chunk, first);

        let heartbeat = tokio::time::timeout(Duration::from_millis(50), stream.next())
            .await
            .expect("heartbeat should be emitted before the next frame")
            .expect("stream should stay open")
            .expect("heartbeat should be ok");
        assert_eq!(heartbeat, Bytes::from_static(SSE_HEARTBEAT_COMMENT));

        tx.send(Ok(second.clone())).await.unwrap();
        drop(tx);
        let second_chunk = stream
            .next()
            .await
            .expect("second frame should be forwarded")
            .expect("second frame should be ok");
        assert_eq!(second_chunk, second);
        assert!(stream.next().await.is_none());
    }
}
