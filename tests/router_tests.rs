/// Integration tests for the Router (model resolution + fallback chain).
///
/// These tests validate the routing logic without making real network calls.
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use bytes::Bytes;
use dashmap::DashMap;
use futures::StreamExt;

use free_agent_gateway::config::{
    AgentConfig, Config, KeyConfig, KeyTier, ModelAlias, ProviderConfig, ProviderType,
    RoutingConfig, RoutingStrategy, ServerConfig,
};
use free_agent_gateway::error::{GatewayError, GatewayResult};
use free_agent_gateway::keyhub::{KeyHub, key_fingerprint};
use free_agent_gateway::models::{ChatCompletionRequest, ChatMessage, KeyStatus};
use free_agent_gateway::providers::BoxedProvider;
use free_agent_gateway::providers::openai_compatible::OpenAiCompatibleProvider;
use free_agent_gateway::providers::traits::{ChatResponse, Provider, StreamResponse};
use free_agent_gateway::router::{ResolvedRoute, Router};
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};

/// Helper to create a Router with an empty disabled_models map.
fn build_router(
    config: Arc<Config>,
    providers: Arc<DashMap<String, BoxedProvider>>,
    keyhub: Arc<KeyHub>,
) -> Router {
    let disabled_models = Arc::new(RwLock::new(HashMap::<String, HashSet<String>>::new()));
    Router::new(config, providers, keyhub, disabled_models, None)
}

fn make_config() -> Config {
    let mut models = HashMap::new();
    models.insert(
        "coding".into(),
        ModelAlias {
            provider: "github".into(),
            model: "openai/gpt-4.1-mini".into(),
        },
    );
    models.insert(
        "chat".into(),
        ModelAlias {
            provider: "nvidia".into(),
            model: "meta/llama-3.1-70b-instruct".into(),
        },
    );
    models.insert(
        "local".into(),
        ModelAlias {
            provider: "ollama".into(),
            model: "qwen2.5:7b".into(),
        },
    );

    let mut agents = HashMap::new();
    agents.insert(
        "hermes".into(),
        AgentConfig {
            default_model: "coding".into(),
        },
    );
    agents.insert(
        "openclaw".into(),
        AgentConfig {
            default_model: "chat".into(),
        },
    );
    agents.insert(
        "document".into(),
        AgentConfig {
            default_model: "local".into(),
        },
    );

    let mut providers = HashMap::new();
    providers.insert(
        "github".into(),
        ProviderConfig {
            provider_type: ProviderType::GithubModels,
            enabled: true,
            base_url: "https://models.inference.ai.azure.com".into(),
            proxy_url: None,
            keys: vec!["key1".into(), "key2".into()],
            health_check_model: "openai/gpt-4.1-mini".into(),
            timeout_seconds: 30,
            priority: 0,
        },
    );
    providers.insert(
        "nvidia".into(),
        ProviderConfig {
            provider_type: ProviderType::Nvidia,
            enabled: true,
            base_url: "https://integrate.api.nvidia.com/v1".into(),
            proxy_url: None,
            keys: vec!["nvkey".into()],
            health_check_model: "meta/llama-3.1-70b-instruct".into(),
            timeout_seconds: 30,
            priority: 0,
        },
    );
    providers.insert(
        "ollama".into(),
        ProviderConfig {
            provider_type: ProviderType::Ollama,
            enabled: true,
            base_url: "http://localhost:11434".into(),
            proxy_url: None,
            keys: vec!["ollama".into()],
            health_check_model: "qwen2.5:7b".into(),
            timeout_seconds: 120,
            priority: 100,
        },
    );

    Config {
        server: ServerConfig {
            host: "127.0.0.1".into(),
            port: 9000,
            log_level: "info".into(),
            request_timeout: 120,
            sse_keepalive: 15,
        },
        routing: RoutingConfig {
            strategy: RoutingStrategy::LeastFailed,
            fail_threshold: 3,
            cooldown_seconds: 60,
            auto_discover: true,
        },
        fallback: vec!["github".into(), "nvidia".into(), "ollama".into()],
        agents,
        models,
        providers,
        watcher: Default::default(),
        state: Default::default(),
        cors: Default::default(),
        adaptive_routing: Default::default(),
        context_compression: Default::default(),
    }
}

fn make_router(config: Config) -> (Arc<Config>, Router) {
    let config = Arc::new(config);
    let providers = Arc::new(DashMap::new());
    let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
    let router = build_router(config.clone(), providers, keyhub);
    (config, router)
}

fn chat_request(model: &str, stream: bool) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: model.into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: serde_json::json!("hello"),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            extra: serde_json::Map::new(),
        }],
        temperature: None,
        top_p: None,
        n: None,
        stream: Some(stream),
        stop: None,
        max_tokens: None,
        presence_penalty: None,
        frequency_penalty: None,
        user: None,
        request_id: None,
        agent_name: None,
        extra: serde_json::Map::new(),
    }
}

fn sse(json: serde_json::Value) -> Bytes {
    Bytes::from(format!("data: {}\n\n", json))
}

#[derive(Debug)]
struct TestStreamProvider {
    name: String,
    fail_in_body: bool,
}

#[derive(Debug)]
struct RecordingProvider {
    name: String,
    calls: Arc<Mutex<Vec<(String, String)>>>,
    fail_keys: Vec<String>,
}

#[derive(Debug)]
struct StaticBodyProvider {
    name: String,
    calls: Arc<Mutex<Vec<(String, String)>>>,
    body: serde_json::Value,
}

#[derive(Debug)]
struct StaticStreamProvider {
    name: String,
    chunks: Vec<Bytes>,
}

#[derive(Debug)]
struct DiscoveryFailProvider {
    name: String,
}

#[async_trait]
impl Provider for DiscoveryFailProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn base_url(&self) -> &str {
        "http://discovery-fail"
    }

    async fn list_models(&self, _api_key: &str) -> GatewayResult<Vec<String>> {
        Err(GatewayError::UpstreamError(
            "error decoding response body".into(),
        ))
    }

    async fn chat(
        &self,
        _api_key: &str,
        _request: ChatCompletionRequest,
    ) -> GatewayResult<ChatResponse> {
        unreachable!("discovery failure test should not reach chat")
    }

    async fn chat_stream(
        &self,
        _api_key: &str,
        _request: ChatCompletionRequest,
    ) -> GatewayResult<StreamResponse> {
        unreachable!()
    }

    async fn health_check(&self, _api_key: &str) -> GatewayResult<u64> {
        Ok(1)
    }

    fn health_check_model(&self) -> &str {
        "working-model"
    }

    fn timeout_seconds(&self) -> u64 {
        5
    }

    fn priority(&self) -> u32 {
        0
    }
}

#[async_trait]
impl Provider for StaticBodyProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn base_url(&self) -> &str {
        "http://static"
    }

    async fn list_models(&self, _api_key: &str) -> GatewayResult<Vec<String>> {
        Ok(vec![])
    }

    async fn chat(
        &self,
        api_key: &str,
        request: ChatCompletionRequest,
    ) -> GatewayResult<ChatResponse> {
        self.calls
            .lock()
            .unwrap()
            .push((self.name.clone(), format!("{api_key}:{}", request.model)));
        Ok(ChatResponse {
            body: self.body.clone(),
            status: 200,
        })
    }

    async fn chat_stream(
        &self,
        _api_key: &str,
        _request: ChatCompletionRequest,
    ) -> GatewayResult<StreamResponse> {
        unreachable!()
    }

    async fn health_check(&self, _api_key: &str) -> GatewayResult<u64> {
        Ok(1)
    }

    fn health_check_model(&self) -> &str {
        "test-model"
    }

    fn timeout_seconds(&self) -> u64 {
        5
    }

    fn priority(&self) -> u32 {
        0
    }
}

#[async_trait]
impl Provider for StaticStreamProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn base_url(&self) -> &str {
        "http://static-stream"
    }

    async fn list_models(&self, _api_key: &str) -> GatewayResult<Vec<String>> {
        Ok(vec!["test-model".into()])
    }

    async fn chat(
        &self,
        _api_key: &str,
        _request: ChatCompletionRequest,
    ) -> GatewayResult<ChatResponse> {
        unreachable!("stream provider does not serve non-stream chat")
    }

    async fn chat_stream(
        &self,
        _api_key: &str,
        _request: ChatCompletionRequest,
    ) -> GatewayResult<StreamResponse> {
        let chunks = self.chunks.clone();
        Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
    }

    async fn health_check(&self, _api_key: &str) -> GatewayResult<u64> {
        Ok(1)
    }

    fn health_check_model(&self) -> &str {
        "test-model"
    }

    fn timeout_seconds(&self) -> u64 {
        5
    }

    fn priority(&self) -> u32 {
        0
    }
}

#[async_trait]
impl Provider for RecordingProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn base_url(&self) -> &str {
        "http://recording"
    }

    async fn list_models(&self, _api_key: &str) -> GatewayResult<Vec<String>> {
        Ok(vec![])
    }

    async fn chat(
        &self,
        api_key: &str,
        request: ChatCompletionRequest,
    ) -> GatewayResult<ChatResponse> {
        self.calls
            .lock()
            .unwrap()
            .push((api_key.to_string(), request.model.clone()));
        if self.fail_keys.iter().any(|key| key == api_key) {
            return Err(GatewayError::HttpError {
                status: 503,
                message: "failed".into(),
                retry_after_seconds: None,
            });
        }
        Ok(ChatResponse {
            body: serde_json::json!({
                "id": "recorded",
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "ok"
                    }
                }]
            }),
            status: 200,
        })
    }

    async fn chat_stream(
        &self,
        _api_key: &str,
        _request: ChatCompletionRequest,
    ) -> GatewayResult<StreamResponse> {
        unreachable!()
    }

    async fn health_check(&self, _api_key: &str) -> GatewayResult<u64> {
        Ok(1)
    }

    fn health_check_model(&self) -> &str {
        "wrong-fallback-model"
    }

    fn timeout_seconds(&self) -> u64 {
        5
    }

    fn priority(&self) -> u32 {
        0
    }
}

#[derive(Debug)]
struct FailingStatusProvider {
    name: String,
    calls: Arc<Mutex<Vec<(String, String)>>>,
    fail_keys: Vec<String>,
    fail_status: u16,
}

#[async_trait]
impl Provider for FailingStatusProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn base_url(&self) -> &str {
        "http://failing-status"
    }

    async fn list_models(&self, _api_key: &str) -> GatewayResult<Vec<String>> {
        Ok(vec![])
    }

    async fn chat(
        &self,
        api_key: &str,
        request: ChatCompletionRequest,
    ) -> GatewayResult<ChatResponse> {
        self.calls
            .lock()
            .unwrap()
            .push((api_key.to_string(), request.model.clone()));
        if self.fail_keys.iter().any(|key| key == api_key) {
            return Err(GatewayError::http_error(
                self.fail_status,
                "rate limit exceeded",
                Some(60),
            ));
        }
        Ok(ChatResponse {
            body: serde_json::json!({
                "id": "recorded",
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "ok"
                    }
                }]
            }),
            status: 200,
        })
    }

    async fn chat_stream(
        &self,
        _api_key: &str,
        _request: ChatCompletionRequest,
    ) -> GatewayResult<StreamResponse> {
        unreachable!()
    }

    async fn health_check(&self, _api_key: &str) -> GatewayResult<u64> {
        Ok(1)
    }

    fn health_check_model(&self) -> &str {
        "wrong-fallback-model"
    }

    fn timeout_seconds(&self) -> u64 {
        5
    }

    fn priority(&self) -> u32 {
        0
    }
}

fn tiered_key(value: &str, tier: KeyTier) -> KeyConfig {
    KeyConfig::detailed(value, tier)
}

#[async_trait]
impl Provider for TestStreamProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn base_url(&self) -> &str {
        "http://test"
    }

    async fn list_models(&self, _api_key: &str) -> GatewayResult<Vec<String>> {
        Ok(vec!["test-model".into()])
    }

    async fn chat(
        &self,
        _api_key: &str,
        _request: ChatCompletionRequest,
    ) -> GatewayResult<ChatResponse> {
        unreachable!("stream tests do not call non-stream chat")
    }

    async fn chat_stream(
        &self,
        _api_key: &str,
        _request: ChatCompletionRequest,
    ) -> GatewayResult<StreamResponse> {
        let mut chunks = vec![Ok(Bytes::from_static(b"data: first\n\n"))];
        if self.fail_in_body {
            chunks.push(Err(GatewayError::UpstreamError("stream broke".into())));
        }
        Ok(Box::pin(futures::stream::iter(chunks)))
    }

    async fn health_check(&self, _api_key: &str) -> GatewayResult<u64> {
        Ok(1)
    }

    fn health_check_model(&self) -> &str {
        "test-model"
    }

    fn timeout_seconds(&self) -> u64 {
        5
    }

    fn priority(&self) -> u32 {
        0
    }
}

#[test]
fn test_resolve_coding_alias() {
    let (_, router) = make_router(make_config());
    let route = router.resolve("coding", None).unwrap();
    assert_eq!(route.provider_name, "github");
    assert_eq!(route.model, "openai/gpt-4.1-mini");
}

#[test]
fn test_resolve_chat_alias() {
    let (_, router) = make_router(make_config());
    let route = router.resolve("chat", None).unwrap();
    assert_eq!(route.provider_name, "nvidia");
    assert_eq!(route.model, "meta/llama-3.1-70b-instruct");
}

#[test]
fn test_resolve_local_alias() {
    let (_, router) = make_router(make_config());
    let route = router.resolve("local", None).unwrap();
    assert_eq!(route.provider_name, "ollama");
    assert_eq!(route.model, "qwen2.5:7b");
}

#[test]
fn test_resolve_agent_hermes() {
    let (_, router) = make_router(make_config());
    let route = router.resolve("coding", Some("hermes")).unwrap();
    assert_eq!(route.provider_name, "github");
}

#[test]
fn test_resolve_agent_openclaw() {
    let (_, router) = make_router(make_config());
    let route = router.resolve("chat", Some("openclaw")).unwrap();
    assert_eq!(route.provider_name, "nvidia");
}

#[test]
fn test_resolve_agent_document() {
    let (_, router) = make_router(make_config());
    let route = router.resolve("local", Some("document")).unwrap();
    assert_eq!(route.provider_name, "ollama");
}

#[test]
fn test_resolve_unknown_model_no_providers_returns_error() {
    let (_, router) = make_router(make_config());
    let result = router.resolve("totally-unknown-model", None);
    assert!(result.is_err());
}

#[test]
fn test_build_fallback_chain_order() {
    let (_, router) = make_router(make_config());
    let chain = router.build_provider_chain("github");
    assert_eq!(chain, vec!["github", "nvidia", "ollama"]);

    let chain = router.build_provider_chain("nvidia");
    assert_eq!(chain, vec!["nvidia", "github", "ollama"]);

    let chain = router.build_provider_chain("ollama");
    assert_eq!(chain, vec!["ollama", "github", "nvidia"]);
}

#[test]
fn test_model_for_fallback_provider_preserves_exact_model() {
    let config = make_config();
    let (_, router) = make_router(config);

    let route = ResolvedRoute {
        provider_name: "github".into(),
        model: "openai/gpt-4.1-mini".into(),
    };
    // For the primary provider, returns the route model
    let m = router.model_for_provider("github", &route);
    assert_eq!(m, "openai/gpt-4.1-mini");

    // Fallback must never substitute a different model.
    let m = router.model_for_provider("ollama", &route);
    assert_eq!(m, "openai/gpt-4.1-mini");
}

#[test]
fn test_resolve_openrouter_free_model_preserves_suffix() {
    let mut config = make_config();
    config
        .fallback
        .extend(["opencode".into(), "openrouter".into()]);
    let provider_config = ProviderConfig {
        provider_type: ProviderType::OpenaiCompatible,
        enabled: true,
        base_url: "https://openrouter.ai/api/v1".into(),
        proxy_url: None,
        keys: vec!["or-key".into()],
        health_check_model: "qwen/qwen3-coder:free".into(),
        timeout_seconds: 30,
        priority: 0,
    };
    config
        .providers
        .insert("openrouter".into(), provider_config.clone());

    let config = Arc::new(config);
    let providers = Arc::new(DashMap::new());
    providers.insert(
        "openrouter".into(),
        Box::new(OpenAiCompatibleProvider::new(
            "openrouter",
            &provider_config,
        )) as BoxedProvider,
    );
    providers.insert(
        "github".into(),
        Box::new(OpenAiCompatibleProvider::new("github", &provider_config)) as BoxedProvider,
    );
    let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
    let router = build_router(config, providers, keyhub);

    let route = router.resolve("qwen/qwen3-coder:free", None).unwrap();
    assert_eq!(route.provider_name, "openrouter");
    assert_eq!(route.model, "qwen/qwen3-coder:free");

    let route = router
        .resolve("openrouter/qwen/qwen3-coder:free", None)
        .unwrap();
    assert_eq!(route.provider_name, "openrouter");
    assert_eq!(route.model, "qwen/qwen3-coder:free");
}

#[tokio::test]
async fn test_non_stream_request_falls_back_and_records_first_failure() {
    let mut first = mockito::Server::new_async().await;
    first
        .mock("POST", "/chat/completions")
        .with_status(503)
        .with_header("content-type", "application/json")
        .with_body(r#"{"error":{"message":"temporarily unavailable"}}"#)
        .create_async()
        .await;

    let mut second = mockito::Server::new_async().await;
    second
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"id":"chatcmpl-fallback","choices":[{"message":{"role":"assistant","content":"ok"}}]}"#,
        )
        .create_async()
        .await;

    let mut config = make_config();
    config.fallback = vec!["first".into(), "second".into()];
    config.models.insert(
        "chat".into(),
        ModelAlias {
            provider: "first".into(),
            model: "test-model".into(),
        },
    );
    config.providers.clear();
    for (name, base_url) in [("first", first.url()), ("second", second.url())] {
        config.providers.insert(
            name.into(),
            ProviderConfig {
                provider_type: ProviderType::OpenaiCompatible,
                enabled: true,
                base_url,
                proxy_url: None,
                keys: vec![tiered_key(&format!("{name}-key"), KeyTier::Free)],
                health_check_model: "test-model".into(),
                timeout_seconds: 5,
                priority: 0,
            },
        );
    }

    let config = Arc::new(config);
    let providers = Arc::new(DashMap::new());
    let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
    for name in ["first", "second"] {
        let provider_config = config.providers.get(name).unwrap();
        providers.insert(
            name.into(),
            Box::new(OpenAiCompatibleProvider::new(name, provider_config))
                as free_agent_gateway::providers::BoxedProvider,
        );
        keyhub.register_provider(name, provider_config.keys.clone());
        keyhub.update_models(name, &format!("{name}-key"), vec!["test-model".into()]);
    }
    let router = build_router(config, providers, keyhub.clone());

    let response = router.chat(&chat_request("chat", false)).await.unwrap();

    assert_eq!(response.body["id"], "chatcmpl-fallback");
    let first_state = keyhub
        .snapshot()
        .into_iter()
        .find(|(name, _)| name == "first")
        .unwrap()
        .1
        .remove(0);
    assert_eq!(first_state.fail_count, 1);
    assert_eq!(first_state.total_fail_count, 1);
}

#[tokio::test]
async fn test_non_stream_empty_completion_falls_back() {
    let mut config = make_config();
    config.fallback = vec!["first".into(), "second".into()];
    config.providers = HashMap::from([
        (
            "first".into(),
            ProviderConfig {
                provider_type: ProviderType::OpenaiCompatible,
                enabled: true,
                base_url: "http://first".into(),
                proxy_url: None,
                keys: vec![tiered_key("first-key", KeyTier::Free)],
                health_check_model: "test-model".into(),
                timeout_seconds: 5,
                priority: 0,
            },
        ),
        (
            "second".into(),
            ProviderConfig {
                provider_type: ProviderType::OpenaiCompatible,
                enabled: true,
                base_url: "http://second".into(),
                proxy_url: None,
                keys: vec![tiered_key("second-key", KeyTier::Free)],
                health_check_model: "test-model".into(),
                timeout_seconds: 5,
                priority: 0,
            },
        ),
    ]);
    let config = Arc::new(config);
    let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
    for name in ["first", "second"] {
        keyhub.register_provider(name, config.providers[name].keys.clone());
        keyhub.update_models(name, &format!("{name}-key"), vec!["test-model".into()]);
    }
    let calls = Arc::new(Mutex::new(Vec::new()));
    let providers = Arc::new(DashMap::new());
    providers.insert(
        "first".into(),
        Box::new(StaticBodyProvider {
            name: "first".into(),
            calls: calls.clone(),
            body: serde_json::json!({
                "id": "empty",
                "choices": [{"message": {"role": "assistant", "content": ""}, "finish_reason": "stop"}]
            }),
        }) as BoxedProvider,
    );
    providers.insert(
        "second".into(),
        Box::new(StaticBodyProvider {
            name: "second".into(),
            calls: calls.clone(),
            body: serde_json::json!({
                "id": "ok",
                "choices": [{"message": {"role": "assistant", "content": "fallback ok"}, "finish_reason": "stop"}]
            }),
        }) as BoxedProvider,
    );
    let router = Router::new(
        config,
        providers,
        keyhub,
        Arc::new(RwLock::new(HashMap::new())),
        None,
    );

    let response = router
        .chat(&chat_request("test-model", false))
        .await
        .unwrap();

    assert_eq!(
        response.body["choices"][0]["message"]["content"],
        "fallback ok"
    );
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, "first");
    assert_eq!(calls[1].0, "second");
}

#[tokio::test]
async fn test_model_discovery_failure_does_not_cooldown_key() {
    let mut config = make_config();
    config.fallback = vec!["opencode".into()];
    config.providers = HashMap::from([(
        "opencode".into(),
        ProviderConfig {
            provider_type: ProviderType::OpenaiCompatible,
            enabled: true,
            base_url: "http://opencode".into(),
            proxy_url: None,
            keys: vec![tiered_key("opencode-key", KeyTier::Free)],
            health_check_model: "working-model".into(),
            timeout_seconds: 5,
            priority: 0,
        },
    )]);
    let config = Arc::new(config);
    let providers = Arc::new(DashMap::new());
    providers.insert(
        "opencode".into(),
        Box::new(DiscoveryFailProvider {
            name: "opencode".into(),
        }) as BoxedProvider,
    );
    let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
    keyhub.register_provider("opencode", config.providers["opencode"].keys.clone());
    let router = build_router(config, providers, keyhub.clone());

    let result = router
        .chat(&chat_request("opencode/some-model", false))
        .await;

    assert!(matches!(result, Err(GatewayError::ModelNotFound(_))));
    let snapshot = keyhub.snapshot();
    let key = snapshot
        .iter()
        .find(|(provider, _)| provider == "opencode")
        .unwrap()
        .1
        .iter()
        .find(|key| key.key_id == key_fingerprint("opencode-key"))
        .unwrap();
    assert_eq!(key.status, KeyStatus::Available);
    assert_eq!(key.fail_count, 0);
    assert_eq!(key.total_fail_count, 0);
}

#[tokio::test]
async fn test_openrouter_free_model_uses_openrouter_key_without_inventory_cache() {
    let mut config = make_config();
    config.fallback = vec!["openrouter".into(), "nvidia".into()];
    config.providers.insert(
        "openrouter".into(),
        ProviderConfig {
            provider_type: ProviderType::OpenaiCompatible,
            enabled: true,
            base_url: "https://openrouter.ai/api/v1".into(),
            proxy_url: None,
            keys: vec![KeyConfig::detailed("or-key", KeyTier::Free)],
            health_check_model: "google/gemma-4-31b-it:free".into(),
            timeout_seconds: 30,
            priority: 0,
        },
    );

    let config = Arc::new(config);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let providers = Arc::new(DashMap::new());
    providers.insert(
        "openrouter".into(),
        Box::new(RecordingProvider {
            name: "openrouter".into(),
            calls: calls.clone(),
            fail_keys: vec![],
        }) as BoxedProvider,
    );
    let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
    keyhub.register_provider(
        "openrouter",
        vec![KeyConfig::detailed("or-key", KeyTier::Free)],
    );
    let router = build_router(config, providers, keyhub);

    let response = router
        .chat(&chat_request("google/gemma-4-31b-it:free", false))
        .await
        .unwrap();

    assert_eq!(response.status, 200);
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &[(
            "or-key".to_string(),
            "google/gemma-4-31b-it:free".to_string()
        )]
    );
}

#[tokio::test]
async fn test_canonical_model_routes_to_openrouter_free_variant() {
    let mut config = make_config();
    config.fallback = vec!["openrouter".into()];
    config.providers = HashMap::from([(
        "openrouter".into(),
        ProviderConfig {
            provider_type: ProviderType::OpenaiCompatible,
            enabled: true,
            base_url: "http://openrouter".into(),
            proxy_url: None,
            keys: vec![KeyConfig::detailed("or-free", KeyTier::Free)],
            health_check_model: "google/gemma-4-31b-it:free".into(),
            timeout_seconds: 5,
            priority: 0,
        },
    )]);
    let config = Arc::new(config);
    let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
    keyhub.register_provider("openrouter", config.providers["openrouter"].keys.clone());
    keyhub.update_models(
        "openrouter",
        "or-free",
        vec!["google/gemma-4-31b-it:free".into()],
    );
    let calls = Arc::new(Mutex::new(Vec::new()));
    let providers = Arc::new(DashMap::new());
    providers.insert(
        "openrouter".into(),
        Box::new(RecordingProvider {
            name: "openrouter".into(),
            calls: calls.clone(),
            fail_keys: vec![],
        }) as BoxedProvider,
    );
    let router = Router::new(
        config,
        providers,
        keyhub,
        Arc::new(RwLock::new(HashMap::new())),
        None,
    );

    router
        .chat(&chat_request("google/gemma-4-31b-it", false))
        .await
        .unwrap();

    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &[(
            "or-free".to_string(),
            "google/gemma-4-31b-it:free".to_string()
        )]
    );
}

#[tokio::test]
async fn test_openrouter_free_model_without_available_key_is_not_model_not_found() {
    let mut config = make_config();
    config.fallback = vec!["openrouter".into(), "nvidia".into()];
    config.providers.insert(
        "openrouter".into(),
        ProviderConfig {
            provider_type: ProviderType::OpenaiCompatible,
            enabled: true,
            base_url: "https://openrouter.ai/api/v1".into(),
            proxy_url: None,
            keys: vec![KeyConfig::detailed("or-key", KeyTier::Free)],
            health_check_model: "google/gemma-4-31b-it:free".into(),
            timeout_seconds: 30,
            priority: 0,
        },
    );

    let config = Arc::new(config);
    let providers = Arc::new(DashMap::new());
    providers.insert(
        "openrouter".into(),
        Box::new(RecordingProvider {
            name: "openrouter".into(),
            calls: Arc::new(Mutex::new(Vec::new())),
            fail_keys: vec![],
        }) as BoxedProvider,
    );
    let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
    keyhub.register_provider(
        "openrouter",
        vec![KeyConfig::detailed("or-key", KeyTier::Free)],
    );
    keyhub.report_failure_with_retry_after("openrouter", "or-key", 429, Some(60));
    let router = build_router(config, providers, keyhub);

    let result = router
        .chat_stream(&chat_request("google/gemma-4-31b-it:free", true))
        .await;

    assert!(matches!(
        result,
        Err(GatewayError::NoAvailableKeys(provider)) if provider == "openrouter"
    ));
}

#[tokio::test]
async fn test_rate_limited_key_is_removed_and_next_key_takes_over() {
    let mut config = make_config();
    config.fallback = vec!["openrouter".into()];
    config.providers.insert(
        "openrouter".into(),
        ProviderConfig {
            provider_type: ProviderType::OpenaiCompatible,
            enabled: true,
            base_url: "https://openrouter.ai/api/v1".into(),
            proxy_url: None,
            keys: vec![
                KeyConfig::detailed("limited-key", KeyTier::Free),
                KeyConfig::detailed("healthy-key", KeyTier::Free),
            ],
            health_check_model: "google/gemma-4-31b-it:free".into(),
            timeout_seconds: 30,
            priority: 0,
        },
    );

    let config = Arc::new(config);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let providers = Arc::new(DashMap::new());
    providers.insert(
        "openrouter".into(),
        Box::new(FailingStatusProvider {
            name: "openrouter".into(),
            calls: calls.clone(),
            fail_keys: vec!["limited-key".into()],
            fail_status: 429,
        }) as BoxedProvider,
    );
    let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
    keyhub.register_provider(
        "openrouter",
        vec![
            KeyConfig::detailed("limited-key", KeyTier::Free),
            KeyConfig::detailed("healthy-key", KeyTier::Free),
        ],
    );
    let router = build_router(config, providers, keyhub.clone());

    let response = router
        .chat(&chat_request("google/gemma-4-31b-it:free", false))
        .await
        .unwrap();

    assert_eq!(response.status, 200);
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &[
            (
                "limited-key".to_string(),
                "google/gemma-4-31b-it:free".to_string()
            ),
            (
                "healthy-key".to_string(),
                "google/gemma-4-31b-it:free".to_string()
            )
        ]
    );
    let snapshot = keyhub.snapshot();
    let openrouter = snapshot
        .iter()
        .find(|(provider, _)| provider == "openrouter")
        .map(|(_, keys)| keys)
        .unwrap();
    let limited = openrouter
        .iter()
        .find(|key| key.key_id == key_fingerprint("limited-key"))
        .unwrap();
    assert_eq!(
        limited.status,
        free_agent_gateway::models::KeyStatus::RateLimited
    );
    assert!(!keyhub.reserve_key("openrouter", "limited-key"));
}

#[tokio::test]
async fn test_stream_success_is_recorded_only_after_body_completes() {
    let mut config = make_config();
    config.fallback = vec!["streamer".into()];
    config.models.insert(
        "stream-test".into(),
        ModelAlias {
            provider: "streamer".into(),
            model: "test-model".into(),
        },
    );
    let config = Arc::new(config);
    let providers = Arc::new(DashMap::new());
    providers.insert(
        "streamer".into(),
        Box::new(TestStreamProvider {
            name: "streamer".into(),
            fail_in_body: false,
        }) as free_agent_gateway::providers::BoxedProvider,
    );
    let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
    keyhub.register_provider("streamer", vec![tiered_key("stream-key", KeyTier::Free)]);
    keyhub.update_models("streamer", "stream-key", vec!["test-model".into()]);
    let router = build_router(config, providers, keyhub.clone());

    let mut stream = router
        .chat_stream(&chat_request("stream-test", true))
        .await
        .unwrap();
    let before = keyhub.snapshot().remove(0).1.remove(0);
    assert_eq!(before.success_count, 0);

    while stream.next().await.is_some() {}

    let after = keyhub.snapshot().remove(0).1.remove(0);
    assert_eq!(after.success_count, 1);
    assert_eq!(after.total_fail_count, 0);
}

#[tokio::test]
async fn test_stream_body_error_records_failure_without_success() {
    let mut config = make_config();
    config.fallback = vec!["streamer".into()];
    config.models.insert(
        "stream-test".into(),
        ModelAlias {
            provider: "streamer".into(),
            model: "test-model".into(),
        },
    );
    let config = Arc::new(config);
    let providers = Arc::new(DashMap::new());
    providers.insert(
        "streamer".into(),
        Box::new(TestStreamProvider {
            name: "streamer".into(),
            fail_in_body: true,
        }) as free_agent_gateway::providers::BoxedProvider,
    );
    let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
    keyhub.register_provider("streamer", vec![tiered_key("stream-key", KeyTier::Free)]);
    keyhub.update_models("streamer", "stream-key", vec!["test-model".into()]);
    let router = build_router(config, providers, keyhub.clone());

    let results: Vec<_> = router
        .chat_stream(&chat_request("stream-test", true))
        .await
        .unwrap()
        .collect()
        .await;

    assert!(results.iter().any(Result::is_err));
    let state = keyhub.snapshot().remove(0).1.remove(0);
    assert_eq!(state.success_count, 0);
    assert_eq!(state.fail_count, 1);
    assert_eq!(state.total_fail_count, 1);
    assert_eq!(state.status, KeyStatus::Cooldown);
    assert!(!keyhub.reserve_key("streamer", "stream-key"));
}

#[tokio::test]
async fn test_stream_fragmented_tool_call_arguments_success_records_success() {
    let mut config = make_config();
    config.fallback = vec!["streamer".into()];
    config.models.insert(
        "stream-test".into(),
        ModelAlias {
            provider: "streamer".into(),
            model: "test-model".into(),
        },
    );
    let config = Arc::new(config);
    let providers = Arc::new(DashMap::new());
    providers.insert(
        "streamer".into(),
        Box::new(StaticStreamProvider {
            name: "streamer".into(),
            chunks: vec![
                sse(serde_json::json!({
                    "choices": [{
                        "delta": {
                            "tool_calls": [{
                                "index": 0,
                                "id": "call_1",
                                "type": "function",
                                "function": {
                                    "name": "lookup",
                                    "arguments": "{\"city\""
                                }
                            }]
                        }
                    }]
                })),
                sse(serde_json::json!({
                    "choices": [{
                        "delta": {
                            "tool_calls": [{
                                "index": 0,
                                "function": {
                                    "arguments": ":\"Shanghai\"}"
                                }
                            }]
                        },
                        "finish_reason": "tool_calls"
                    }]
                })),
            ],
        }) as free_agent_gateway::providers::BoxedProvider,
    );
    let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
    keyhub.register_provider("streamer", vec![tiered_key("stream-key", KeyTier::Free)]);
    keyhub.update_models("streamer", "stream-key", vec!["test-model".into()]);
    let router = build_router(config, providers, keyhub.clone());

    let results: Vec<_> = router
        .chat_stream(&chat_request("stream-test", true))
        .await
        .unwrap()
        .collect()
        .await;

    assert!(results.iter().all(Result::is_ok));
    let state = keyhub.snapshot().remove(0).1.remove(0);
    assert_eq!(state.success_count, 1);
    assert_eq!(state.total_fail_count, 0);
}

#[tokio::test]
async fn test_stream_incomplete_tool_call_arguments_records_failure() {
    let mut config = make_config();
    config.fallback = vec!["streamer".into()];
    config.models.insert(
        "stream-test".into(),
        ModelAlias {
            provider: "streamer".into(),
            model: "test-model".into(),
        },
    );
    let config = Arc::new(config);
    let providers = Arc::new(DashMap::new());
    providers.insert(
        "streamer".into(),
        Box::new(StaticStreamProvider {
            name: "streamer".into(),
            chunks: vec![
                sse(serde_json::json!({
                    "choices": [{
                        "delta": {
                            "tool_calls": [{
                                "index": 0,
                                "id": "call_1",
                                "type": "function",
                                "function": {
                                    "name": "lookup",
                                    "arguments": "{\"city\""
                                }
                            }]
                        }
                    }]
                })),
                sse(serde_json::json!({
                    "choices": [{
                        "delta": {},
                        "finish_reason": "tool_calls"
                    }]
                })),
            ],
        }) as free_agent_gateway::providers::BoxedProvider,
    );
    let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
    keyhub.register_provider("streamer", vec![tiered_key("stream-key", KeyTier::Free)]);
    keyhub.update_models("streamer", "stream-key", vec!["test-model".into()]);
    let router = build_router(config, providers, keyhub.clone());

    let results: Vec<_> = router
        .chat_stream(&chat_request("stream-test", true))
        .await
        .unwrap()
        .collect()
        .await;

    assert!(results.iter().any(Result::is_err));
    let state = keyhub.snapshot().remove(0).1.remove(0);
    assert_eq!(state.success_count, 0);
    assert_eq!(state.total_fail_count, 1);
    assert_eq!(state.status, KeyStatus::Cooldown);
}

#[tokio::test]
async fn test_router_never_uses_paid_or_unknown_keys() {
    let mut config = make_config();
    config.models.clear();
    config.fallback = vec!["shared".into()];
    config.providers.clear();
    config.providers.insert(
        "shared".into(),
        ProviderConfig {
            provider_type: ProviderType::OpenaiCompatible,
            enabled: true,
            base_url: "http://recording".into(),
            proxy_url: None,
            keys: vec![
                tiered_key("paid-key", KeyTier::Paid),
                KeyConfig::Legacy("unknown-key".into()),
            ],
            health_check_model: "wrong-model".into(),
            timeout_seconds: 5,
            priority: 0,
        },
    );
    let config = Arc::new(config);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let providers = Arc::new(DashMap::new());
    providers.insert(
        "shared".into(),
        Box::new(RecordingProvider {
            name: "shared".into(),
            calls: calls.clone(),
            fail_keys: vec![],
        }) as free_agent_gateway::providers::BoxedProvider,
    );
    let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
    keyhub.register_provider("shared", config.providers["shared"].keys.clone());
    keyhub.update_models("shared", "paid-key", vec!["target-model".into()]);
    keyhub.update_models("shared", "unknown-key", vec!["target-model".into()]);
    let router = build_router(config, providers, keyhub);

    let result = router.chat(&chat_request("target-model", false)).await;

    assert!(matches!(result, Err(GatewayError::ModelNotFound(_))));
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn test_router_falls_back_across_free_keys_without_changing_model() {
    let mut config = make_config();
    config.models.clear();
    config.fallback = vec!["first".into(), "second".into()];
    config.providers.clear();
    for name in ["first", "second"] {
        config.providers.insert(
            name.into(),
            ProviderConfig {
                provider_type: ProviderType::OpenaiCompatible,
                enabled: true,
                base_url: "http://recording".into(),
                proxy_url: None,
                keys: vec![tiered_key(&format!("{name}-key"), KeyTier::Free)],
                health_check_model: format!("wrong-{name}"),
                timeout_seconds: 5,
                priority: 0,
            },
        );
    }
    let config = Arc::new(config);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let providers = Arc::new(DashMap::new());
    for name in ["first", "second"] {
        providers.insert(
            name.into(),
            Box::new(RecordingProvider {
                name: name.into(),
                calls: calls.clone(),
                fail_keys: if name == "first" {
                    vec!["first-key".into()]
                } else {
                    vec![]
                },
            }) as free_agent_gateway::providers::BoxedProvider,
        );
    }
    let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
    for name in ["first", "second"] {
        keyhub.register_provider(name, config.providers[name].keys.clone());
        keyhub.update_models(name, &format!("{name}-key"), vec!["target-model".into()]);
    }
    let router = build_router(config, providers, keyhub);

    let response = router
        .chat(&chat_request("target-model", false))
        .await
        .unwrap();

    assert_eq!(response.body["id"], "recorded");
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            ("first-key".into(), "target-model".into()),
            ("second-key".into(), "target-model".into()),
        ]
    );
}

#[tokio::test]
async fn test_least_rate_prefers_less_used_key_across_providers() {
    let mut config = make_config();
    config.routing.strategy = RoutingStrategy::LeastRate;
    config.models.clear();
    config.fallback = vec!["first".into(), "second".into()];
    config.providers.clear();
    for name in ["first", "second"] {
        config.providers.insert(
            name.into(),
            ProviderConfig {
                provider_type: ProviderType::OpenaiCompatible,
                enabled: true,
                base_url: "http://recording".into(),
                proxy_url: None,
                keys: vec![tiered_key(&format!("{name}-key"), KeyTier::Free)],
                health_check_model: format!("wrong-{name}"),
                timeout_seconds: 5,
                priority: 0,
            },
        );
    }
    let config = Arc::new(config);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let providers = Arc::new(DashMap::new());
    for name in ["first", "second"] {
        providers.insert(
            name.into(),
            Box::new(RecordingProvider {
                name: name.into(),
                calls: calls.clone(),
                fail_keys: vec![],
            }) as free_agent_gateway::providers::BoxedProvider,
        );
    }
    let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
    for name in ["first", "second"] {
        keyhub.register_provider(name, config.providers[name].keys.clone());
        keyhub.update_models(name, &format!("{name}-key"), vec!["target-model".into()]);
    }
    keyhub.report_success("first", "first-key", Some(10), Some(5));
    let router = build_router(config, providers, keyhub);

    router
        .chat(&chat_request("target-model", false))
        .await
        .unwrap();

    assert_eq!(
        *calls.lock().unwrap(),
        vec![("second-key".into(), "target-model".into())]
    );
}

#[tokio::test]
async fn test_round_robin_rotates_same_model_across_providers() {
    let mut config = make_config();
    config.routing.strategy = RoutingStrategy::RoundRobin;
    config.models.clear();
    config.fallback = vec!["first".into(), "second".into()];
    config.providers.clear();
    for name in ["first", "second"] {
        config.providers.insert(
            name.into(),
            ProviderConfig {
                provider_type: ProviderType::OpenaiCompatible,
                enabled: true,
                base_url: "http://recording".into(),
                proxy_url: None,
                keys: vec![tiered_key(&format!("{name}-key"), KeyTier::Free)],
                health_check_model: format!("wrong-{name}"),
                timeout_seconds: 5,
                priority: 0,
            },
        );
    }
    let config = Arc::new(config);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let providers = Arc::new(DashMap::new());
    for name in ["first", "second"] {
        providers.insert(
            name.into(),
            Box::new(RecordingProvider {
                name: name.into(),
                calls: calls.clone(),
                fail_keys: vec![],
            }) as free_agent_gateway::providers::BoxedProvider,
        );
    }
    let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
    for name in ["first", "second"] {
        keyhub.register_provider(name, config.providers[name].keys.clone());
        keyhub.update_models(name, &format!("{name}-key"), vec!["target-model".into()]);
    }
    let router = build_router(config, providers, keyhub);

    router
        .chat(&chat_request("target-model", false))
        .await
        .unwrap();
    router
        .chat(&chat_request("target-model", false))
        .await
        .unwrap();

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_ne!(calls[0].0, calls[1].0);
    assert!(calls.iter().all(|(_, model)| model == "target-model"));
}
