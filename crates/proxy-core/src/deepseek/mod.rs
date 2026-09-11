use crate::{
    anthropic::schema::AnthropicRequest,
    canonical::canonicalize_anthropic_request,
    codex::client::ByteStream,
    config::{DeepSeekConfig, DEEPSEEK_API_KEY_ENV},
    error::{ProxyError, Result},
    http_client::{
        build_client, duration_from_millis, monitor_idle_stream, optional_duration_from_millis,
        HttpClientTuning,
    },
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
        let http = build_client(HttpClientTuning {
            connect_timeout_ms: config.connect_timeout_ms,
            pool_idle_timeout_ms: config.pool_idle_timeout_ms,
            pool_max_idle_per_host: config.pool_max_idle_per_host,
            tcp_keepalive_ms: config.tcp_keepalive_ms,
        })?;
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
        let mut body = request.clone();
        let replaced = substitute_unsupported_content(&mut body)?;
        if replaced > 0 {
            warn!(
                replaced,
                "replaced unsupported DeepSeek content blocks with text placeholders"
            );
        }
        let api_key = resolve_api_key(&self.api_key_file)?;
        canonicalize_anthropic_request(&mut body);
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
            duration_from_millis(self.config.header_timeout_ms),
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
            body: monitor_idle_stream(
                response.bytes_stream().map_err(ProxyError::from),
                "DeepSeek HTTP",
                None,
                duration_from_millis(self.config.stream_idle_warn_ms),
                optional_duration_from_millis(self.config.stream_idle_timeout_ms),
            ),
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

/// Replaces content blocks DeepSeek still rejects with a text placeholder so the
/// turn can be forwarded anyway, returning how many blocks were replaced.
///
/// Images are forwarded unchanged: DeepSeek's Anthropic-compatible API accepts
/// `image` blocks on its vision-capable models. `document` blocks remain
/// unsupported and become text.
///
/// A message whose content is nothing but unsupported blocks is rejected
/// instead: once the documents are gone the model would receive no user input at
/// all, which is worse than a loud error.
fn substitute_unsupported_content(request: &mut AnthropicRequest) -> Result<usize> {
    let mut replaced = 0;
    if let Some(system) = request.system.as_mut() {
        replace_unsupported_blocks(system, &mut replaced);
    }
    for message in &mut request.messages {
        reject_unsupported_only_message(&message.content)?;
        replace_unsupported_blocks(&mut message.content, &mut replaced);
    }
    Ok(replaced)
}

fn reject_unsupported_only_message(content: &Value) -> Result<()> {
    let Value::Array(items) = content else {
        return Ok(());
    };
    let mut unsupported: Vec<&str> = Vec::new();
    let mut has_forwardable = false;
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some(kind) if is_unsupported_kind(kind) => {
                if !unsupported.contains(&kind) {
                    unsupported.push(kind);
                }
            }
            _ => has_forwardable = true,
        }
    }
    if has_forwardable || unsupported.is_empty() {
        return Ok(());
    }
    Err(ProxyError::InvalidRequest(format!(
        "DeepSeek Anthropic API does not support {} content blocks, and this message contains no text, image, or tool content to forward",
        unsupported.join(" or ")
    )))
}

fn replace_unsupported_blocks(value: &mut Value, replaced: &mut usize) {
    match value {
        Value::Array(items) => {
            for item in items.iter_mut() {
                replace_unsupported_blocks(item, replaced);
            }
        }
        Value::Object(object) => {
            if let Some(kind) = object
                .get("type")
                .and_then(Value::as_str)
                .filter(|kind| is_unsupported_kind(kind))
            {
                *replaced += 1;
                *value = serde_json::json!({
                    "type": "text",
                    "text": unsupported_block_placeholder(kind),
                });
                return;
            }
            if let Some(content) = object.get_mut("content") {
                replace_unsupported_blocks(content, replaced);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// Content block types DeepSeek's Anthropic-compatible API still rejects.
/// `image` is deliberately absent: vision-capable DeepSeek models accept it.
fn is_unsupported_kind(kind: &str) -> bool {
    kind == "document"
}

fn unsupported_block_placeholder(kind: &str) -> String {
    format!(
        "[A note from the proxy: a {kind} content block was NOT forwarded - the DeepSeek \
         Anthropic API does not support {kind} blocks. Only text was sent. Tell the user this \
         {kind} could not be seen.]"
    )
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
        let mut request = request_with_messages(Vec::new());
        request.output_config = output_config;
        request
    }

    fn request_with_messages(
        messages: Vec<crate::anthropic::schema::AnthropicMessage>,
    ) -> AnthropicRequest {
        AnthropicRequest {
            model: "deepseek-flash".into(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            stream: None,
            system: None,
            messages,
            tools: None,
            tool_choice: None,
            metadata: None,
            output_config: None,
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
    fn forwards_image_blocks_unchanged() {
        let original = serde_json::json!([
            {"type": "text", "text": "what is in this picture?"},
            {
                "type": "image",
                "source": {"type": "base64", "media_type": "image/png", "data": "abc123"}
            }
        ]);
        let mut request = request_with_messages(vec![crate::anthropic::schema::AnthropicMessage {
            role: "user".into(),
            content: original.clone(),
            extra: Default::default(),
        }]);

        let replaced = substitute_unsupported_content(&mut request).unwrap();

        assert_eq!(replaced, 0);
        assert_eq!(request.messages[0].content, original);
    }

    #[test]
    fn replaces_document_blocks_and_keeps_nested_tool_result_images() {
        let mut request = request_with_messages(vec![crate::anthropic::schema::AnthropicMessage {
            role: "user".into(),
            content: serde_json::json!([
                {
                    "type": "document",
                    "source": {"type": "url", "url": "https://example.test/doc.pdf"}
                },
                {
                    "type": "tool_result",
                    "tool_use_id": "t1",
                    "content": [
                        {"type": "text", "text": "screen"},
                        {"type": "image", "source": {"type": "base64", "data": "abc"}}
                    ]
                }
            ]),
            extra: Default::default(),
        }]);

        let replaced = substitute_unsupported_content(&mut request).unwrap();

        assert_eq!(replaced, 1);
        let blocks = request.messages[0].content.as_array().unwrap();
        assert!(blocks[0]["text"].as_str().unwrap().contains("document"));
        let nested = blocks[1]["content"].as_array().unwrap();
        assert_eq!(nested[0]["text"], "screen");
        assert_eq!(nested[1]["type"], "image");
        assert_eq!(nested[1]["source"]["data"], "abc");
    }

    #[test]
    fn rejects_message_that_contains_only_document_blocks() {
        let mut request = request_with_messages(vec![crate::anthropic::schema::AnthropicMessage {
            role: "user".into(),
            content: serde_json::json!([
                {
                    "type": "document",
                    "source": {"type": "url", "url": "https://example.test/doc.pdf"}
                }
            ]),
            extra: Default::default(),
        }]);

        let err = substitute_unsupported_content(&mut request).unwrap_err();

        assert!(err.to_string().contains("does not support document"));
        assert!(err.to_string().contains("no text, image, or tool content"));
    }

    #[test]
    fn accepts_message_that_contains_only_image_blocks() {
        let original = serde_json::json!([
            {
                "type": "image",
                "source": {"type": "url", "url": "https://example.test/image.png"}
            }
        ]);
        let mut request = request_with_messages(vec![crate::anthropic::schema::AnthropicMessage {
            role: "user".into(),
            content: original.clone(),
            extra: Default::default(),
        }]);

        let replaced = substitute_unsupported_content(&mut request).unwrap();

        assert_eq!(replaced, 0);
        assert_eq!(request.messages[0].content, original);
    }

    #[test]
    fn leaves_plain_text_messages_untouched() {
        let original = serde_json::json!([
            {"type": "text", "text": "hello"},
            {"type": "tool_result", "tool_use_id": "t1", "content": "output"}
        ]);
        let mut request = request_with_messages(vec![crate::anthropic::schema::AnthropicMessage {
            role: "user".into(),
            content: original.clone(),
            extra: Default::default(),
        }]);

        let replaced = substitute_unsupported_content(&mut request).unwrap();

        assert_eq!(replaced, 0);
        assert_eq!(request.messages[0].content, original);
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
            .resolve(Provider::DeepSeek, "deepseek-flash[1m]")
            .unwrap();
        assert_eq!(resolved.upstream_model, "deepseek-flash");
    }
}
