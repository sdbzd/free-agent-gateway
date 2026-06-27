/// Integration tests for providers.
///
/// Uses mockito to simulate upstream provider responses.
use agent_gateway::config::{ProviderConfig, ProviderType};
use agent_gateway::models::{ChatCompletionRequest, ChatMessage};
use agent_gateway::providers::traits::Provider;
use agent_gateway::providers::{create_provider, github_models, nvidia, ollama, openai_compatible};

fn mock_provider_config(base_url: &str, ptype: ProviderType) -> ProviderConfig {
    ProviderConfig {
        provider_type: ptype,
        enabled: true,
        base_url: base_url.into(),
        keys: vec!["test-api-key".into()],
        health_check_model: "test-model".into(),
        timeout_seconds: 5,
        priority: 0,
    }
}

#[tokio::test]
async fn test_github_models_list_models() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/models")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"data":[{"id":"openai/gpt-4.1-mini"},{"id":"openai/o3-mini"}]}"#)
        .create_async()
        .await;

    let provider = github_models::GithubModelsProvider::new(
        "github",
        &mock_provider_config(&server.url(), ProviderType::GithubModels),
    );

    let models = provider.list_models("test-key").await.unwrap();
    assert_eq!(models.len(), 2);
    assert!(models.contains(&"openai/gpt-4.1-mini".to_string()));

    mock.assert_async().await;
}

#[tokio::test]
async fn test_nvidia_list_models() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/models")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"data":[{"id":"meta/llama-3.1-70b-instruct"}]}"#)
        .create_async()
        .await;

    let provider = nvidia::NvidiaProvider::new(
        "nvidia",
        &mock_provider_config(&server.url(), ProviderType::Nvidia),
    );

    let models = provider.list_models("test-key").await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0], "meta/llama-3.1-70b-instruct");
}

#[tokio::test]
async fn test_openai_compatible_chat() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"id":"chatcmpl-1","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"Hello!"},"finish_reason":"stop"}]}"#,
        )
        .create_async()
        .await;

    let provider = openai_compatible::OpenAiCompatibleProvider::new(
        "opencode",
        &mock_provider_config(&server.url(), ProviderType::OpenaiCompatible),
    );

    let request = ChatCompletionRequest {
        model: "gpt-4o-mini".into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: serde_json::json!("Hi"),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            extra: serde_json::Map::new(),
        }],
        temperature: None,
        top_p: None,
        n: None,
        stream: None,
        stop: None,
        max_tokens: None,
        presence_penalty: None,
        frequency_penalty: None,
        user: None,
        request_id: None,
        agent_name: None,
        extra: serde_json::Map::new(),
    };

    let response = provider.chat("test-key", request).await.unwrap();
    assert_eq!(response.status, 200);
    let content = response.body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap();
    assert_eq!(content, "Hello!");
}

#[tokio::test]
async fn test_provider_chat_error_propagation() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("POST", "/chat/completions")
        .with_status(429)
        .with_body(r#"{"error":{"message":"rate limited"}}"#)
        .create_async()
        .await;

    let provider = openai_compatible::OpenAiCompatibleProvider::new(
        "test",
        &mock_provider_config(&server.url(), ProviderType::OpenaiCompatible),
    );

    let request = ChatCompletionRequest {
        model: "gpt-4o-mini".into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: serde_json::json!("Hi"),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            extra: serde_json::Map::new(),
        }],
        temperature: None,
        top_p: None,
        n: None,
        stream: None,
        stop: None,
        max_tokens: None,
        presence_penalty: None,
        frequency_penalty: None,
        user: None,
        request_id: None,
        agent_name: None,
        extra: serde_json::Map::new(),
    };

    let result = provider.chat("test-key", request).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        agent_gateway::error::GatewayError::HttpError { status, .. } => assert_eq!(status, 429),
        other => panic!("Expected HttpError, got {other:?}"),
    }
}

#[tokio::test]
async fn test_ollama_provider_construction() {
    let config = ProviderConfig {
        provider_type: ProviderType::Ollama,
        enabled: true,
        base_url: "http://localhost:11434/".into(), // trailing slash should be trimmed
        keys: vec!["ollama".into()],
        health_check_model: "".into(),
        timeout_seconds: 120,
        priority: 0,
    };

    let provider = ollama::OllamaProvider::new("ollama", &config);
    assert_eq!(provider.base_url(), "http://localhost:11434");
    assert_eq!(provider.health_check_model(), "qwen2.5:7b"); // default applied
    assert_eq!(provider.priority(), 100); // default applied
}

#[test]
fn test_create_provider_factory() {
    let config = mock_provider_config("http://localhost", ProviderType::GithubModels);
    let provider = create_provider("github", &config).unwrap();
    assert_eq!(provider.name(), "github");

    let config = mock_provider_config("http://localhost", ProviderType::Nvidia);
    let provider = create_provider("nvidia", &config).unwrap();
    assert_eq!(provider.name(), "nvidia");

    let config = mock_provider_config("http://localhost", ProviderType::Ollama);
    let provider = create_provider("ollama", &config).unwrap();
    assert_eq!(provider.name(), "ollama");

    let config = mock_provider_config("http://localhost", ProviderType::OpenaiCompatible);
    let provider = create_provider("oc", &config).unwrap();
    assert_eq!(provider.name(), "oc");
}
