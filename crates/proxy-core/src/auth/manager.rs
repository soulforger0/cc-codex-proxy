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
    cached: Arc<Mutex<CachedAuth>>,
}

#[derive(Default)]
struct CachedAuth {
    current: Option<StoredAuth>,
    requires_login: bool,
}

const LOGIN_REQUIRED: &str = "ChatGPT session has expired or been revoked. Sign in again in CC Codex Proxy, or run `cc-codex-proxy auth login`.";

impl AuthManager {
    pub fn new(store: Arc<dyn TokenStore>, refresh_client: Arc<dyn TokenRefreshClient>) -> Self {
        Self {
            store,
            refresh_client,
            cached: Arc::new(Mutex::new(CachedAuth::default())),
        }
    }

    pub async fn get_auth(&self) -> Result<StoredAuth> {
        self.resolve_auth(None).await
    }

    pub async fn force_refresh(&self) -> Result<StoredAuth> {
        let current = self
            .status()
            .await?
            .ok_or_else(|| ProxyError::NotAuthenticated(LOGIN_REQUIRED.into()))?;
        self.refresh_after_rejection(&current).await
    }

    /// Reuse credentials already replaced by a concurrent refresh or re-login.
    pub async fn refresh_after_rejection(&self, rejected: &StoredAuth) -> Result<StoredAuth> {
        self.resolve_auth(Some(rejected)).await
    }

    async fn resolve_auth(&self, rejected: Option<&StoredAuth>) -> Result<StoredAuth> {
        let mut guard = self.cached.lock().await;
        self.reload(&mut guard).await?;
        if guard.requires_login {
            return Err(ProxyError::NotAuthenticated(LOGIN_REQUIRED.into()));
        }
        let current = guard.current.clone().ok_or_else(|| {
            ProxyError::NotAuthenticated("run `cc-codex-proxy auth login` first".into())
        })?;
        let force = rejected.is_some_and(|rejected| rejected == &current);
        if !force && !current.is_expiring(now_ms(), REFRESH_MARGIN_MS) {
            return Ok(current);
        }
        let refreshed = match self.refresh_current(&current).await {
            Ok(auth) => auth,
            Err(error) if refresh_requires_login(&error) => {
                guard.requires_login = true;
                return Err(ProxyError::NotAuthenticated(LOGIN_REQUIRED.into()));
            }
            Err(error) => return Err(error),
        };
        guard.current = Some(refreshed.clone());
        Ok(refreshed)
    }

    async fn reload(&self, cached: &mut CachedAuth) -> Result<()> {
        let loaded = self.store.load().await?;
        if cached.current != loaded {
            cached.current = loaded;
            cached.requires_login = false;
        }
        Ok(())
    }

    pub async fn persist_initial(&self, tokens: TokenResponse) -> Result<StoredAuth> {
        tokens.validate_initial()?;
        let auth = stored_auth_from_token_response(tokens, None)?;
        let mut guard = self.cached.lock().await;
        self.store.save(&auth).await?;
        *guard = CachedAuth {
            current: Some(auth.clone()),
            requires_login: false,
        };
        Ok(auth)
    }

    pub async fn status(&self) -> Result<Option<StoredAuth>> {
        let mut guard = self.cached.lock().await;
        self.reload(&mut guard).await?;
        Ok((!guard.requires_login)
            .then(|| guard.current.clone())
            .flatten())
    }

    pub async fn logout(&self) -> Result<()> {
        let mut guard = self.cached.lock().await;
        self.store.clear().await?;
        *guard = CachedAuth::default();
        Ok(())
    }

    pub fn storage_label(&self) -> &'static str {
        self.store.label()
    }

    async fn refresh_current(&self, current: &StoredAuth) -> Result<StoredAuth> {
        let response = self.refresh_client.refresh(&current.refresh).await;
        // A browser login or logout may complete while the refresh is in flight.
        // Do not overwrite those credentials with a result for the old session.
        let stored = self.store.load().await?.ok_or_else(|| {
            ProxyError::NotAuthenticated("run `cc-codex-proxy auth login` first".into())
        })?;
        if stored != *current {
            return Ok(stored);
        }
        let response = response?;
        let auth = stored_auth_from_token_response(response, Some(current))?;
        self.store.save(&auth).await?;
        Ok(auth)
    }
}

fn refresh_requires_login(error: &ProxyError) -> bool {
    let ProxyError::Upstream { status, body, .. } = error else {
        return false;
    };
    if !matches!(status.as_u16(), 400 | 401 | 403) {
        return false;
    }
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return *status == http::StatusCode::UNAUTHORIZED;
    };
    let code = value
        .pointer("/error/code")
        .and_then(Value::as_str)
        .or_else(|| value.get("error").and_then(Value::as_str));
    *status == http::StatusCode::UNAUTHORIZED
        || matches!(
            code,
            Some(
                "invalid_grant"
                    | "refresh_token_expired"
                    | "refresh_token_reused"
                    | "refresh_token_invalidated"
            )
        )
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

    fn valid_auth(access: &str) -> StoredAuth {
        StoredAuth {
            access: access.into(),
            refresh: "refresh".into(),
            expires_at_ms: i64::MAX,
            account_id: None,
        }
    }

    #[tokio::test]
    async fn reloads_login_and_logout_from_another_process() {
        let store = Arc::new(MemoryTokenStore::with(valid_auth("old")));
        let refresh = Arc::new(CountingRefresh {
            calls: AtomicUsize::new(0),
        });
        let manager = AuthManager::new(store.clone(), refresh.clone());
        assert_eq!(manager.get_auth().await.unwrap().access, "old");
        store.save(&valid_auth("new-login")).await.unwrap();
        assert_eq!(manager.get_auth().await.unwrap().access, "new-login");
        store.clear().await.unwrap();
        assert!(matches!(
            manager.get_auth().await,
            Err(ProxyError::NotAuthenticated(_))
        ));
        assert_eq!(refresh.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn concurrent_rejections_only_refresh_the_rejected_credentials_once() {
        let old = valid_auth("old");
        let refresh = Arc::new(CountingRefresh {
            calls: AtomicUsize::new(0),
        });
        let manager = AuthManager::new(
            Arc::new(MemoryTokenStore::with(old.clone())),
            refresh.clone(),
        );
        let results =
            futures_util::future::join_all((0..16).map(|_| manager.refresh_after_rejection(&old)))
                .await;
        assert!(results
            .into_iter()
            .all(|result| result.unwrap().access == "new-access"));
        assert_eq!(refresh.calls.load(Ordering::SeqCst), 1);
    }

    struct FailingRefresh {
        calls: AtomicUsize,
        status: http::StatusCode,
        body: &'static str,
    }

    #[async_trait]
    impl TokenRefreshClient for FailingRefresh {
        async fn refresh(&self, _: &str) -> Result<TokenResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(ProxyError::Upstream {
                status: self.status,
                body: self.body.into(),
                retry_after: None,
            })
        }
    }

    #[tokio::test]
    async fn expired_refresh_token_requires_login_and_recovers_after_external_login() {
        let old = StoredAuth {
            expires_at_ms: 1,
            ..valid_auth("old")
        };
        let store = Arc::new(MemoryTokenStore::with(old));
        let refresh = Arc::new(FailingRefresh {
            calls: AtomicUsize::new(0),
            status: http::StatusCode::UNAUTHORIZED,
            body: r#"{"error":{"code":"refresh_token_expired","message":"Your session has expired. Please log in again."}}"#,
        });
        let manager = AuthManager::new(store.clone(), refresh.clone());
        for _ in 0..3 {
            let error = manager.get_auth().await.unwrap_err();
            assert!(matches!(error, ProxyError::NotAuthenticated(_)));
            assert!(error.to_string().contains("cc-codex-proxy auth login"));
            assert_eq!(manager.status().await.unwrap(), None);
        }
        assert_eq!(refresh.calls.load(Ordering::SeqCst), 1);
        store.save(&valid_auth("new-login")).await.unwrap();
        assert_eq!(manager.get_auth().await.unwrap().access, "new-login");
        assert!(manager.status().await.unwrap().is_some());
    }

    #[tokio::test]
    async fn transient_refresh_failure_does_not_require_relogin() {
        let refresh = Arc::new(FailingRefresh {
            calls: AtomicUsize::new(0),
            status: http::StatusCode::SERVICE_UNAVAILABLE,
            body: "temporarily unavailable",
        });
        let manager = AuthManager::new(
            Arc::new(MemoryTokenStore::with(StoredAuth {
                expires_at_ms: 1,
                ..valid_auth("old")
            })),
            refresh.clone(),
        );
        for _ in 0..2 {
            assert!(matches!(
                manager.get_auth().await,
                Err(ProxyError::Upstream { .. })
            ));
            assert!(manager.status().await.unwrap().is_some());
        }
        assert_eq!(refresh.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn invalid_grant_is_a_permanent_failure_but_invalid_client_is_not() {
        for (code, expected) in [
            ("invalid_grant", true),
            ("refresh_token_reused", true),
            ("refresh_token_invalidated", true),
            ("invalid_client", false),
        ] {
            let error = ProxyError::Upstream {
                status: http::StatusCode::BAD_REQUEST,
                body: serde_json::json!({"error":code}).to_string(),
                retry_after: None,
            };
            assert_eq!(refresh_requires_login(&error), expected, "{code}");
        }
    }
}
