use crate::error::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{ErrorKind, Write},
    path::PathBuf,
    sync::Arc,
};
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
pub struct FileTokenStore {
    path: PathBuf,
}

impl FileTokenStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

#[async_trait]
impl TokenStore for FileTokenStore {
    async fn load(&self) -> Result<Option<StoredAuth>> {
        match fs::read_to_string(&self.path) {
            Ok(raw) if raw.trim().is_empty() => Ok(None),
            Ok(raw) => Ok(Some(serde_json::from_str(&raw)?)),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    async fn save(&self, auth: &StoredAuth) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut options = fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&self.path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        writeln!(file, "{}", serde_json::to_string_pretty(auth)?)?;
        Ok(())
    }

    async fn clear(&self) -> Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    fn label(&self) -> &'static str {
        "local auth file"
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

    #[tokio::test]
    async fn file_store_round_trips_and_clears_auth() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let store = FileTokenStore::new(path.clone());
        let auth = StoredAuth {
            access: "access".into(),
            refresh: "refresh".into(),
            expires_at_ms: 123,
            account_id: Some("acct".into()),
        };

        store.save(&auth).await.unwrap();
        assert_eq!(store.load().await.unwrap(), Some(auth));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        store.clear().await.unwrap();
        assert_eq!(store.load().await.unwrap(), None);
    }
}
