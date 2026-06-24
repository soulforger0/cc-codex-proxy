use async_trait::async_trait;
use axum::{
    body::Body,
    http::{header, Response, StatusCode},
    routing::post,
    Router,
};
use proxy_core::{
    auth::{AuthManager, MemoryTokenStore, StoredAuth, TokenRefreshClient, TokenResponse},
    config::{AppConfig, AppPaths, CodexTransport},
    serve,
};
use std::sync::Arc;

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
    assert!(response.json::<serde_json::Value>().await.unwrap()["input_tokens"].as_u64().unwrap() > 0);
    server.stop().await;
}

#[tokio::test]
async fn upstream_429_is_preserved() {
    let upstream = start_mock_upstream(
        Router::new().route(
            "/rate-limit",
            post(|| async {
                Response::builder()
                    .status(StatusCode::TOO_MANY_REQUESTS)
                    .header(header::RETRY_AFTER, "5")
                    .body(Body::from("slow down"))
                    .unwrap()
            }),
        ),
    )
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

async fn start_mock_upstream(app: Router) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

async fn test_config(upstream: std::net::SocketAddr, path: &str) -> (AppConfig, AppPaths) {
    let dir = tempfile::tempdir().unwrap().into_path();
    let paths = AppPaths {
        config_dir: dir.join("config"),
        logs_dir: dir.join("logs"),
        config_file: dir.join("config/config.json"),
        model_profiles_file: dir.join("config/model-profiles.json"),
        admin_token_file: dir.join("config/admin-token"),
    };
    let mut config = AppConfig::default();
    config.port = 0;
    config.admin_token = "test-admin-token".into();
    config.codex.transport = CodexTransport::Http;
    config.codex.base_url = format!("http://{upstream}{path}");
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
