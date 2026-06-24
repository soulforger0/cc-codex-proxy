use async_trait::async_trait;
use axum::{
    body::Body,
    extract::State,
    http::{header, Response, StatusCode},
    routing::{get, post},
    Router,
};
use bytes::Bytes;
use futures_util::{future::join_all, StreamExt};
use proxy_core::{
    auth::{AuthManager, MemoryTokenStore, StoredAuth, TokenRefreshClient, TokenResponse},
    config::{AppConfig, AppPaths, CodexTransport},
    serve,
};
use std::{
    convert::Infallible,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
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
