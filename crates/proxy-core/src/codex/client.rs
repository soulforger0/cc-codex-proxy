use crate::{
    auth::AuthManager,
    codex::translate::ResponsesRequest,
    config::{CodexConfig, CodexTransport},
    error::{ProxyError, Result},
};
use bytes::Bytes;
use futures_util::{SinkExt, Stream, StreamExt};
use http::StatusCode;
use serde_json::json;
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, warn};

pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>;

const WEBSOCKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const WEBSOCKET_FAILURE_COOLDOWN: Duration = Duration::from_secs(120);

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
        debug!(model = %body.model, input_items = body.input.len(), "posting codex HTTP request");
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
        if !status.is_success() {
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .map(ToOwned::to_owned);
            let body = response.text().await.unwrap_or_default();
            return Err(ProxyError::Upstream {
                status,
                body,
                retry_after,
            });
        }
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
        let request = http::Request::builder()
            .method("GET")
            .uri(ws_url.as_str())
            .header("authorization", format!("Bearer {access_token}"))
            .header("openai-beta", "responses=experimental")
            .header("originator", self.config.originator.as_str())
            .header("user-agent", self.config.user_agent.as_str());
        let request = if let Some(account_id) = account_id {
            request.header("ChatGPT-Account-Id", account_id)
        } else {
            request
        };
        let request = if let Some(session_id) = session_id {
            request
                .header("session_id", session_id)
                .header("x-client-request-id", session_id)
                .header("x-codex-window-id", format!("{session_id}:0"))
        } else {
            request
        };
        let request = request
            .body(())
            .map_err(|err| ProxyError::Transport(format!("bad websocket request: {err}")))?;
        let (mut socket, _) =
            tokio::time::timeout(WEBSOCKET_CONNECT_TIMEOUT, connect_async(request))
                .await
                .map_err(|_| ProxyError::Transport("timed out opening Codex websocket".into()))?
                .map_err(|err| {
                    ProxyError::Transport(format!("Codex websocket setup failed: {err}"))
                })?;
        let payload = json!({ "type": "responses.create", "request": body }).to_string();
        socket
            .send(Message::Text(payload.into()))
            .await
            .map_err(|err| ProxyError::Transport(format!("Codex websocket send failed: {err}")))?;
        let stream = socket.filter_map(|message| async move {
            match message {
                Ok(Message::Text(text)) => Some(Ok(Bytes::from(format!("data: {text}\n\n")))),
                Ok(Message::Binary(bytes)) => Some(Ok(Bytes::copy_from_slice(bytes.as_ref()))),
                Ok(Message::Close(_)) => None,
                Ok(_) => None,
                Err(err) => Some(Err(ProxyError::Transport(format!(
                    "Codex websocket read failed: {err}"
                )))),
            }
        });
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

#[derive(Debug, Default)]
struct TransportState {
    websocket_disabled_until: Mutex<Option<Instant>>,
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
    fn websocket_cooldown_expires() {
        let state = TransportState::default();
        state.record_websocket_failure(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(5));
        assert!(state.websocket_cooldown_remaining().is_none());
    }
}
