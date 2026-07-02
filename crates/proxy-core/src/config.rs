use crate::error::{ProxyError, Result};
use rand::{distributions::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    str::FromStr,
};

pub const APP_NAME: &str = "CCCodexProxy";
pub const DEFAULT_PORT: u16 = 18765;
pub const DEFAULT_CODEX_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
pub const DEFAULT_DEEPSEEK_ENDPOINT: &str = "https://api.deepseek.com/anthropic";
pub const DEFAULT_CUSTOM_OPENAI_ENDPOINT: &str = "";
pub const DEFAULT_OAUTH_ISSUER: &str = "https://auth.openai.com";
pub const DEFAULT_CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const DEFAULT_ORIGINATOR: &str = "cc-codex-proxy";
pub const OAUTH_CALLBACK_PORT: u16 = 1455;
pub const OAUTH_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
pub const DEEPSEEK_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";
pub const CUSTOM_OPENAI_API_KEY_ENV: &str = "CUSTOM_OPENAI_API_KEY";
pub const DEFAULT_PUBLIC_PRIMARY_MODEL: &str = "gpt-5.5[1m]";
pub const DEFAULT_PUBLIC_SMALL_MODEL: &str = "gpt-5.4-mini[1m]";
pub const DEFAULT_DEEPSEEK_PUBLIC_PRIMARY_MODEL: &str = "deepseek-v4-pro[1m]";
pub const DEFAULT_DEEPSEEK_PUBLIC_SMALL_MODEL: &str = "deepseek-v4-flash";
pub const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 15_000;
pub const DEFAULT_POOL_IDLE_TIMEOUT_MS: u64 = 90_000;
pub const DEFAULT_POOL_MAX_IDLE_PER_HOST: usize = 16;
pub const DEFAULT_TCP_KEEPALIVE_MS: u64 = 60_000;
pub const DEFAULT_STREAM_IDLE_WARN_MS: u64 = 120_000;
pub const DEFAULT_STREAM_IDLE_TIMEOUT_MS: u64 = 0;
pub const DEFAULT_CLAUDE_COMPAT_DOWNSTREAM_IDLE_PING_MS: u64 = 10_000;
pub const DEFAULT_HEADER_TIMEOUT_MS: u64 = 60_000;
pub const DEFAULT_MESSAGES_BODY_LIMIT_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_SHUTDOWN_GRACE_PERIOD_MS: u64 = 10_000;
pub const DEFAULT_SESSION_PIN_TTL_SECONDS: u64 = 30 * 24 * 60 * 60;
pub const DEFAULT_MAX_PINNED_SESSIONS: usize = 4_096;

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub config_file: PathBuf,
    pub model_profiles_file: PathBuf,
    pub admin_token_file: PathBuf,
    pub claude_shim_file: PathBuf,
    pub auth_file: PathBuf,
    pub route_pins_file: PathBuf,
    pub deepseek_api_key_file: PathBuf,
    pub custom_openai_api_key_file: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let app_support = dirs::data_dir()
            .ok_or_else(|| {
                ProxyError::Config("cannot locate user application support directory".into())
            })?
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
            claude_shim_file: app_support.join("claude-shim.json"),
            auth_file: app_support.join("auth.json"),
            route_pins_file: app_support.join("route-pins.json"),
            deepseek_api_key_file: app_support.join("deepseek-api-key"),
            custom_openai_api_key_file: app_support.join("custom-openai-api-key"),
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

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    #[default]
    Codex,
    DeepSeek,
    #[serde(rename = "custom-openai", alias = "custom-open-ai")]
    CustomOpenAI,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Codex => "codex",
            Provider::DeepSeek => "deepseek",
            Provider::CustomOpenAI => "custom-openai",
        }
    }
}

impl FromStr for Provider {
    type Err = ProxyError;

    fn from_str(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "codex" => Ok(Self::Codex),
            "deepseek" | "deep-seek" => Ok(Self::DeepSeek),
            "custom-openai" | "custom_openai" | "openai-compatible" | "openai" | "custom" => {
                Ok(Self::CustomOpenAI)
            }
            other => Err(ProxyError::Config(format!(
                "unsupported provider \"{other}\"; expected codex, deepseek, or custom-openai"
            ))),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodexTransport {
    #[default]
    Auto,
    WebSocket,
    Http,
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
    pub connect_timeout_ms: u64,
    pub pool_idle_timeout_ms: u64,
    pub pool_max_idle_per_host: usize,
    pub tcp_keepalive_ms: u64,
    pub stream_idle_warn_ms: u64,
    pub stream_idle_timeout_ms: u64,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_CODEX_ENDPOINT.to_string(),
            oauth_issuer: DEFAULT_OAUTH_ISSUER.to_string(),
            oauth_client_id: DEFAULT_CODEX_CLIENT_ID.to_string(),
            originator: DEFAULT_ORIGINATOR.to_string(),
            user_agent: format!("{DEFAULT_ORIGINATOR}/{}", env!("CARGO_PKG_VERSION")),
            transport: CodexTransport::default(),
            previous_response_id: false,
            header_timeout_ms: DEFAULT_HEADER_TIMEOUT_MS,
            connect_timeout_ms: DEFAULT_CONNECT_TIMEOUT_MS,
            pool_idle_timeout_ms: DEFAULT_POOL_IDLE_TIMEOUT_MS,
            pool_max_idle_per_host: DEFAULT_POOL_MAX_IDLE_PER_HOST,
            tcp_keepalive_ms: DEFAULT_TCP_KEEPALIVE_MS,
            stream_idle_warn_ms: DEFAULT_STREAM_IDLE_WARN_MS,
            stream_idle_timeout_ms: DEFAULT_STREAM_IDLE_TIMEOUT_MS,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DeepSeekConfig {
    pub base_url: String,
    pub user_agent: String,
    pub header_timeout_ms: u64,
    pub connect_timeout_ms: u64,
    pub pool_idle_timeout_ms: u64,
    pub pool_max_idle_per_host: usize,
    pub tcp_keepalive_ms: u64,
    pub stream_idle_warn_ms: u64,
    pub stream_idle_timeout_ms: u64,
}

impl Default for DeepSeekConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_DEEPSEEK_ENDPOINT.to_string(),
            user_agent: format!("{DEFAULT_ORIGINATOR}/{}", env!("CARGO_PKG_VERSION")),
            header_timeout_ms: DEFAULT_HEADER_TIMEOUT_MS,
            connect_timeout_ms: DEFAULT_CONNECT_TIMEOUT_MS,
            pool_idle_timeout_ms: DEFAULT_POOL_IDLE_TIMEOUT_MS,
            pool_max_idle_per_host: DEFAULT_POOL_MAX_IDLE_PER_HOST,
            tcp_keepalive_ms: DEFAULT_TCP_KEEPALIVE_MS,
            stream_idle_warn_ms: DEFAULT_STREAM_IDLE_WARN_MS,
            stream_idle_timeout_ms: DEFAULT_STREAM_IDLE_TIMEOUT_MS,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CustomOpenAIProtocol {
    #[default]
    Responses,
    ChatCompletions,
}

impl CustomOpenAIProtocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            CustomOpenAIProtocol::Responses => "responses",
            CustomOpenAIProtocol::ChatCompletions => "chat-completions",
        }
    }
}

impl FromStr for CustomOpenAIProtocol {
    type Err = ProxyError;

    fn from_str(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "responses" | "response" => Ok(Self::Responses),
            "chat" | "chat-completions" | "chat_completions" | "completions" => {
                Ok(Self::ChatCompletions)
            }
            other => Err(ProxyError::Config(format!(
                "unsupported custom OpenAI protocol \"{other}\"; expected responses or chat-completions"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomOpenAIConfig {
    pub base_url: String,
    pub user_agent: String,
    pub header_timeout_ms: u64,
    pub protocol: CustomOpenAIProtocol,
    pub connect_timeout_ms: u64,
    pub pool_idle_timeout_ms: u64,
    pub pool_max_idle_per_host: usize,
    pub tcp_keepalive_ms: u64,
    pub stream_idle_warn_ms: u64,
    pub stream_idle_timeout_ms: u64,
}

impl Default for CustomOpenAIConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_CUSTOM_OPENAI_ENDPOINT.to_string(),
            user_agent: format!("{DEFAULT_ORIGINATOR}/{}", env!("CARGO_PKG_VERSION")),
            header_timeout_ms: DEFAULT_HEADER_TIMEOUT_MS,
            protocol: CustomOpenAIProtocol::Responses,
            connect_timeout_ms: DEFAULT_CONNECT_TIMEOUT_MS,
            pool_idle_timeout_ms: DEFAULT_POOL_IDLE_TIMEOUT_MS,
            pool_max_idle_per_host: DEFAULT_POOL_MAX_IDLE_PER_HOST,
            tcp_keepalive_ms: DEFAULT_TCP_KEEPALIVE_MS,
            stream_idle_warn_ms: DEFAULT_STREAM_IDLE_WARN_MS,
            stream_idle_timeout_ms: DEFAULT_STREAM_IDLE_TIMEOUT_MS,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SessionRoutingPolicy {
    #[default]
    PinOnFirstRequest,
    Immediate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct RouteProfileConfig {
    pub id: String,
    pub provider: Provider,
    pub primary_model: String,
    pub small_model: String,
    pub context_window: u32,
}

impl Default for RouteProfileConfig {
    fn default() -> Self {
        Self {
            id: "codex".into(),
            provider: Provider::Codex,
            primary_model: "gpt-5.5".into(),
            small_model: "gpt-5.4-mini".into(),
            context_window: 272_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct RoutingConfig {
    pub active_profile: String,
    pub session_policy: SessionRoutingPolicy,
    pub session_pin_ttl_seconds: u64,
    pub max_pinned_sessions: usize,
    pub persist_session_pins: bool,
    pub profiles: Vec<RouteProfileConfig>,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            active_profile: "codex".into(),
            session_policy: SessionRoutingPolicy::PinOnFirstRequest,
            session_pin_ttl_seconds: DEFAULT_SESSION_PIN_TTL_SECONDS,
            max_pinned_sessions: DEFAULT_MAX_PINNED_SESSIONS,
            persist_session_pins: true,
            profiles: default_route_profiles(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct ClaudeProxyConfig {
    pub stable_host: String,
    pub stable_port: u16,
    pub public_primary_model: String,
    pub public_small_model: String,
    pub auto_compact_window: u32,
    pub downstream_idle_ping_ms: u64,
}

impl Default for ClaudeProxyConfig {
    fn default() -> Self {
        Self {
            stable_host: "127.0.0.1".into(),
            stable_port: DEFAULT_PORT,
            public_primary_model: DEFAULT_PUBLIC_PRIMARY_MODEL.into(),
            public_small_model: DEFAULT_PUBLIC_SMALL_MODEL.into(),
            auto_compact_window: 272_000,
            downstream_idle_ping_ms: DEFAULT_CLAUDE_COMPAT_DOWNSTREAM_IDLE_PING_MS,
        }
    }
}

pub fn default_route_profiles() -> Vec<RouteProfileConfig> {
    vec![
        RouteProfileConfig {
            id: "codex".into(),
            provider: Provider::Codex,
            primary_model: "gpt-5.5".into(),
            small_model: "gpt-5.4-mini".into(),
            context_window: 272_000,
        },
        RouteProfileConfig {
            id: "deepseek".into(),
            provider: Provider::DeepSeek,
            primary_model: "deepseek-v4-pro".into(),
            small_model: "deepseek-v4-flash".into(),
            context_window: 1_000_000,
        },
        RouteProfileConfig {
            id: "custom-openai".into(),
            provider: Provider::CustomOpenAI,
            primary_model: "gpt-5.5".into(),
            small_model: "gpt-5.4-mini".into(),
            context_window: 128_000,
        },
    ]
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    pub stderr: bool,
    pub verbose: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub port: u16,
    pub admin_token: String,
    pub provider: Provider,
    pub routing: RoutingConfig,
    pub claude: ClaudeProxyConfig,
    pub codex: CodexConfig,
    pub deepseek: DeepSeekConfig,
    pub custom_openai: CustomOpenAIConfig,
    pub log: LogConfig,
    pub messages_body_limit_bytes: usize,
    pub shutdown_grace_period_ms: u64,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            admin_token: String::new(),
            provider: Provider::Codex,
            routing: RoutingConfig::default(),
            claude: ClaudeProxyConfig::default(),
            codex: CodexConfig::default(),
            deepseek: DeepSeekConfig::default(),
            custom_openai: CustomOpenAIConfig::default(),
            log: LogConfig::default(),
            messages_body_limit_bytes: DEFAULT_MESSAGES_BODY_LIMIT_BYTES,
            shutdown_grace_period_ms: DEFAULT_SHUTDOWN_GRACE_PERIOD_MS,
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

    pub fn active_provider(&self) -> Result<Provider> {
        self.routing
            .profiles
            .iter()
            .find(|profile| profile.id == self.routing.active_profile)
            .map(|profile| profile.provider)
            .ok_or_else(|| {
                ProxyError::Config(format!(
                    "active route profile \"{}\" is not configured",
                    self.routing.active_profile
                ))
            })
    }

    fn apply_env(&mut self) {
        if let Ok(port) = env::var("PORT") {
            if let Ok(port) = port.parse::<u16>() {
                self.port = port;
                self.claude.stable_port = port;
            }
        }
        if let Ok(url) = env::var("CCP_CODEX_BASE_URL") {
            self.codex.base_url = url;
        }
        if let Ok(provider) = env::var("CCP_PROVIDER") {
            if let Ok(provider) = provider.parse::<Provider>() {
                self.provider = provider;
                self.routing.active_profile = provider.as_str().into();
            }
        }
        if let Ok(url) = env::var("CCP_DEEPSEEK_BASE_URL") {
            self.deepseek.base_url = url;
        }
        if let Ok(url) = env::var("CCP_CUSTOM_OPENAI_BASE_URL") {
            self.custom_openai.base_url = url;
        }
        if let Ok(protocol) = env::var("CCP_CUSTOM_OPENAI_PROTOCOL") {
            if let Ok(protocol) = protocol.parse::<CustomOpenAIProtocol>() {
                self.custom_openai.protocol = protocol;
            }
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
        if let Some(value) = env_u64("OPENAI_UPSTREAM_IDLE_WARN_MS") {
            self.codex.stream_idle_warn_ms = value;
            self.custom_openai.stream_idle_warn_ms = value;
        }
        if let Some(value) = env_u64("OPENAI_UPSTREAM_IDLE_ABORT_MS") {
            self.codex.stream_idle_timeout_ms = value;
            self.custom_openai.stream_idle_timeout_ms = value;
        }
        if let Some(value) = env_u64("CLAUDE_COMPAT_DOWNSTREAM_IDLE_PING_MS") {
            self.claude.downstream_idle_ping_ms = value;
        }
    }
}

fn env_u64(name: &str) -> Option<u64> {
    env::var(name).ok()?.parse::<u64>().ok()
}

fn truthy(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
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

    #[test]
    fn default_routing_uses_stable_profiles() {
        let config = AppConfig::default();
        assert_eq!(config.routing.active_profile, "codex");
        assert!(config
            .routing
            .profiles
            .iter()
            .any(|profile| profile.id == "deepseek" && profile.provider == Provider::DeepSeek));
        assert_eq!(
            config.claude.public_primary_model,
            DEFAULT_PUBLIC_PRIMARY_MODEL
        );
    }
}
