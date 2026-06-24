use crate::error::{ProxyError, Result};
use rand::{distributions::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use std::{
    env,
    fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

pub const APP_NAME: &str = "CCCodexProxy";
pub const DEFAULT_PORT: u16 = 18765;
pub const DEFAULT_CODEX_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
pub const DEFAULT_OAUTH_ISSUER: &str = "https://auth.openai.com";
pub const DEFAULT_CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const DEFAULT_ORIGINATOR: &str = "cc-codex-proxy";
pub const OAUTH_CALLBACK_PORT: u16 = 1455;
pub const OAUTH_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub config_file: PathBuf,
    pub model_profiles_file: PathBuf,
    pub admin_token_file: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let app_support = dirs::data_dir()
            .ok_or_else(|| ProxyError::Config("cannot locate user application support directory".into()))?
            .join(APP_NAME);
        let logs_dir = dirs::home_dir()
            .ok_or_else(|| ProxyError::Config("cannot locate home directory".into()))?
            .join("Library")
            .join("Logs")
            .join(APP_NAME);
        Ok(Self {
            config_file: app_support.join("config.json"),
            model_profiles_file: app_support.join("model-profiles.json"),
            admin_token_file: app_support.join("admin-token"),
            config_dir: app_support,
            logs_dir,
        })
    }

    pub fn ensure(&self) -> Result<()> {
        fs::create_dir_all(&self.config_dir)?;
        fs::create_dir_all(&self.logs_dir)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodexTransport {
    Auto,
    WebSocket,
    Http,
}

impl Default for CodexTransport {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CodexConfig {
    pub base_url: String,
    pub oauth_issuer: String,
    pub oauth_client_id: String,
    pub originator: String,
    pub user_agent: String,
    pub transport: CodexTransport,
    pub previous_response_id: bool,
    pub header_timeout_ms: u64,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_CODEX_ENDPOINT.to_string(),
            oauth_issuer: DEFAULT_OAUTH_ISSUER.to_string(),
            oauth_client_id: DEFAULT_CODEX_CLIENT_ID.to_string(),
            originator: DEFAULT_ORIGINATOR.to_string(),
            user_agent: format!("{DEFAULT_ORIGINATOR}/{}", env!("CARGO_PKG_VERSION")),
            transport: CodexTransport::Auto,
            previous_response_id: false,
            header_timeout_ms: 60_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    pub stderr: bool,
    pub verbose: bool,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            stderr: false,
            verbose: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub port: u16,
    pub admin_token: String,
    pub codex: CodexConfig,
    pub log: LogConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            admin_token: String::new(),
            codex: CodexConfig::default(),
            log: LogConfig::default(),
        }
    }
}

impl AppConfig {
    pub fn load(paths: &AppPaths) -> Result<Self> {
        paths.ensure()?;
        let mut cfg = match fs::read_to_string(&paths.config_file) {
            Ok(raw) => serde_json::from_str::<AppConfig>(&raw)?,
            Err(err) if err.kind() == ErrorKind::NotFound => AppConfig::default(),
            Err(err) => return Err(err.into()),
        };
        cfg.apply_env();
        cfg.admin_token = ensure_admin_token(&paths.admin_token_file)?;
        Ok(cfg)
    }

    pub fn load_default() -> Result<(Self, AppPaths)> {
        let paths = AppPaths::discover()?;
        let cfg = Self::load(&paths)?;
        Ok((cfg, paths))
    }

    fn apply_env(&mut self) {
        if let Ok(port) = env::var("PORT") {
            if let Ok(port) = port.parse::<u16>() {
                self.port = port;
            }
        }
        if let Ok(url) = env::var("CCP_CODEX_BASE_URL") {
            self.codex.base_url = url;
        }
        if let Ok(value) = env::var("CCP_LOG_STDERR") {
            self.log.stderr = truthy(&value);
        }
        if let Ok(value) = env::var("CCP_LOG_VERBOSE") {
            self.log.verbose = truthy(&value);
        }
        if let Ok(value) = env::var("CCP_CODEX_TRANSPORT") {
            self.codex.transport = match value.to_ascii_lowercase().as_str() {
                "websocket" | "ws" => CodexTransport::WebSocket,
                "http" | "sse" => CodexTransport::Http,
                _ => CodexTransport::Auto,
            };
        }
    }
}

fn truthy(value: &str) -> bool {
    matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

pub fn ensure_admin_token(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(token) if !token.trim().is_empty() => return Ok(token.trim().to_string()),
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    let token: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(48)
        .map(char::from)
        .collect();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    writeln!(file, "{token}")?;
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_token_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("admin-token");
        let first = ensure_admin_token(&path).unwrap();
        let second = ensure_admin_token(&path).unwrap();
        assert_eq!(first, second);
        assert!(first.len() >= 48);
    }
}

