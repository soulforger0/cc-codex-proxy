use crate::{
    anthropic::schema::AnthropicRequest,
    codex::client::ByteStream,
    config::{DeepSeekConfig, DEEPSEEK_API_KEY_ENV},
    error::{ProxyError, Result},
    model::ResolvedModel,
};
use futures_util::TryStreamExt;
use http::StatusCode;
use serde::Serialize;
use serde_json::Value;
use std::{
    env, fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use tracing::{info, warn};

const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct DeepSeekResponse {
    pub body: ByteStream,
    pub status: StatusCode,
}

#[derive(Clone)]
pub struct DeepSeekClient {
    http: reqwest::Client,
    config: DeepSeekConfig,
    api_key_file: PathBuf,
}

impl DeepSeekClient {
    pub fn new(config: DeepSeekConfig, api_key_file: PathBuf) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()?;
        Ok(Self {
            http,
            config,
            api_key_file,
        })
    }

    pub fn api_key_status(&self) -> DeepSeekApiKeyStatus {
        api_key_status(&self.api_key_file)
    }

    pub async fn post(
        &self,
        request: &AnthropicRequest,
        resolved: &ResolvedModel,
    ) -> Result<DeepSeekResponse> {
        validate_deepseek_request(request)?;
        let api_key = resolve_api_key(&self.api_key_file)?;
        let mut body = request.clone();
        body.model = resolved.upstream_model.clone();
        normalize_deepseek_effort(&mut body);
        let url = messages_url(&self.config.base_url);
        info!(
            model = %body.model,
            stream = body.wants_stream(),
            message_count = body.messages.len(),
            "posting DeepSeek Anthropic request"
        );

        let response = tokio::time::timeout(
            Duration::from_millis(self.config.header_timeout_ms),
            self.http
                .post(url)
                .headers(self.headers(&api_key)?)
                .json(&body)
                .send(),
        )
        .await
        .map_err(|_| {
            ProxyError::Transport("timed out waiting for DeepSeek response headers".into())
        })??;
        let status =
            StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        info!(%status, "received DeepSeek response headers");
        if !status.is_success() {
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .map(ToOwned::to_owned);
            let body = response.text().await.unwrap_or_default();
            warn!(
                %status,
                retry_after = ?retry_after,
                upstream_body = %truncate_for_log(&body, 4_000),
                "DeepSeek request failed"
            );
            return Err(ProxyError::Upstream {
                status,
                body,
                retry_after,
            });
        }

        Ok(DeepSeekResponse {
            body: Box::pin(response.bytes_stream().map_err(ProxyError::from)),
            status,
        })
    }

    fn headers(&self, api_key: &str) -> Result<reqwest::header::HeaderMap> {
        let mut headers = reqwest::header::HeaderMap::new();
        insert_header(
            &mut headers,
            reqwest::header::CONTENT_TYPE,
            "application/json",
        )?;
        insert_header(&mut headers, reqwest::header::ACCEPT, "*/*")?;
        insert_header(&mut headers, "x-api-key", api_key)?;
        insert_header(&mut headers, "anthropic-version", ANTHROPIC_VERSION)?;
        insert_header(
            &mut headers,
            reqwest::header::USER_AGENT,
            &self.config.user_agent,
        )?;
        Ok(headers)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepSeekApiKeyStatus {
    pub configured: bool,
    pub source: Option<String>,
}

pub fn api_key_status(path: &Path) -> DeepSeekApiKeyStatus {
    if env_api_key().is_some() {
        return DeepSeekApiKeyStatus {
            configured: true,
            source: Some(DEEPSEEK_API_KEY_ENV.into()),
        };
    }
    match fs::read_to_string(path) {
        Ok(raw) if !raw.trim().is_empty() => DeepSeekApiKeyStatus {
            configured: true,
            source: Some("local api key file".into()),
        },
        _ => DeepSeekApiKeyStatus {
            configured: false,
            source: None,
        },
    }
}

pub fn store_api_key(path: &Path, api_key: &str) -> Result<()> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err(ProxyError::InvalidRequest(
            "DeepSeek API key cannot be empty".into(),
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

fn resolve_api_key(path: &Path) -> Result<String> {
    if let Some(key) = env_api_key() {
        return Ok(key);
    }
    match fs::read_to_string(path) {
        Ok(raw) if !raw.trim().is_empty() => Ok(raw.trim().to_string()),
        Ok(_) => Err(ProxyError::NotAuthenticated(
            "set a DeepSeek API key with `cc-codex-proxy auth set-api-key --provider deepseek --stdin` or DEEPSEEK_API_KEY".into(),
        )),
        Err(err) if err.kind() == ErrorKind::NotFound => Err(ProxyError::NotAuthenticated(
            "set a DeepSeek API key with `cc-codex-proxy auth set-api-key --provider deepseek --stdin` or DEEPSEEK_API_KEY".into(),
        )),
        Err(err) => Err(err.into()),
    }
}

fn env_api_key() -> Option<String> {
    env::var(DEEPSEEK_API_KEY_ENV)
        .ok()
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
}

fn validate_deepseek_request(request: &AnthropicRequest) -> Result<()> {
    if let Some(system) = &request.system {
        reject_unsupported_content(system)?;
    }
    for message in &request.messages {
        reject_unsupported_content(&message.content)?;
    }
    Ok(())
}

fn normalize_deepseek_effort(request: &mut AnthropicRequest) {
    let Some(Value::Object(output_config)) = request.output_config.as_mut() else {
        return;
    };
    let Some(effort) = output_config
        .get("effort")
        .and_then(Value::as_str)
        .map(normalize_deepseek_effort_value)
    else {
        return;
    };
    output_config.insert("effort".into(), Value::String(effort.into()));
}

fn normalize_deepseek_effort_value(effort: &str) -> &'static str {
    match effort {
        "auto" => "auto",
        "max" | "ultracode" => "max",
        _ => "high",
    }
}

fn reject_unsupported_content(value: &Value) -> Result<()> {
    match value {
        Value::Array(items) => {
            for item in items {
                reject_unsupported_content(item)?;
            }
        }
        Value::Object(object) => {
            if let Some(kind) = object.get("type").and_then(Value::as_str) {
                if matches!(kind, "image" | "document") {
                    return Err(ProxyError::InvalidRequest(format!(
                        "DeepSeek Anthropic API does not support {kind} content blocks"
                    )));
                }
            }
            if let Some(content) = object.get("content") {
                reject_unsupported_content(content)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

fn insert_header<K>(headers: &mut reqwest::header::HeaderMap, name: K, value: &str) -> Result<()>
where
    K: reqwest::header::IntoHeaderName,
{
    let value = reqwest::header::HeaderValue::from_str(value).map_err(|err| {
        ProxyError::Config(format!("invalid header value for DeepSeek request: {err}"))
    })?;
    headers.insert(name, value);
    Ok(())
}

fn messages_url(base_url: &str) -> String {
    format!("{}/v1/messages", base_url.trim_end_matches('/'))
}

fn truncate_for_log(value: &str, max_chars: usize) -> String {
    let mut out = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        out.push_str("...[truncated]");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Provider;
    use crate::model::ModelRegistry;

    fn request_with_output_config(output_config: Option<Value>) -> AnthropicRequest {
        AnthropicRequest {
            model: "deepseek-v4-pro".into(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            stream: None,
            system: None,
            messages: vec![],
            tools: None,
            tool_choice: None,
            metadata: None,
            output_config,
            thinking: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn messages_url_appends_anthropic_messages_path() {
        assert_eq!(
            messages_url("https://api.deepseek.com/anthropic/"),
            "https://api.deepseek.com/anthropic/v1/messages"
        );
    }

    #[test]
    fn normalizes_deepseek_effort_to_supported_values() {
        for (input, expected) in [
            ("max", "max"),
            ("ultracode", "max"),
            ("auto", "auto"),
            ("low", "high"),
            ("medium", "high"),
            ("high", "high"),
            ("xhigh", "high"),
            ("minimal", "high"),
            ("none", "high"),
            ("other", "high"),
        ] {
            let mut request =
                request_with_output_config(Some(serde_json::json!({ "effort": input })));

            normalize_deepseek_effort(&mut request);

            assert_eq!(
                request.output_config.unwrap()["effort"],
                serde_json::json!(expected),
                "input effort {input}"
            );
        }
    }

    #[test]
    fn preserves_deepseek_output_config_without_string_effort() {
        let mut no_output_config = request_with_output_config(None);
        normalize_deepseek_effort(&mut no_output_config);
        assert!(no_output_config.output_config.is_none());

        let mut missing_effort =
            request_with_output_config(Some(serde_json::json!({ "format": { "type": "json" } })));
        normalize_deepseek_effort(&mut missing_effort);
        assert_eq!(
            missing_effort.output_config.unwrap(),
            serde_json::json!({ "format": { "type": "json" } })
        );

        let mut non_string_effort =
            request_with_output_config(Some(serde_json::json!({ "effort": 3 })));
        normalize_deepseek_effort(&mut non_string_effort);
        assert_eq!(
            non_string_effort.output_config.unwrap(),
            serde_json::json!({ "effort": 3 })
        );
    }

    #[test]
    fn rejects_image_blocks_before_forwarding() {
        let request = AnthropicRequest {
            model: "deepseek-v4-pro".into(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            stream: None,
            system: None,
            messages: vec![crate::anthropic::schema::AnthropicMessage {
                role: "user".into(),
                content: serde_json::json!([
                    {
                        "type": "image",
                        "source": {"type": "url", "url": "https://example.test/image.png"}
                    }
                ]),
                extra: Default::default(),
            }],
            tools: None,
            tool_choice: None,
            metadata: None,
            output_config: None,
            thinking: None,
            extra: Default::default(),
        };
        let err = validate_deepseek_request(&request).unwrap_err();
        assert!(err.to_string().contains("does not support image"));
    }

    #[test]
    fn stores_api_key_with_private_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deepseek-api-key");
        store_api_key(&path, "secret").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap().trim(), "secret");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn deepseek_model_resolution_rewrites_context_hint() {
        let registry = ModelRegistry::from_profiles(crate::model::default_profiles());
        let resolved = registry
            .resolve(Provider::DeepSeek, "deepseek-v4-pro[1m]")
            .unwrap();
        assert_eq!(resolved.upstream_model, "deepseek-v4-pro");
    }
}
