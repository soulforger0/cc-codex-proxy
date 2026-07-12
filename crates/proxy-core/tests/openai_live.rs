use futures_util::StreamExt;
use proxy_core::{
    anthropic::schema::{AnthropicMessage, AnthropicRequest},
    auth::{AuthManager, FileTokenStore, OAuthRefreshClient},
    codex::{
        client::OpenAIResponsesClient,
        translate::{translate_request, ResponsesRequest},
    },
    config::{
        AppPaths, CodexConfig, CustomOpenAIConfig, DEFAULT_PUBLIC_PRIMARY_MODEL,
        DEFAULT_PUBLIC_SMALL_MODEL, DEFAULT_PUBLIC_SONNET_MODEL,
    },
    model::{default_profiles, ModelRegistry},
    routing::RouteSnapshot,
};
use serde_json::json;
use std::{fs, sync::Arc};

#[tokio::test]
#[ignore = "uses local ChatGPT OAuth and calls the live Codex endpoint"]
async fn live_codex_sol_terra_luna_and_plan_haiku() {
    let paths = AppPaths::discover().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let auth_file = temp.path().join("auth.json");
    fs::copy(&paths.auth_file, &auth_file).expect("local ChatGPT OAuth auth.json is required");

    let config = CodexConfig::default();
    let auth = AuthManager::new(
        Arc::new(FileTokenStore::new(auth_file)),
        Arc::new(
            OAuthRefreshClient::with_timeout(
                config.oauth_issuer.clone(),
                config.oauth_client_id.clone(),
                config.header_timeout_ms,
            )
            .unwrap(),
        ),
    );
    let client = OpenAIResponsesClient::new_codex(config, auth).unwrap();
    let registry = ModelRegistry::from_profiles(default_profiles());
    let route = RouteSnapshot {
        id: "codex".into(),
        provider: proxy_core::config::Provider::Codex,
        primary_model: "gpt-5.6-sol".into(),
        sonnet_model: "gpt-5.6-terra".into(),
        small_model: "gpt-5.6-luna".into(),
        context_window: 372_000,
    };

    for (alias, prompt) in [
        (DEFAULT_PUBLIC_PRIMARY_MODEL, "Reply exactly SOL_OK"),
        (DEFAULT_PUBLIC_SONNET_MODEL, "Reply exactly TERRA_OK"),
        (
            DEFAULT_PUBLIC_SMALL_MODEL,
            "Design patch release plan. Reply exactly LUNA_PLAN_OK",
        ),
    ] {
        let resolved = registry
            .resolve_for_route(
                &route,
                DEFAULT_PUBLIC_PRIMARY_MODEL,
                DEFAULT_PUBLIC_SONNET_MODEL,
                DEFAULT_PUBLIC_SMALL_MODEL,
                alias,
            )
            .unwrap();
        let session = format!("ccp-live-{}", resolved.upstream_model);
        let request = live_request(alias, prompt);
        let translated = translate_request(&request, &resolved, Some(&session)).unwrap();
        assert_responses_lite_body(&translated);
        let response = client.post(&translated, Some(&session)).await.unwrap();
        let mut body = response.body;
        let mut received = Vec::new();
        while let Some(chunk) = body.next().await {
            received.extend_from_slice(&chunk.unwrap());
        }
        let received = String::from_utf8_lossy(&received);
        assert!(received.contains("response.completed"), "{received}");
        assert!(!received.contains("model_not_found"), "{received}");
    }
}

#[tokio::test]
#[ignore = "requires CCP_LIVE_CUSTOM_OPENAI_BASE_URL and calls a live custom Responses endpoint"]
async fn live_custom_openai_sol_terra_luna() {
    let base_url = std::env::var("CCP_LIVE_CUSTOM_OPENAI_BASE_URL")
        .expect("CCP_LIVE_CUSTOM_OPENAI_BASE_URL is required");
    let temp = tempfile::tempdir().unwrap();
    let client = OpenAIResponsesClient::new_custom(
        CustomOpenAIConfig {
            base_url,
            ..Default::default()
        },
        temp.path().join("custom-openai-api-key"),
    )
    .unwrap();

    for model in ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"] {
        let resolved = proxy_core::model::ResolvedModel {
            provider: proxy_core::config::Provider::CustomOpenAI,
            requested: model.into(),
            public_id: model.into(),
            upstream_model: model.into(),
            service_tier: None,
            context_window: 372_000,
        };
        let session = format!("ccp-live-{model}");
        let translated = translate_request(
            &live_request(model, "Reply exactly CUSTOM_OK"),
            &resolved,
            Some(&session),
        )
        .unwrap();
        let response = client.post(&translated, Some(&session)).await.unwrap();
        let mut body = response.body;
        let mut received = Vec::new();
        while let Some(chunk) = body.next().await {
            received.extend_from_slice(&chunk.unwrap());
        }
        let received = String::from_utf8_lossy(&received);
        assert!(received.contains("response.completed"), "{received}");
        assert!(!received.contains("model_not_found"), "{received}");
    }
}

fn live_request(model: &str, prompt: &str) -> AnthropicRequest {
    AnthropicRequest {
        model: model.into(),
        max_tokens: Some(32),
        temperature: None,
        top_p: None,
        stream: Some(true),
        system: Some(json!("You are a concise Claude Code planning subagent.")),
        messages: vec![AnthropicMessage {
            role: "user".into(),
            content: json!(prompt),
            extra: Default::default(),
        }],
        tools: None,
        tool_choice: None,
        metadata: None,
        output_config: None,
        thinking: None,
        extra: Default::default(),
    }
}

fn assert_responses_lite_body(body: &ResponsesRequest) {
    assert_eq!(body.instructions.as_deref(), Some(""));
    assert!(body.tools.is_none());
    assert_eq!(body.parallel_tool_calls, Some(false));
    assert_eq!(body.reasoning.as_ref().unwrap()["context"], "all_turns");
    assert_eq!(body.input[0]["type"], "additional_tools");
}
