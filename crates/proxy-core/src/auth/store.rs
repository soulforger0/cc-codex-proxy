use crate::error::{ProxyError, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredAuth {
    pub access: String,
    pub refresh: String,
    pub expires_at_ms: i64,
    pub account_id: Option<String>,
}

impl StoredAuth {
    pub fn is_expiring(&self, now_ms: i64, margin_ms: i64) -> bool {
        self.expires_at_ms - margin_ms <= now_ms
    }
}

#[async_trait]
pub trait TokenStore: Send + Sync {
    async fn load(&self) -> Result<Option<StoredAuth>>;
    async fn save(&self, auth: &StoredAuth) -> Result<()>;
    async fn clear(&self) -> Result<()>;
    fn label(&self) -> &'static str;
}

#[derive(Debug, Clone)]
pub struct KeychainTokenStore {
    service: String,
    account: String,
}

impl Default for KeychainTokenStore {
    fn default() -> Self {
        Self {
            service: "CCCodexProxy.codex".to_string(),
            account: "auth".to_string(),
        }
    }
}

impl KeychainTokenStore {
    pub fn new(service: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            account: account.into(),
        }
    }
}

#[async_trait]
impl TokenStore for KeychainTokenStore {
    async fn load(&self) -> Result<Option<StoredAuth>> {
        let entry = keyring::Entry::new(&self.service, &self.account)
            .map_err(|err| ProxyError::Config(format!("cannot open Keychain entry: {err}")))?;
        match entry.get_password() {
            Ok(raw) => Ok(Some(serde_json::from_str(&raw)?)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(ProxyError::Config(format!(
                "cannot read Keychain token: {err}"
            ))),
        }
    }

    async fn save(&self, auth: &StoredAuth) -> Result<()> {
        let entry = keyring::Entry::new(&self.service, &self.account)
            .map_err(|err| ProxyError::Config(format!("cannot open Keychain entry: {err}")))?;
        entry
            .set_password(&serde_json::to_string(auth)?)
            .map_err(|err| ProxyError::Config(format!("cannot write Keychain token: {err}")))?;
        Ok(())
    }

    async fn clear(&self) -> Result<()> {
        let entry = keyring::Entry::new(&self.service, &self.account)
            .map_err(|err| ProxyError::Config(format!("cannot open Keychain entry: {err}")))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(ProxyError::Config(format!(
                "cannot delete Keychain token: {err}"
            ))),
        }
    }

    fn label(&self) -> &'static str {
        "macOS Keychain"
    }
}

#[derive(Debug, Clone, Default)]
pub struct MemoryTokenStore {
    inner: Arc<Mutex<Option<StoredAuth>>>,
}

impl MemoryTokenStore {
    pub fn with(auth: StoredAuth) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Some(auth))),
        }
    }
}

#[async_trait]
impl TokenStore for MemoryTokenStore {
    async fn load(&self) -> Result<Option<StoredAuth>> {
        Ok(self.inner.lock().await.clone())
    }

    async fn save(&self, auth: &StoredAuth) -> Result<()> {
        *self.inner.lock().await = Some(auth.clone());
        Ok(())
    }

    async fn clear(&self) -> Result<()> {
        *self.inner.lock().await = None;
        Ok(())
    }

    fn label(&self) -> &'static str {
        "memory"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_margin_is_applied() {
        let auth = StoredAuth {
            access: "a".into(),
            refresh: "r".into(),
            expires_at_ms: 10_000,
            account_id: None,
        };
        assert!(auth.is_expiring(6_000, 5_000));
        assert!(!auth.is_expiring(4_000, 5_000));
    }
}
