use async_trait::async_trait;
use axum::http::StatusCode;
use proxy_core::{
    auth::{AuthManager, MemoryTokenStore, TokenRefreshClient, TokenResponse},
    config::{AppConfig, AppPaths, Provider},
    serve,
};
use serde_json::Value;
use std::{sync::Arc, time::Duration};

struct NoRefresh;

#[async_trait]
impl TokenRefreshClient for NoRefresh {
    async fn refresh(&self, _: &str) -> proxy_core::error::Result<TokenResponse> {
        unreachable!("DeepSeek live test does not use Codex OAuth")
    }
}

#[tokio::test]
#[ignore = "requires DEEPSEEK_API_KEY and calls the live DeepSeek API"]
async fn live_deepseek_proxy_reaches_deepseek_api() {
    assert!(
        std::env::var("DEEPSEEK_API_KEY")
            .ok()
            .is_some_and(|key| !key.trim().is_empty()),
        "set DEEPSEEK_API_KEY before running this ignored live test"
    );

    let dir = tempfile::tempdir().unwrap();
    let paths = AppPaths {
        config_dir: dir.path().join("config"),
        logs_dir: dir.path().join("logs"),
        config_file: dir.path().join("config/config.json"),
        model_profiles_file: dir.path().join("config/model-profiles.json"),
        admin_token_file: dir.path().join("config/admin-token"),
        claude_shim_file: dir.path().join("config/claude-shim.json"),
        auth_file: dir.path().join("config/auth.json"),
        route_pins_file: dir.path().join("config/route-pins.json"),
        codex_session_state_file: dir.path().join("config/codex-session-state.json"),
        deepseek_api_key_file: dir.path().join("config/deepseek-api-key"),
        custom_openai_api_key_file: dir.path().join("config/custom-openai-api-key"),
    };
    let config = AppConfig {
        port: 0,
        provider: Provider::DeepSeek,
        ..Default::default()
    };

    let auth = AuthManager::new(Arc::new(MemoryTokenStore::default()), Arc::new(NoRefresh));
    let server = serve(config, paths, auth).await.unwrap();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap();

    let response = client
        .post(format!("http://{}/v1/messages", server.addr))
        .json(&serde_json::json!({
            "model": "deepseek-v4-pro[1m]",
            "max_tokens": 128,
            "stream": false,
            "system": "You are a test responder. Follow the user's instruction exactly.",
            "messages": [{
                "role": "user",
                "content": "Reply with exactly: cc-codex-proxy deepseek ok"
            }]
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    server.stop().await;

    match status {
        StatusCode::OK => {
            let value: Value =
                serde_json::from_str(&body).expect("response should be Anthropic JSON");
            let text = value
                .get("content")
                .and_then(Value::as_array)
                .map(|content| {
                    content
                        .iter()
                        .filter_map(|block| block.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default()
                .to_ascii_lowercase();
            assert!(
                text.contains("cc-codex-proxy") && text.contains("deepseek"),
                "unexpected response body: {body}"
            );
        }
        StatusCode::PAYMENT_REQUIRED => {
            assert!(
                body.to_ascii_lowercase().contains("insufficient balance"),
                "unexpected payment-required response: {body}"
            );
        }
        _ => panic!("unexpected response: {body}"),
    }
}
