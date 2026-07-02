use crate::{
    auth::{StoredAuth, TokenResponse, TokenStore},
    error::{ProxyError, Result},
    http_client::{build_client, duration_from_millis, HttpClientTuning},
};
use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::Value;
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

const REFRESH_MARGIN_MS: i64 = 5 * 60 * 1000;

#[async_trait]
pub trait TokenRefreshClient: Send + Sync {
    async fn refresh(&self, refresh_token: &str) -> Result<TokenResponse>;
}

#[derive(Clone)]
pub struct OAuthRefreshClient {
    issuer: String,
    client_id: String,
    client: reqwest::Client,
    timeout_ms: u64,
}

impl OAuthRefreshClient {
    pub fn new(issuer: impl Into<String>, client_id: impl Into<String>) -> Self {
        Self::with_timeout(issuer, client_id, crate::config::DEFAULT_HEADER_TIMEOUT_MS)
            .expect("default OAuth refresh client configuration should be valid")
    }

    pub fn with_timeout(
        issuer: impl Into<String>,
        client_id: impl Into<String>,
        timeout_ms: u64,
    ) -> Result<Self> {
        Ok(Self {
            issuer: issuer.into(),
            client_id: client_id.into(),
            client: build_client(HttpClientTuning {
                connect_timeout_ms: timeout_ms,
                pool_idle_timeout_ms: crate::config::DEFAULT_POOL_IDLE_TIMEOUT_MS,
                pool_max_idle_per_host: crate::config::DEFAULT_POOL_MAX_IDLE_PER_HOST,
                tcp_keepalive_ms: crate::config::DEFAULT_TCP_KEEPALIVE_MS,
            })?,
            timeout_ms,
        })
    }
}

#[async_trait]
impl TokenRefreshClient for OAuthRefreshClient {
    async fn refresh(&self, refresh_token: &str) -> Result<TokenResponse> {
        let response = tokio::time::timeout(
            duration_from_millis(self.timeout_ms),
            self.client
                .post(format!("{}/oauth/token", self.issuer.trim_end_matches('/')))
                .form(&[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", refresh_token),
                    ("client_id", self.client_id.as_str()),
                ])
                .send(),
        )
        .await
        .map_err(|_| ProxyError::Transport("timed out refreshing Codex OAuth token".into()))??;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ProxyError::Upstream {
                status,
                body,
                retry_after: None,
            });
        }
        Ok(response.json::<TokenResponse>().await?)
    }
}

#[derive(Clone)]
pub struct AuthManager {
    store: Arc<dyn TokenStore>,
    refresh_client: Arc<dyn TokenRefreshClient>,
    cached: Arc<Mutex<Option<StoredAuth>>>,
}

impl AuthManager {
    pub fn new(store: Arc<dyn TokenStore>, refresh_client: Arc<dyn TokenRefreshClient>) -> Self {
        Self {
            store,
            refresh_client,
            cached: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn get_auth(&self) -> Result<StoredAuth> {
        let mut guard = self.cached.lock().await;
        if guard.is_none() {
            *guard = self.store.load().await?;
        }
        let current = guard.clone().ok_or_else(|| {
            ProxyError::NotAuthenticated("run `cc-codex-proxy auth login` first".into())
        })?;
        if !current.is_expiring(now_ms(), REFRESH_MARGIN_MS) {
            return Ok(current);
        }
        let refreshed = self.refresh_current(&current).await?;
        *guard = Some(refreshed.clone());
        Ok(refreshed)
    }

    pub async fn force_refresh(&self) -> Result<StoredAuth> {
        let mut guard = self.cached.lock().await;
        if guard.is_none() {
            *guard = self.store.load().await?;
        }
        let current = guard.clone().ok_or_else(|| {
            ProxyError::NotAuthenticated("run `cc-codex-proxy auth login` first".into())
        })?;
        let refreshed = self.refresh_current(&current).await?;
        *guard = Some(refreshed.clone());
        Ok(refreshed)
    }

    pub async fn persist_initial(&self, tokens: TokenResponse) -> Result<StoredAuth> {
        tokens.validate_initial()?;
        let auth = stored_auth_from_token_response(tokens, None)?;
        self.store.save(&auth).await?;
        *self.cached.lock().await = Some(auth.clone());
        Ok(auth)
    }

    pub async fn status(&self) -> Result<Option<StoredAuth>> {
        let loaded = self.store.load().await?;
        *self.cached.lock().await = loaded.clone();
        Ok(loaded)
    }

    pub async fn logout(&self) -> Result<()> {
        self.store.clear().await?;
        *self.cached.lock().await = None;
        Ok(())
    }

    pub fn storage_label(&self) -> &'static str {
        self.store.label()
    }

    async fn refresh_current(&self, current: &StoredAuth) -> Result<StoredAuth> {
        let response = self.refresh_client.refresh(&current.refresh).await?;
        let auth = stored_auth_from_token_response(response, Some(current))?;
        self.store.save(&auth).await?;
        Ok(auth)
    }
}

fn stored_auth_from_token_response(
    response: TokenResponse,
    previous: Option<&StoredAuth>,
) -> Result<StoredAuth> {
    if response.access_token.is_empty() {
        return Err(ProxyError::InvalidRequest(
            "token response missing access_token".into(),
        ));
    }
    let refresh = response
        .refresh_token
        .clone()
        .or_else(|| previous.map(|auth| auth.refresh.clone()))
        .ok_or_else(|| ProxyError::InvalidRequest("token response missing refresh_token".into()))?;
    let account_id = extract_chatgpt_account_id(&response)
        .or_else(|| previous.and_then(|auth| auth.account_id.clone()));
    Ok(StoredAuth {
        access: response.access_token,
        refresh,
        expires_at_ms: now_ms() + response.expires_in.unwrap_or(3600) * 1000,
        account_id,
    })
}

pub fn extract_chatgpt_account_id(response: &TokenResponse) -> Option<String> {
    response
        .id_token
        .as_deref()
        .and_then(extract_account_from_jwt)
        .or_else(|| extract_account_from_jwt(&response.access_token))
}

fn extract_account_from_jwt(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let value = serde_json::from_slice::<Value>(&decoded).ok()?;
    value
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .or_else(|| value.get("chatgpt_account_id").and_then(Value::as_str))
        .map(ToOwned::to_owned)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::MemoryTokenStore;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingRefresh {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl TokenRefreshClient for CountingRefresh {
        async fn refresh(&self, _: &str) -> Result<TokenResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(TokenResponse {
                access_token: "new-access".into(),
                refresh_token: Some("new-refresh".into()),
                expires_in: Some(3600),
                id_token: None,
                extra: Default::default(),
            })
        }
    }

    #[tokio::test]
    async fn refreshes_expiring_token_once_under_lock() {
        let store = MemoryTokenStore::with(StoredAuth {
            access: "old".into(),
            refresh: "refresh".into(),
            expires_at_ms: 1,
            account_id: None,
        });
        let refresh = Arc::new(CountingRefresh {
            calls: AtomicUsize::new(0),
        });
        let manager = AuthManager::new(Arc::new(store), refresh.clone());
        let a = manager.get_auth().await.unwrap();
        let b = manager.get_auth().await.unwrap();
        assert_eq!(a.access, "new-access");
        assert_eq!(b.access, "new-access");
        assert_eq!(refresh.calls.load(Ordering::SeqCst), 1);
    }
}
