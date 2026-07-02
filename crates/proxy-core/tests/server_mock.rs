use async_trait::async_trait;
use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, Response, StatusCode},
    routing::{get, post},
    Json, Router,
};
use bytes::Bytes;
use futures_util::{future::join_all, StreamExt};
use proxy_core::{
    auth::{AuthManager, MemoryTokenStore, StoredAuth, TokenRefreshClient, TokenResponse},
    config::{AppConfig, AppPaths, CodexConfig, CodexTransport, CustomOpenAIProtocol, Provider},
    custom_openai::store_api_key as store_custom_openai_api_key,
    deepseek::store_api_key,
    serve,
};
use std::{
    convert::Infallible,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};
use tokio::time::{sleep, Duration};

struct NoRefresh;

#[async_trait]
impl TokenRefreshClient for NoRefresh {
    async fn refresh(&self, _: &str) -> proxy_core::error::Result<TokenResponse> {
        unreachable!("test token is not expiring")
    }
}

#[tokio::test]
async fn non_streaming_message_accumulates_mock_upstream_sse() {
    let upstream = start_mock_upstream(mock_success_app()).await;
    let (config, paths) = test_config(upstream, "/codex").await;
    let server = serve(config.clone(), paths, test_auth()).await.unwrap();
    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{}/v1/messages", server.addr))
        .json(&serde_json::json!({
            "model": "gpt-5.4",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<serde_json::Value>().await.unwrap();
    assert_eq!(body["content"][0]["text"], "hello from codex");
    server.stop().await;
}

#[tokio::test]
async fn count_tokens_is_local() {
    let upstream = start_mock_upstream(mock_success_app()).await;
    let (config, paths) = test_config(upstream, "/codex").await;
    let server = serve(config, paths, test_auth()).await.unwrap();
    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{}/v1/messages/count_tokens", server.addr))
        .json(&serde_json::json!({
            "model": "gpt-5.4",
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.json::<serde_json::Value>().await.unwrap()["input_tokens"]
            .as_u64()
            .unwrap()
            > 0
    );
    server.stop().await;
}

#[tokio::test]
async fn claude_clear_starts_fresh_codex_upstream_session_without_forwarding() {
    let state = Arc::new(CodexSessionCaptureState::default());
    let upstream = start_mock_upstream(mock_codex_session_capture_app(state.clone())).await;
    let (config, paths) = test_config(upstream, "/codex").await;
    let server = serve(config, paths, test_auth()).await.unwrap();
    let client = reqwest::Client::new();

    let first = client
        .post(format!("http://{}/v1/messages", server.addr))
        .header("x-claude-code-session-id", "session-a")
        .json(&serde_json::json!({
            "model": "gpt-5.5[1m]",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "before clear"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let clear = client
        .post(format!("http://{}/v1/messages", server.addr))
        .header("x-claude-code-session-id", "session-a")
        .json(&serde_json::json!({
            "model": "gpt-5.5[1m]",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "/clear previous task"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(clear.status(), StatusCode::OK);
    let clear_body = clear.json::<serde_json::Value>().await.unwrap();
    assert_eq!(clear_body["stop_reason"], "end_turn");
    assert!(clear_body["content"].as_array().unwrap().is_empty());
    assert_eq!(state.calls.load(Ordering::SeqCst), 1);

    let after = client
        .post(format!("http://{}/v1/messages", server.addr))
        .header("x-claude-code-session-id", "session-a")
        .json(&serde_json::json!({
            "model": "gpt-5.5[1m]",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "after clear"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(after.status(), StatusCode::OK);

    let session_ids = state.session_ids.lock().unwrap().clone();
    assert_eq!(session_ids.len(), 2);
    let first_session = session_ids[0].as_ref().unwrap();
    let after_session = session_ids[1].as_ref().unwrap();
    assert_ne!(first_session, after_session);
    assert!(first_session.starts_with("ccp-"), "{first_session}");
    assert!(after_session.ends_with("-session-a-g1"), "{after_session}");

    let bodies = state.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0]["input"][0]["content"][0]["text"], "before clear");
    assert_eq!(bodies[1]["input"][0]["content"][0]["text"], "after clear");
    server.stop().await;
}

#[tokio::test]
async fn transcript_shrink_starts_fresh_codex_upstream_session() {
    let state = Arc::new(CodexSessionCaptureState::default());
    let upstream = start_mock_upstream(mock_codex_session_capture_app(state.clone())).await;
    let (config, paths) = test_config(upstream, "/codex").await;
    let server = serve(config, paths, test_auth()).await.unwrap();
    let client = reqwest::Client::new();

    let before_compact = client
        .post(format!("http://{}/v1/messages", server.addr))
        .header("x-claude-code-session-id", "session-b")
        .json(&serde_json::json!({
            "model": "gpt-5.5[1m]",
            "max_tokens": 64,
            "stream": false,
            "messages": [
                {"role": "user", "content": "first"},
                {"role": "assistant", "content": "second"},
                {"role": "user", "content": "third"}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(before_compact.status(), StatusCode::OK);

    let after_compact = client
        .post(format!("http://{}/v1/messages", server.addr))
        .header("x-claude-code-session-id", "session-b")
        .json(&serde_json::json!({
            "model": "gpt-5.5[1m]",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "compacted follow-up"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(after_compact.status(), StatusCode::OK);

    let session_ids = state.session_ids.lock().unwrap().clone();
    assert_eq!(session_ids.len(), 2);
    assert_ne!(session_ids[0], session_ids[1]);
    assert!(
        session_ids[1]
            .as_ref()
            .is_some_and(|session| session.ends_with("-session-b-g1")),
        "{session_ids:?}"
    );
    server.stop().await;
}

#[tokio::test]
async fn dynamic_provider_switch_uses_same_local_server() {
    let codex_upstream = start_mock_upstream(mock_success_app()).await;
    let deepseek_state = Arc::new(DeepSeekMockState::default());
    let deepseek_upstream =
        start_mock_upstream(mock_deepseek_json_app(deepseek_state.clone())).await;
    let (mut config, paths) = test_config(codex_upstream, "/codex").await;
    config.deepseek.base_url = format!("http://{deepseek_upstream}/anthropic");
    store_api_key(&paths.deepseek_api_key_file, "deepseek-secret").unwrap();
    let server = serve(config, paths, test_auth()).await.unwrap();
    let addr = server.addr;
    let client = reqwest::Client::new();

    let codex_response = client
        .post(format!("http://{addr}/v1/messages"))
        .json(&serde_json::json!({
            "model": "gpt-5.5[1m]",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(codex_response.status(), StatusCode::OK);
    let codex_body = codex_response.json::<serde_json::Value>().await.unwrap();
    assert_eq!(codex_body["content"][0]["text"], "hello from codex");

    admin_set_route(addr, "deepseek").await;

    let deepseek_response = client
        .post(format!("http://{addr}/v1/messages"))
        .json(&serde_json::json!({
            "model": "gpt-5.5[1m]",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(deepseek_response.status(), StatusCode::OK);
    let deepseek_body = deepseek_response.json::<serde_json::Value>().await.unwrap();
    assert_eq!(deepseek_body["content"][0]["text"], "hello from deepseek");
    assert_eq!(deepseek_state.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        deepseek_state.last_model.lock().unwrap().as_deref(),
        Some("deepseek-v4-pro")
    );
    assert_eq!(server.addr, addr);
    server.stop().await;
}

#[tokio::test]
async fn session_pinning_keeps_existing_session_on_original_route() {
    let codex_upstream = start_mock_upstream(mock_success_app()).await;
    let deepseek_state = Arc::new(DeepSeekMockState::default());
    let deepseek_upstream =
        start_mock_upstream(mock_deepseek_json_app(deepseek_state.clone())).await;
    let (mut config, paths) = test_config(codex_upstream, "/codex").await;
    config.deepseek.base_url = format!("http://{deepseek_upstream}/anthropic");
    store_api_key(&paths.deepseek_api_key_file, "deepseek-secret").unwrap();
    let server = serve(config, paths, test_auth()).await.unwrap();
    let client = reqwest::Client::new();

    let first = client
        .post(format!("http://{}/v1/messages", server.addr))
        .header("x-claude-code-session-id", "session-a")
        .json(&serde_json::json!({
            "model": "gpt-5.5[1m]",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        first.json::<serde_json::Value>().await.unwrap()["content"][0]["text"],
        "hello from codex"
    );

    admin_set_route(server.addr, "deepseek").await;

    let pinned = client
        .post(format!("http://{}/v1/messages", server.addr))
        .header("x-claude-code-session-id", "session-a")
        .json(&serde_json::json!({
            "model": "gpt-5.5[1m]",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "still pinned"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(pinned.status(), StatusCode::OK);
    assert_eq!(
        pinned.json::<serde_json::Value>().await.unwrap()["content"][0]["text"],
        "hello from codex"
    );

    let fresh = client
        .post(format!("http://{}/v1/messages", server.addr))
        .header("x-claude-code-session-id", "session-b")
        .json(&serde_json::json!({
            "model": "gpt-5.5[1m]",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "new route"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(fresh.status(), StatusCode::OK);
    assert_eq!(
        fresh.json::<serde_json::Value>().await.unwrap()["content"][0]["text"],
        "hello from deepseek"
    );
    assert_eq!(deepseek_state.calls.load(Ordering::SeqCst), 1);
    server.stop().await;
}

#[tokio::test]
async fn persisted_session_pin_survives_server_restart() {
    let codex_upstream = start_mock_upstream(mock_success_app()).await;
    let deepseek_state = Arc::new(DeepSeekMockState::default());
    let deepseek_upstream =
        start_mock_upstream(mock_deepseek_json_app(deepseek_state.clone())).await;
    let (mut config, paths) = test_config(codex_upstream, "/codex").await;
    config.deepseek.base_url = format!("http://{deepseek_upstream}/anthropic");
    store_api_key(&paths.deepseek_api_key_file, "deepseek-secret").unwrap();

    let server = serve(config.clone(), paths.clone(), test_auth())
        .await
        .unwrap();
    let client = reqwest::Client::new();
    let first = client
        .post(format!("http://{}/v1/messages", server.addr))
        .header("x-claude-code-session-id", "session-a")
        .json(&serde_json::json!({
            "model": "gpt-5.5[1m]",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    admin_set_route(server.addr, "deepseek").await;
    server.stop().await;

    let mut restarted_config = config;
    restarted_config.routing.active_profile = "deepseek".into();
    let restarted = serve(restarted_config, paths, test_auth()).await.unwrap();
    let pinned = client
        .post(format!("http://{}/v1/messages", restarted.addr))
        .header("x-claude-code-session-id", "session-a")
        .json(&serde_json::json!({
            "model": "gpt-5.5[1m]",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "still pinned"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(pinned.status(), StatusCode::OK);
    assert_eq!(
        pinned.json::<serde_json::Value>().await.unwrap()["content"][0]["text"],
        "hello from codex"
    );

    let fresh = client
        .post(format!("http://{}/v1/messages", restarted.addr))
        .header("x-claude-code-session-id", "session-b")
        .json(&serde_json::json!({
            "model": "gpt-5.5[1m]",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "new route"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(fresh.status(), StatusCode::OK);
    assert_eq!(
        fresh.json::<serde_json::Value>().await.unwrap()["content"][0]["text"],
        "hello from deepseek"
    );
    assert_eq!(deepseek_state.calls.load(Ordering::SeqCst), 1);
    restarted.stop().await;
}

#[tokio::test]
async fn upstream_429_is_preserved() {
    let upstream = start_mock_upstream(Router::new().route(
        "/rate-limit",
        post(|| async {
            Response::builder()
                .status(StatusCode::TOO_MANY_REQUESTS)
                .header(header::RETRY_AFTER, "5")
                .body(Body::from("slow down"))
                .unwrap()
        }),
    ))
    .await;
    let (config, paths) = test_config(upstream, "/rate-limit").await;
    let server = serve(config, paths, test_auth()).await.unwrap();
    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{}/v1/messages", server.addr))
        .json(&serde_json::json!({
            "model": "gpt-5.4",
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers().get(header::RETRY_AFTER).unwrap(), "5");
    server.stop().await;
}

#[tokio::test]
async fn deepseek_non_streaming_request_is_forwarded_to_anthropic_messages() {
    let state = Arc::new(DeepSeekMockState::default());
    let upstream = start_mock_upstream(mock_deepseek_json_app(state.clone())).await;
    let (config, paths) = test_deepseek_config(upstream).await;
    let server = serve(config, paths, test_auth()).await.unwrap();
    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{}/v1/messages", server.addr))
        .json(&serde_json::json!({
            "model": "deepseek-v4-pro[1m]",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<serde_json::Value>().await.unwrap();
    assert_eq!(body["content"][0]["text"], "hello from deepseek");
    assert_eq!(state.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        state.last_model.lock().unwrap().as_deref(),
        Some("deepseek-v4-pro")
    );
    assert_eq!(
        state.last_key.lock().unwrap().as_deref(),
        Some("deepseek-secret")
    );
    server.stop().await;
}

#[tokio::test]
async fn deepseek_effort_is_normalized_before_forwarding() {
    let state = Arc::new(DeepSeekMockState::default());
    let upstream = start_mock_upstream(mock_deepseek_json_app(state.clone())).await;
    let (config, paths) = test_deepseek_config(upstream).await;
    let server = serve(config, paths, test_auth()).await.unwrap();
    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{}/v1/messages", server.addr))
        .json(&serde_json::json!({
            "model": "deepseek-v4-pro",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "hello"}],
            "output_config": {
                "effort": "xhigh",
                "format": { "type": "json_schema" }
            }
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let output_config = state.last_output_config.lock().unwrap().clone().unwrap();
    assert_eq!(output_config["effort"], serde_json::json!("high"));
    assert_eq!(
        output_config["format"],
        serde_json::json!({ "type": "json_schema" })
    );
    server.stop().await;
}

#[tokio::test]
async fn deepseek_stale_codex_model_is_rewritten_before_forwarding() {
    let state = Arc::new(DeepSeekMockState::default());
    let upstream = start_mock_upstream(mock_deepseek_json_app(state.clone())).await;
    let (config, paths) = test_deepseek_config(upstream).await;
    let server = serve(config, paths, test_auth()).await.unwrap();
    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{}/v1/messages", server.addr))
        .json(&serde_json::json!({
            "model": "gpt-5.5[1m]",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        state.last_model.lock().unwrap().as_deref(),
        Some("deepseek-v4-pro")
    );
    server.stop().await;
}

#[tokio::test]
async fn deepseek_streaming_response_is_passed_through() {
    let state = Arc::new(DeepSeekMockState::default());
    let upstream = start_mock_upstream(mock_deepseek_streaming_app(state)).await;
    let (config, paths) = test_deepseek_config(upstream).await;
    let server = serve(config, paths, test_auth()).await.unwrap();
    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{}/v1/messages", server.addr))
        .json(&serde_json::json!({
            "model": "deepseek-v4-flash",
            "max_tokens": 64,
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.unwrap();
    assert!(body.contains("event: message_start"), "{body}");
    assert!(body.contains("hello from deepseek stream"), "{body}");
    assert!(body.contains("event: message_stop"), "{body}");
    server.stop().await;
}

#[tokio::test]
async fn deepseek_upstream_errors_are_preserved() {
    for status in [
        StatusCode::UNAUTHORIZED,
        StatusCode::PAYMENT_REQUIRED,
        StatusCode::UNPROCESSABLE_ENTITY,
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::SERVICE_UNAVAILABLE,
    ] {
        let upstream = start_mock_upstream(mock_deepseek_error_app(status)).await;
        let (config, paths) = test_deepseek_config(upstream).await;
        let server = serve(config, paths, test_auth()).await.unwrap();
        let client = reqwest::Client::new();
        let response = client
            .post(format!("http://{}/v1/messages", server.addr))
            .json(&serde_json::json!({
                "model": "deepseek-v4-pro",
                "max_tokens": 1,
                "messages": [{"role": "user", "content": "hello"}]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        if status == StatusCode::TOO_MANY_REQUESTS {
            assert_eq!(response.headers().get(header::RETRY_AFTER).unwrap(), "7");
        }
        server.stop().await;
    }
}

#[tokio::test]
async fn custom_openai_responses_request_uses_configured_url_and_optional_key() {
    let state = Arc::new(CustomOpenAIMockState::default());
    let upstream = start_mock_upstream(mock_custom_openai_responses_app(state.clone())).await;
    let (config, paths) =
        test_custom_openai_config(upstream, CustomOpenAIProtocol::Responses, false).await;
    let server = serve(config, paths, test_auth()).await.unwrap();
    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{}/v1/messages", server.addr))
        .json(&serde_json::json!({
            "model": "llama-3.3-70b[1m]",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<serde_json::Value>().await.unwrap();
    assert_eq!(body["content"][0]["text"], "hello from custom responses");
    assert_eq!(state.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        state.last_model.lock().unwrap().as_deref(),
        Some("llama-3.3-70b")
    );
    assert!(state.last_authorization.lock().unwrap().is_none());
    server.stop().await;
}

#[tokio::test]
async fn custom_openai_responses_request_sends_authorization_when_key_saved() {
    let state = Arc::new(CustomOpenAIMockState::default());
    let upstream = start_mock_upstream(mock_custom_openai_responses_app(state.clone())).await;
    let (config, paths) =
        test_custom_openai_config(upstream, CustomOpenAIProtocol::Responses, true).await;
    let server = serve(config, paths, test_auth()).await.unwrap();
    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{}/v1/messages", server.addr))
        .json(&serde_json::json!({
            "model": "gpt-5.4",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        state.last_authorization.lock().unwrap().as_deref(),
        Some("Bearer custom-secret")
    );
    server.stop().await;
}

#[tokio::test]
async fn custom_openai_chat_completions_non_streaming_is_translated() {
    let state = Arc::new(CustomOpenAIMockState::default());
    let upstream = start_mock_upstream(mock_custom_openai_chat_app(state.clone())).await;
    let (config, paths) =
        test_custom_openai_config(upstream, CustomOpenAIProtocol::ChatCompletions, false).await;
    let server = serve(config, paths, test_auth()).await.unwrap();
    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{}/v1/messages", server.addr))
        .json(&serde_json::json!({
            "model": "local-model[1m]",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<serde_json::Value>().await.unwrap();
    assert_eq!(body["content"][0]["text"], "hello from custom chat");
    assert_eq!(state.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        state.last_model.lock().unwrap().as_deref(),
        Some("local-model")
    );
    server.stop().await;
}

#[tokio::test]
async fn custom_openai_admin_status_reports_protocol() {
    let state = Arc::new(CustomOpenAIMockState::default());
    let upstream = start_mock_upstream(mock_custom_openai_chat_app(state)).await;
    let (config, paths) =
        test_custom_openai_config(upstream, CustomOpenAIProtocol::ChatCompletions, false).await;
    let server = serve(config, paths, test_auth()).await.unwrap();

    let status = admin_status(server.addr).await;
    assert_eq!(status["provider"], "custom-openai");
    assert_eq!(status["transport"]["configured"], "chat-completions");
    assert_eq!(status["transport"]["currentMethod"], "http-sse");
    assert_eq!(status["customOpenAI"]["protocol"], "chat-completions");
    assert_eq!(status["customOpenAI"]["apiKey"]["configured"], false);
    server.stop().await;
}

#[tokio::test]
async fn deepseek_missing_api_key_is_local_unauthorized() {
    let upstream = start_mock_upstream(mock_deepseek_json_app(Arc::default())).await;
    let (mut config, paths) = test_config(upstream, "/unused").await;
    config.provider = Provider::DeepSeek;
    config.routing.active_profile = "deepseek".into();
    config.deepseek.base_url = format!("http://{upstream}/anthropic");
    let server = serve(config, paths, test_auth()).await.unwrap();
    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{}/v1/messages", server.addr))
        .json(&serde_json::json!({
            "model": "deepseek-v4-pro",
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    server.stop().await;
}

#[tokio::test]
async fn deepseek_admin_status_reports_http_sse_transport() {
    let upstream = start_mock_upstream(mock_deepseek_json_app(Arc::default())).await;
    let (config, paths) = test_deepseek_config(upstream).await;
    let server = serve(config, paths, test_auth()).await.unwrap();

    let status = admin_status(server.addr).await;
    assert_eq!(status["provider"], "deepseek");
    assert_eq!(status["transport"]["configured"], "http-sse");
    assert_eq!(status["transport"]["currentMethod"], "http-sse");
    assert!(status["transport"]["websocketCooldownMs"].is_null());
    server.stop().await;
}

#[tokio::test]
async fn auto_transport_falls_back_to_http_and_cools_down_websocket() {
    let state = Arc::new(HttpOnlyState::default());
    let upstream = start_mock_upstream(mock_http_only_app(state.clone())).await;
    let (mut config, paths) = test_config(upstream, "/codex").await;
    config.codex.transport = CodexTransport::Auto;
    let server = serve(config, paths, test_auth()).await.unwrap();
    let client = reqwest::Client::new();

    for _ in 0..2 {
        let response = client
            .post(format!("http://{}/v1/messages", server.addr))
            .json(&serde_json::json!({
                "model": "gpt-5.4",
                "max_tokens": 64,
                "stream": false,
                "messages": [{"role": "user", "content": "hello"}]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.json::<serde_json::Value>().await.unwrap();
        assert_eq!(body["content"][0]["text"], "hello from http");
    }

    assert!(state.websocket_attempts.load(Ordering::SeqCst) <= 1);
    assert_eq!(state.http_posts.load(Ordering::SeqCst), 2);
    let status = admin_status(server.addr).await;
    assert_eq!(status["transport"]["configured"], "auto");
    assert_eq!(status["transport"]["currentMethod"], "http-sse");
    assert!(status["transport"]["websocketCooldownMs"].as_u64().unwrap() > 0);
    server.stop().await;
}

#[tokio::test]
async fn streaming_response_sets_sse_headers() {
    let upstream = start_mock_upstream(mock_success_app()).await;
    let (config, paths) = test_config(upstream, "/codex").await;
    let server = serve(config, paths, test_auth()).await.unwrap();
    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{}/v1/messages", server.addr))
        .json(&serde_json::json!({
            "model": "gpt-5.4",
            "max_tokens": 64,
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-cache, no-transform"
    );
    assert_eq!(response.headers().get("x-accel-buffering").unwrap(), "no");
    server.stop().await;
}

#[tokio::test]
async fn messages_body_limit_returns_payload_too_large() {
    let upstream = start_mock_upstream(mock_success_app()).await;
    let (mut config, paths) = test_config(upstream, "/codex").await;
    config.messages_body_limit_bytes = 128;
    let server = serve(config, paths, test_auth()).await.unwrap();
    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{}/v1/messages", server.addr))
        .json(&serde_json::json!({
            "model": "gpt-5.4",
            "max_tokens": 64,
            "stream": false,
            "messages": [{"role": "user", "content": "x".repeat(512)}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn streaming_load_completes_100_agents() {
    let state = Arc::new(LoadState::default());
    let upstream = start_mock_upstream(mock_streaming_app(state.clone())).await;
    let (config, paths) = test_config(upstream, "/codex").await;
    let server = serve(config, paths, test_auth()).await.unwrap();
    let report = run_streaming_load(server.addr, 100).await;

    assert_eq!(state.completed.load(Ordering::SeqCst), 100);
    assert!(
        state.max_active.load(Ordering::SeqCst) >= 25,
        "expected meaningful upstream overlap, max_active={}",
        state.max_active.load(Ordering::SeqCst)
    );
    println!(
        "100-agent streaming load: elapsed {:?}, first-delta p95 {:?}, completion p95 {:?}, max upstream concurrency {}",
        report.elapsed,
        report.first_delta_p95,
        report.completion_p95,
        state.max_active.load(Ordering::SeqCst),
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "explicit stress run: cargo test -p proxy-core --test server_mock -- streaming_stress_250_agents --ignored --nocapture"]
async fn streaming_stress_250_agents() {
    let state = Arc::new(LoadState::default());
    let upstream = start_mock_upstream(mock_streaming_app(state.clone())).await;
    let (config, paths) = test_config(upstream, "/codex").await;
    let server = serve(config, paths, test_auth()).await.unwrap();
    let before = ProcessSnapshot::capture();
    let report = run_streaming_load(server.addr, 250).await;
    let after = ProcessSnapshot::capture();

    assert_eq!(state.completed.load(Ordering::SeqCst), 250);
    println!(
        "250-agent stress: elapsed {:?}, first-delta p95 {:?}, completion p95 {:?}, max completion {:?}, max upstream concurrency {}, fd {}->{}, rss_kb {:?}->{:?}",
        report.elapsed,
        report.first_delta_p95,
        report.completion_p95,
        report.completion_max,
        state.max_active.load(Ordering::SeqCst),
        before.fd_count,
        after.fd_count,
        before.rss_kb,
        after.rss_kb,
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn downstream_disconnect_cancels_upstream_stream() {
    let state = Arc::new(LoadState::default());
    let upstream = start_mock_upstream(mock_streaming_app(state.clone())).await;
    let (config, paths) = test_config(upstream, "/codex").await;
    let server = serve(config, paths, test_auth()).await.unwrap();
    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{}/v1/messages", server.addr))
        .json(&serde_json::json!({
            "model": "gpt-5.4",
            "max_tokens": 64,
            "stream": true,
            "messages": [{"role": "user", "content": "cancel"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.bytes_stream();
    let mut body = String::new();
    while !body.contains("event: content_block_delta") {
        let chunk = stream.next().await.unwrap().unwrap();
        body.push_str(&String::from_utf8_lossy(&chunk));
    }
    drop(stream);

    for _ in 0..50 {
        if state.completed.load(Ordering::SeqCst) == 1 {
            server.stop().await;
            return;
        }
        sleep(Duration::from_millis(10)).await;
    }
    server.stop().await;
    panic!("upstream stream was not cancelled after downstream disconnect");
}

fn mock_success_app() -> Router {
    Router::new().route(
        "/codex",
        post(|| async {
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from(
                    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello from codex\"}\n\n\
                     data: {\"type\":\"response.completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":3}}\n\n",
                ))
                .unwrap()
        }),
    )
}

#[derive(Default)]
struct CodexSessionCaptureState {
    calls: AtomicUsize,
    session_ids: Mutex<Vec<Option<String>>>,
    bodies: Mutex<Vec<serde_json::Value>>,
}

fn mock_codex_session_capture_app(state: Arc<CodexSessionCaptureState>) -> Router {
    Router::new()
        .route("/codex", post(mock_codex_session_capture_response))
        .with_state(state)
}

async fn mock_codex_session_capture_response(
    State(state): State<Arc<CodexSessionCaptureState>>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response<Body> {
    state.calls.fetch_add(1, Ordering::SeqCst);
    state.session_ids.lock().unwrap().push(
        headers
            .get("session_id")
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned),
    );
    state.bodies.lock().unwrap().push(body);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"captured\"}\n\n\
             data: {\"type\":\"response.completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":3}}\n\n",
        ))
        .unwrap()
}

#[derive(Default)]
struct DeepSeekMockState {
    calls: AtomicUsize,
    last_model: Mutex<Option<String>>,
    last_key: Mutex<Option<String>>,
    last_output_config: Mutex<Option<serde_json::Value>>,
}

#[derive(Default)]
struct CustomOpenAIMockState {
    calls: AtomicUsize,
    last_model: Mutex<Option<String>>,
    last_authorization: Mutex<Option<String>>,
}

fn mock_custom_openai_responses_app(state: Arc<CustomOpenAIMockState>) -> Router {
    Router::new()
        .route(
            "/openai/v1/responses",
            post(mock_custom_openai_responses_response),
        )
        .with_state(state)
}

async fn mock_custom_openai_responses_response(
    State(state): State<Arc<CustomOpenAIMockState>>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response<Body> {
    state.calls.fetch_add(1, Ordering::SeqCst);
    *state.last_model.lock().unwrap() = body
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    *state.last_authorization.lock().unwrap() = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello from custom responses\"}\n\n\
             data: {\"type\":\"response.completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":3}}\n\n",
        ))
        .unwrap()
}

fn mock_custom_openai_chat_app(state: Arc<CustomOpenAIMockState>) -> Router {
    Router::new()
        .route(
            "/openai/v1/chat/completions",
            post(mock_custom_openai_chat_response),
        )
        .with_state(state)
}

async fn mock_custom_openai_chat_response(
    State(state): State<Arc<CustomOpenAIMockState>>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response<Body> {
    state.calls.fetch_add(1, Ordering::SeqCst);
    *state.last_model.lock().unwrap() = body
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    *state.last_authorization.lock().unwrap() = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "id": "chatcmpl_custom",
                "object": "chat.completion",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "hello from custom chat"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 3, "completion_tokens": 4}
            })
            .to_string(),
        ))
        .unwrap()
}

fn mock_deepseek_json_app(state: Arc<DeepSeekMockState>) -> Router {
    Router::new()
        .route("/anthropic/v1/messages", post(mock_deepseek_json_response))
        .with_state(state)
}

async fn mock_deepseek_json_response(
    State(state): State<Arc<DeepSeekMockState>>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response<Body> {
    state.calls.fetch_add(1, Ordering::SeqCst);
    *state.last_model.lock().unwrap() = body
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    *state.last_key.lock().unwrap() = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    *state.last_output_config.lock().unwrap() = body.get("output_config").cloned();
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "id": "msg_deepseek",
                "type": "message",
                "role": "assistant",
                "model": "deepseek-v4-pro",
                "content": [{"type": "text", "text": "hello from deepseek"}],
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": {
                    "input_tokens": 3,
                    "output_tokens": 3,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0
                }
            })
            .to_string(),
        ))
        .unwrap()
}

fn mock_deepseek_streaming_app(state: Arc<DeepSeekMockState>) -> Router {
    Router::new()
        .route(
            "/anthropic/v1/messages",
            post(move || {
                let state = state.clone();
                async move {
                    state.calls.fetch_add(1, Ordering::SeqCst);
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from(
                        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_deepseek\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"deepseek-v4-flash\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":0,\"output_tokens\":0,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0}}}\n\n\
                         event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
                         event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello from deepseek stream\"}}\n\n\
                         event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
                         event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"input_tokens\":3,\"output_tokens\":4,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0}}\n\n\
                         event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
                    ))
                    .unwrap()
                }
            }),
        )
}

fn mock_deepseek_error_app(status: StatusCode) -> Router {
    Router::new().route(
        "/anthropic/v1/messages",
        post(move || async move {
            let mut builder = Response::builder().status(status);
            if status == StatusCode::TOO_MANY_REQUESTS {
                builder = builder.header(header::RETRY_AFTER, "7");
            }
            builder.body(Body::from("deepseek error")).unwrap()
        }),
    )
}

#[derive(Default)]
struct HttpOnlyState {
    websocket_attempts: AtomicUsize,
    http_posts: AtomicUsize,
}

fn mock_http_only_app(state: Arc<HttpOnlyState>) -> Router {
    Router::new()
        .route(
            "/codex",
            get(mock_http_only_websocket_attempt).post(mock_http_only_response),
        )
        .with_state(state)
}

async fn mock_http_only_websocket_attempt(
    State(state): State<Arc<HttpOnlyState>>,
) -> Response<Body> {
    state.websocket_attempts.fetch_add(1, Ordering::SeqCst);
    Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .body(Body::from("websocket unavailable"))
        .unwrap()
}

async fn mock_http_only_response(State(state): State<Arc<HttpOnlyState>>) -> Response<Body> {
    state.http_posts.fetch_add(1, Ordering::SeqCst);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello from http\"}\n\n\
             data: {\"type\":\"response.completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":3}}\n\n",
        ))
        .unwrap()
}

#[derive(Default)]
struct LoadState {
    active: AtomicUsize,
    max_active: AtomicUsize,
    completed: AtomicUsize,
}

fn mock_streaming_app(state: Arc<LoadState>) -> Router {
    Router::new()
        .route("/codex", post(mock_streaming_response))
        .with_state(state)
}

async fn mock_streaming_response(State(state): State<Arc<LoadState>>) -> Response<Body> {
    let stream = async_stream::stream! {
        let _guard = ActiveRequest::new(state.clone());
        yield Ok::<Bytes, Infallible>(Bytes::from_static(
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n",
        ));
        sleep(Duration::from_millis(10)).await;
        yield Ok::<Bytes, Infallible>(Bytes::from_static(
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\" from\"}\n\n",
        ));
        sleep(Duration::from_millis(10)).await;
        yield Ok::<Bytes, Infallible>(Bytes::from_static(
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\" codex\"}\n\n",
        ));
        sleep(Duration::from_millis(10)).await;
        yield Ok::<Bytes, Infallible>(Bytes::from_static(
            b"data: {\"type\":\"response.completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":3}}\n\n",
        ));
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}

struct ActiveRequest {
    state: Arc<LoadState>,
}

impl ActiveRequest {
    fn new(state: Arc<LoadState>) -> Self {
        let active = state.active.fetch_add(1, Ordering::SeqCst) + 1;
        update_max(&state.max_active, active);
        Self { state }
    }
}

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        self.state.active.fetch_sub(1, Ordering::SeqCst);
        self.state.completed.fetch_add(1, Ordering::SeqCst);
    }
}

fn update_max(max: &AtomicUsize, candidate: usize) {
    let mut current = max.load(Ordering::Relaxed);
    while candidate > current {
        match max.compare_exchange(current, candidate, Ordering::SeqCst, Ordering::Relaxed) {
            Ok(_) => break,
            Err(next) => current = next,
        }
    }
}

#[derive(Debug)]
struct LoadReport {
    elapsed: std::time::Duration,
    first_delta_p95: std::time::Duration,
    completion_p95: std::time::Duration,
    completion_max: std::time::Duration,
}

#[derive(Debug)]
struct StreamTiming {
    first_delta: std::time::Duration,
    completion: std::time::Duration,
}

async fn run_streaming_load(addr: std::net::SocketAddr, agents: usize) -> LoadReport {
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(agents)
        .build()
        .unwrap();
    let started = Instant::now();
    let tasks = (0..agents)
        .map(|idx| {
            let client = client.clone();
            let url = format!("http://{addr}/v1/messages");
            tokio::spawn(async move {
                let started = Instant::now();
                let response = client
                    .post(url)
                    .header("x-claude-code-session-id", format!("load-session-{idx}"))
                    .json(&serde_json::json!({
                        "model": "gpt-5.4",
                        "max_tokens": 64,
                        "stream": true,
                        "messages": [{"role": "user", "content": format!("hello {idx}")}]
                    }))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let mut stream = response.bytes_stream();
                let mut body = String::new();
                let mut first_delta = None;
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.unwrap();
                    body.push_str(&String::from_utf8_lossy(&chunk));
                    if first_delta.is_none() && body.contains("event: content_block_delta") {
                        first_delta = Some(started.elapsed());
                    }
                }
                assert!(body.contains("event: message_start"), "{body}");
                assert!(body.contains("event: content_block_delta"), "{body}");
                assert!(body.contains("event: message_stop"), "{body}");
                StreamTiming {
                    first_delta: first_delta.expect("first content_block_delta event"),
                    completion: started.elapsed(),
                }
            })
        })
        .collect::<Vec<_>>();
    let mut timings = join_all(tasks)
        .await
        .into_iter()
        .map(|result| result.unwrap())
        .collect::<Vec<_>>();
    timings.sort_by_key(|timing| timing.completion);
    let mut first_delta = timings
        .iter()
        .map(|timing| timing.first_delta)
        .collect::<Vec<_>>();
    first_delta.sort();
    let completion = timings
        .iter()
        .map(|timing| timing.completion)
        .collect::<Vec<_>>();
    LoadReport {
        elapsed: started.elapsed(),
        first_delta_p95: percentile(&first_delta, 95),
        completion_p95: percentile(&completion, 95),
        completion_max: completion.last().copied().unwrap_or_default(),
    }
}

fn percentile(values: &[std::time::Duration], percentile: usize) -> std::time::Duration {
    if values.is_empty() {
        return std::time::Duration::default();
    }
    let idx = ((values.len() - 1) * percentile) / 100;
    values[idx]
}

#[derive(Debug)]
struct ProcessSnapshot {
    fd_count: usize,
    rss_kb: Option<u64>,
}

impl ProcessSnapshot {
    fn capture() -> Self {
        Self {
            fd_count: std::fs::read_dir("/dev/fd")
                .map(|entries| entries.count())
                .unwrap_or_default(),
            rss_kb: current_rss_kb(),
        }
    }
}

fn current_rss_kb() -> Option<u64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

async fn start_mock_upstream(app: Router) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

async fn admin_status(addr: std::net::SocketAddr) -> serde_json::Value {
    reqwest::Client::new()
        .get(format!("http://{addr}/admin/status"))
        .header("x-cc-codex-admin-token", "test-admin-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn admin_set_route(addr: std::net::SocketAddr, active_profile: &str) -> serde_json::Value {
    let response = reqwest::Client::new()
        .put(format!("http://{addr}/admin/route"))
        .header("x-cc-codex-admin-token", "test-admin-token")
        .json(&serde_json::json!({ "activeProfile": active_profile }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.unwrap()
}

async fn test_config(upstream: std::net::SocketAddr, path: &str) -> (AppConfig, AppPaths) {
    let dir = tempfile::tempdir().unwrap().keep();
    let paths = AppPaths {
        config_dir: dir.join("config"),
        logs_dir: dir.join("logs"),
        config_file: dir.join("config/config.json"),
        model_profiles_file: dir.join("config/model-profiles.json"),
        admin_token_file: dir.join("config/admin-token"),
        claude_shim_file: dir.join("config/claude-shim.json"),
        auth_file: dir.join("config/auth.json"),
        route_pins_file: dir.join("config/route-pins.json"),
        deepseek_api_key_file: dir.join("config/deepseek-api-key"),
        custom_openai_api_key_file: dir.join("config/custom-openai-api-key"),
    };
    let config = AppConfig {
        port: 0,
        admin_token: "test-admin-token".into(),
        codex: CodexConfig {
            transport: CodexTransport::Http,
            base_url: format!("http://{upstream}{path}"),
            ..Default::default()
        },
        ..Default::default()
    };
    (config, paths)
}

async fn test_deepseek_config(upstream: std::net::SocketAddr) -> (AppConfig, AppPaths) {
    let (mut config, paths) = test_config(upstream, "/unused").await;
    config.provider = Provider::DeepSeek;
    config.routing.active_profile = "deepseek".into();
    config.deepseek.base_url = format!("http://{upstream}/anthropic");
    store_api_key(&paths.deepseek_api_key_file, "deepseek-secret").unwrap();
    (config, paths)
}

async fn test_custom_openai_config(
    upstream: std::net::SocketAddr,
    protocol: CustomOpenAIProtocol,
    with_key: bool,
) -> (AppConfig, AppPaths) {
    let (mut config, paths) = test_config(upstream, "/unused").await;
    config.provider = Provider::CustomOpenAI;
    config.routing.active_profile = "custom-openai".into();
    config.custom_openai.base_url = format!("http://{upstream}/openai");
    config.custom_openai.protocol = protocol;
    if with_key {
        store_custom_openai_api_key(&paths.custom_openai_api_key_file, "custom-secret").unwrap();
    }
    (config, paths)
}

fn test_auth() -> AuthManager {
    AuthManager::new(
        Arc::new(MemoryTokenStore::with(StoredAuth {
            access: "access".into(),
            refresh: "refresh".into(),
            expires_at_ms: i64::MAX,
            account_id: Some("acct".into()),
        })),
        Arc::new(NoRefresh),
    )
}
