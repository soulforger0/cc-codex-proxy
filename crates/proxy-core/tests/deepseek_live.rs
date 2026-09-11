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

/// A 64x64 solid-red PNG, used to exercise DeepSeek's vision input.
const RED_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAS0lEQVR42u3PQQkAAAgAsetfWiP4FgYrsKZeS0BAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEDgsqnc8OJg6Ln3AAAAAElFTkSuQmCC";

async fn spawn_deepseek_proxy() -> proxy_core::server::ServerHandle {
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
    let mut config = AppConfig {
        port: 0,
        provider: Provider::DeepSeek,
        ..Default::default()
    };
    config.routing.active_profile = "deepseek".into();

    let auth = AuthManager::new(Arc::new(MemoryTokenStore::default()), Arc::new(NoRefresh));
    // Keep the tempdir alive for the process lifetime: the proxy reads the API
    // key file lazily on each request.
    std::mem::forget(dir);
    serve(config, paths, auth).await.unwrap()
}

fn response_text(body: &str) -> String {
    let value: Value = serde_json::from_str(body).expect("response should be Anthropic JSON");
    value
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
        .to_ascii_lowercase()
}

/// DeepSeek reports an exhausted balance as 402; treat that as "no API budget to
/// test with" rather than a proxy failure.
fn assert_not_out_of_balance(status: StatusCode, body: &str) {
    if status == StatusCode::PAYMENT_REQUIRED {
        assert!(
            body.to_ascii_lowercase().contains("insufficient balance"),
            "unexpected payment-required response: {body}"
        );
    }
}

#[tokio::test]
#[ignore = "requires DEEPSEEK_API_KEY and calls the live DeepSeek API"]
async fn live_deepseek_proxy_reaches_deepseek_api() {
    let server = spawn_deepseek_proxy().await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap();

    let response = client
        .post(format!("http://{}/v1/messages", server.addr))
        .json(&serde_json::json!({
            "model": "deepseek-flash[1m]",
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

    assert_not_out_of_balance(status, &body);
    match status {
        StatusCode::OK => {
            let text = response_text(&body);
            assert!(
                text.contains("cc-codex-proxy") && text.contains("deepseek"),
                "unexpected response body: {body}"
            );
        }
        StatusCode::PAYMENT_REQUIRED => {}
        _ => panic!("unexpected response: {body}"),
    }
}

#[tokio::test]
#[ignore = "requires DEEPSEEK_API_KEY and calls the live DeepSeek API"]
async fn live_deepseek_proxy_forwards_image_blocks() {
    let server = spawn_deepseek_proxy().await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap();

    let response = client
        .post(format!("http://{}/v1/messages", server.addr))
        .json(&serde_json::json!({
            "model": "deepseek-flash[1m]",
            "max_tokens": 64,
            "stream": false,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "What color is this image? Answer with one word."},
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": "image/png",
                            "data": RED_PNG_BASE64
                        }
                    }
                ]
            }]
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    server.stop().await;

    assert_not_out_of_balance(status, &body);
    match status {
        StatusCode::OK => {
            let text = response_text(&body);
            assert!(
                text.contains("red"),
                "vision response should name the color, got: {body}"
            );
        }
        StatusCode::PAYMENT_REQUIRED => {}
        _ => panic!("unexpected response: {body}"),
    }
}
