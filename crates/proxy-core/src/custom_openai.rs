use crate::{
    config::CUSTOM_OPENAI_API_KEY_ENV,
    error::{ProxyError, Result},
};
use serde::Serialize;
use std::{
    env, fs,
    io::{ErrorKind, Write},
    path::Path,
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomOpenAIApiKeyStatus {
    pub configured: bool,
    pub source: Option<String>,
}

pub fn api_key_status(path: &Path) -> CustomOpenAIApiKeyStatus {
    if env_api_key().is_some() {
        return CustomOpenAIApiKeyStatus {
            configured: true,
            source: Some(CUSTOM_OPENAI_API_KEY_ENV.into()),
        };
    }
    match fs::read_to_string(path) {
        Ok(raw) if !raw.trim().is_empty() => CustomOpenAIApiKeyStatus {
            configured: true,
            source: Some("local api key file".into()),
        },
        _ => CustomOpenAIApiKeyStatus {
            configured: false,
            source: None,
        },
    }
}

pub fn store_api_key(path: &Path, api_key: &str) -> Result<()> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err(ProxyError::InvalidRequest(
            "Custom OpenAI API key cannot be empty".into(),
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    writeln!(file, "{api_key}")?;
    Ok(())
}

pub fn clear_api_key(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

pub(crate) fn resolve_api_key(path: &Path) -> Result<Option<String>> {
    if let Some(key) = env_api_key() {
        return Ok(Some(key));
    }
    match fs::read_to_string(path) {
        Ok(raw) => {
            let key = raw.trim().to_string();
            Ok((!key.is_empty()).then_some(key))
        }
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn env_api_key() -> Option<String> {
    env::var(CUSTOM_OPENAI_API_KEY_ENV)
        .ok()
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
}

pub fn responses_url(base_url: &str) -> Result<String> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(ProxyError::Config(
            "custom OpenAI base URL is required; set --custom-openai-base-url or CCP_CUSTOM_OPENAI_BASE_URL".into(),
        ));
    }
    if base.ends_with("/responses") {
        return Ok(base.to_string());
    }
    if base.ends_with("/v1") {
        return Ok(format!("{base}/responses"));
    }
    Ok(format!("{base}/v1/responses"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_url_accepts_base_or_full_endpoint() {
        assert_eq!(
            responses_url("http://localhost:8000").unwrap(),
            "http://localhost:8000/v1/responses"
        );
        assert_eq!(
            responses_url("http://localhost:8000/v1").unwrap(),
            "http://localhost:8000/v1/responses"
        );
        assert_eq!(
            responses_url("http://localhost:8000/custom/responses").unwrap(),
            "http://localhost:8000/custom/responses"
        );
    }

    #[test]
    fn stores_custom_api_key_with_private_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("custom-openai-api-key");
        store_api_key(&path, "secret").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap().trim(), "secret");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}
