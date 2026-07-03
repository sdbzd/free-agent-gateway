/// Integration tests for config parsing, state persistence, and health registry.
use free_agent_gateway::config::{
    Config, KeyConfig, KeyTier, ProviderConfig, ProviderType, RoutingStrategy,
};
use free_agent_gateway::health::HealthRegistry;
use free_agent_gateway::keyhub::{KeyHub, key_fingerprint};
use free_agent_gateway::models::{KeyState, KeyStatus};
use free_agent_gateway::state::PersistedState;

const SAMPLE_CONFIG: &str = r#"
server:
  host: "0.0.0.0"
  port: 8080
  log_level: "debug"
  request_timeout: 60
  sse_keepalive: 10

routing:
  strategy: "round_robin"
  fail_threshold: 5
  cooldown_seconds: 300
  auto_discover: false

fallback:
  - "nvidia"
  - "ollama"

agents:
  hermes:
    default_model: "coding"

models:
  coding:
    provider: "github"
    model: "openai/gpt-4.1-mini"

providers:
  github:
    type: "github_models"
    enabled: true
    base_url: "https://models.inference.ai.azure.com"
    keys:
      - "gh-key"
    health_check_model: "openai/gpt-4.1-mini"
    timeout_seconds: 30
  nvidia:
    type: "nvidia"
    enabled: true
    base_url: "https://integrate.api.nvidia.com/v1"
    keys:
      - "nv-key"
    health_check_model: "meta/llama-3.1-70b-instruct"
    timeout_seconds: 30
  ollama:
    type: "ollama"
    enabled: true
    base_url: "http://localhost:11434"
    keys:
      - "ollama"
    health_check_model: "qwen2.5:7b"
    timeout_seconds: 120
    priority: 100
"#;

#[test]
fn test_config_parse_from_yaml() {
    let config = Config::from_str_yaml(SAMPLE_CONFIG).unwrap();
    assert_eq!(config.server.host, "0.0.0.0");
    assert_eq!(config.server.port, 8080);
    assert_eq!(config.server.log_level, "debug");
    assert_eq!(config.routing.strategy, RoutingStrategy::RoundRobin);
    assert_eq!(config.routing.fail_threshold, 5);
    assert!(!config.routing.auto_discover);
    assert_eq!(
        config.fallback,
        vec!["nvidia".to_string(), "ollama".to_string()]
    );
}

#[test]
fn test_config_parse_without_routing_uses_defaults() {
    let yaml = r#"
server:
  host: "127.0.0.1"
  port: 9000

providers:
  github:
    type: "github_models"
    enabled: true
    base_url: "https://models.inference.ai.azure.com"
    keys:
      - "gh-key"
    health_check_model: "openai/gpt-4.1-mini"
"#;

    let config = Config::from_str_yaml(yaml).unwrap();

    assert_eq!(config.routing.strategy, RoutingStrategy::LeastFailed);
    assert_eq!(config.routing.fail_threshold, 3);
    assert_eq!(config.routing.cooldown_seconds, 600);
    assert!(config.routing.auto_discover);
}

#[test]
fn test_sample_config_parses_and_validates() {
    let yaml = std::fs::read_to_string("config.yaml.sample").unwrap();
    let config = Config::from_str_yaml(&yaml).unwrap();

    config.validate().unwrap();
    assert_eq!(
        config.providers["cerebras"].provider_type,
        ProviderType::OpenaiCompatible
    );
    assert!(!config.providers["cerebras"].enabled);
}

#[test]
fn test_config_parse_agents() {
    let config = Config::from_str_yaml(SAMPLE_CONFIG).unwrap();
    let hermes = config.agents.get("hermes").unwrap();
    assert_eq!(hermes.default_model, "coding");
}

#[test]
fn test_config_parse_models() {
    let config = Config::from_str_yaml(SAMPLE_CONFIG).unwrap();
    let coding = config.models.get("coding").unwrap();
    assert_eq!(coding.provider, "github");
    assert_eq!(coding.model, "openai/gpt-4.1-mini");
}

#[test]
fn test_config_parse_providers() {
    let config = Config::from_str_yaml(SAMPLE_CONFIG).unwrap();
    let github = config.providers.get("github").unwrap();
    assert_eq!(github.provider_type, ProviderType::GithubModels);
    assert!(github.enabled);
    assert_eq!(github.keys[0].value(), "gh-key");

    let ollama = config.providers.get("ollama").unwrap();
    assert_eq!(ollama.provider_type, ProviderType::Ollama);
    assert_eq!(ollama.priority, 100);
}

#[test]
fn test_config_env_var_expansion() {
    // SAFETY: single-threaded test, no other threads reading the env.
    unsafe {
        std::env::set_var("TEST_GATEWAY_KEY", "secret-value-123");
    }
    let yaml = r#"
server:
  host: "127.0.0.1"
  port: 9000
  log_level: "info"
  request_timeout: 120
  sse_keepalive: 15
routing:
  strategy: "least_failed"
  fail_threshold: 3
  cooldown_seconds: 600
  auto_discover: true
providers:
  test:
    type: "openai_compatible"
    enabled: true
    base_url: "http://localhost"
    keys:
      - "${TEST_GATEWAY_KEY}"
    health_check_model: "m"
    timeout_seconds: 30
"#;
    let config = Config::from_str_yaml(yaml).unwrap();
    let key = &config.providers.get("test").unwrap().keys[0];
    assert_eq!(key.value(), "secret-value-123");
    // SAFETY: single-threaded test.
    unsafe {
        std::env::remove_var("TEST_GATEWAY_KEY");
    }
}

#[test]
fn test_config_validation_rejects_unknown_fallback_provider() {
    let yaml = r#"
server: {}
routing: {}
fallback:
  - missing
providers:
  test:
    type: openai_compatible
    base_url: https://example.test/v1
    keys:
      - test-key
"#;

    let error = Config::from_str_yaml(yaml).unwrap_err();

    assert!(error.to_string().contains("fallback provider 'missing'"));
}

#[test]
fn test_config_validation_rejects_empty_provider_key() {
    let yaml = r#"
server: {}
routing: {}
providers:
  test:
    type: openai_compatible
    base_url: https://example.test/v1
    keys:
      - ""
"#;

    let error = Config::from_str_yaml(yaml).unwrap_err();

    assert!(error.to_string().contains("contains an empty key"));
}

#[test]
fn test_config_validation_allows_empty_keys_on_disabled_provider() {
    let yaml = r#"
server: {}
routing: {}
providers:
  disabled:
    type: openai_compatible
    enabled: false
    base_url: https://example.test/v1
    keys:
      - ""
  active:
    type: openai_compatible
    base_url: https://example.test/v1
    keys:
      - active-key
"#;

    let config = Config::from_str_yaml(yaml).unwrap();

    assert!(!config.providers["disabled"].enabled);
}

#[test]
fn test_config_validation_rejects_alias_with_unknown_provider() {
    let yaml = r#"
server: {}
routing: {}
models:
  chat:
    provider: missing
    model: gpt-test
providers:
  test:
    type: openai_compatible
    base_url: https://example.test/v1
    keys:
      - test-key
"#;

    let error = Config::from_str_yaml(yaml).unwrap_err();

    assert!(error.to_string().contains("references unknown provider"));
}

#[test]
fn test_state_save_and_load_roundtrip() {
    let temp_dir = std::env::temp_dir();
    let path = temp_dir.join("openclaw_test_state.json");
    let path_str = path.to_str().unwrap();

    // Clean up any previous test artifact
    let _ = std::fs::remove_file(path_str);

    let mut state = PersistedState::new();
    state.providers.insert(
        "github".into(),
        free_agent_gateway::state::ProviderKeyState {
            keys: vec![free_agent_gateway::models::KeyState::new("test-key".into())],
        },
    );

    state.save(path_str).unwrap();
    assert!(path.exists());

    let loaded = PersistedState::load(path_str).unwrap();
    assert_eq!(loaded.version, 1);
    assert!(loaded.providers.contains_key("github"));
    assert_eq!(loaded.providers["github"].keys.len(), 1);

    let _ = std::fs::remove_file(path_str);
}

#[test]
fn test_state_load_missing_file_returns_default() {
    let path = std::env::temp_dir().join("definitely_nonexistent_state.json");
    let state = PersistedState::load(path.to_str().unwrap()).unwrap();
    assert_eq!(state.version, 1);
    assert!(state.providers.is_empty());
}

#[test]
fn test_health_registry_register_and_snapshot() {
    let registry = HealthRegistry::new();
    let config = ProviderConfig {
        provider_type: ProviderType::GithubModels,
        enabled: true,
        base_url: "http://localhost".into(),
        proxy_url: None,
        keys: vec!["k1".into(), "k2".into()],
        health_check_model: "m".into(),
        timeout_seconds: 30,
        priority: 0,
    };
    registry.register("github", &config);

    let snapshot = registry.snapshot();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].provider, "github");
    assert_eq!(snapshot[0].total_keys, 2);
}

#[test]
fn test_health_registry_update_healthy() {
    let registry = HealthRegistry::new();
    let config = ProviderConfig {
        provider_type: ProviderType::GithubModels,
        enabled: true,
        base_url: "http://localhost".into(),
        proxy_url: None,
        keys: vec!["k1".into()],
        health_check_model: "m".into(),
        timeout_seconds: 30,
        priority: 0,
    };
    registry.register("github", &config);

    registry.update("github", "healthy", 50, 10, 1, 1);
    let snapshot = registry.snapshot();
    assert_eq!(snapshot[0].status, "healthy");
    assert_eq!(snapshot[0].latency_ms, 50);
    assert_eq!(snapshot[0].models_count, 10);
    assert_eq!(snapshot[0].success_count, 1);
}

#[test]
fn test_health_registry_record_error() {
    let registry = HealthRegistry::new();
    let config = ProviderConfig {
        provider_type: ProviderType::GithubModels,
        enabled: true,
        base_url: "http://localhost".into(),
        proxy_url: None,
        keys: vec!["k1".into()],
        health_check_model: "m".into(),
        timeout_seconds: 30,
        priority: 0,
    };
    registry.register("github", &config);

    registry.record_error("github", "connection refused");
    let snapshot = registry.snapshot();
    assert_eq!(snapshot[0].status, "unhealthy");
    assert_eq!(snapshot[0].last_error, "connection refused");
    assert_eq!(snapshot[0].fail_count, 1);
}

#[test]
fn test_health_registry_all_remote_down() {
    let registry = HealthRegistry::new();
    let remote_cfg = ProviderConfig {
        provider_type: ProviderType::GithubModels,
        enabled: true,
        base_url: "http://localhost".into(),
        proxy_url: None,
        keys: vec!["k1".into()],
        health_check_model: "m".into(),
        timeout_seconds: 30,
        priority: 0,
    };
    let local_cfg = ProviderConfig {
        provider_type: ProviderType::Ollama,
        enabled: true,
        base_url: "http://localhost:11434".into(),
        proxy_url: None,
        keys: vec!["ollama".into()],
        health_check_model: "m".into(),
        timeout_seconds: 120,
        priority: 100,
    };
    registry.register("github", &remote_cfg);
    registry.register("ollama", &local_cfg);

    // Both healthy 鈫?not all remote down
    registry.update("github", "healthy", 10, 5, 1, 1);
    registry.update("ollama", "healthy", 10, 5, 1, 1);
    assert!(!registry.all_remote_down());

    // Remote down, local healthy 鈫?all remote down is true
    registry.record_error("github", "timeout");
    assert!(registry.all_remote_down());
}

#[test]
fn test_state_serializes_key_identifier_without_raw_key() {
    let temp_dir = std::env::temp_dir();
    let path = temp_dir.join("openclaw_key_identifier_state.json");
    let path_str = path.to_str().unwrap();
    let _ = std::fs::remove_file(path_str);

    let hub = KeyHub::new(free_agent_gateway::config::RoutingConfig {
        strategy: RoutingStrategy::LeastFailed,
        fail_threshold: 3,
        cooldown_seconds: 60,
        auto_discover: true,
    });
    hub.register_provider("github", vec!["raw-secret-key".into()]);
    hub.report_failure("github", "raw-secret-key", 429);

    let mut state = PersistedState::new();
    for (provider, keys) in hub.snapshot() {
        state.providers.insert(
            provider,
            free_agent_gateway::state::ProviderKeyState { keys },
        );
    }
    state.save(path_str).unwrap();

    let serialized = std::fs::read_to_string(path_str).unwrap();
    assert!(!serialized.contains("raw-secret-key"));
    assert!(serialized.contains(&key_fingerprint("raw-secret-key")));

    let _ = std::fs::remove_file(path_str);
}

#[test]
fn test_persisted_state_restores_only_matching_configured_keys() {
    let routing = free_agent_gateway::config::RoutingConfig {
        strategy: RoutingStrategy::LeastFailed,
        fail_threshold: 3,
        cooldown_seconds: 60,
        auto_discover: true,
    };
    let source = KeyHub::new(routing.clone());
    source.register_provider("github", vec!["key-a".into()]);
    source.update_models("github", "key-a", vec!["model-a".into()]);
    source.report_failure("github", "key-a", 401);
    let persisted = source.snapshot().remove(0).1;

    let restored = KeyHub::new(routing);
    restored.register_provider("github", vec!["key-a".into(), "key-new".into()]);
    restored.restore_provider_states("github", &persisted);

    let snapshot = restored.snapshot().remove(0).1;
    let disabled = snapshot
        .iter()
        .find(|state| state.key_id == key_fingerprint("key-a"))
        .unwrap();
    let fresh = snapshot
        .iter()
        .find(|state| state.key_id == key_fingerprint("key-new"))
        .unwrap();

    assert_eq!(disabled.status, KeyStatus::Disabled);
    assert_eq!(disabled.models, vec!["model-a"]);
    assert_eq!(fresh.status, KeyStatus::Available);

    let mut unknown = KeyState::new("removed-key".into());
    unknown.status = KeyStatus::Disabled;
    restored.restore_provider_states("github", &[unknown]);
    assert_eq!(restored.snapshot().remove(0).1.len(), 2);
}

#[test]
fn test_key_object_parses_explicit_tiers() {
    let yaml = r#"
server: {}
routing: {}
providers:
  test:
    type: openai_compatible
    base_url: https://example.test/v1
    keys:
      - value: free-key
        tier: free
      - value: paid-key
        tier: paid
"#;

    let config = Config::from_str_yaml(yaml).unwrap();
    let keys = &config.providers["test"].keys;

    assert_eq!(keys[0].value(), "free-key");
    assert_eq!(keys[0].tier(), KeyTier::Free);
    assert_eq!(keys[1].tier(), KeyTier::Paid);
}

#[test]
fn test_legacy_key_defaults_to_unknown_and_optional_models_parse() {
    let yaml = r#"
server: {}
routing: {}
providers:
  test:
    type: nvidia
    base_url: https://example.test/v1
    keys:
      - legacy-key
"#;

    let config = Config::from_str_yaml(yaml).unwrap();
    let provider = &config.providers["test"];

    assert!(config.models.is_empty());
    assert_eq!(provider.provider_type, ProviderType::Nvidia);
    assert_eq!(provider.keys[0].tier(), KeyTier::Unknown);
    assert!(provider.health_check_model.is_empty());
    assert!(matches!(provider.keys[0], KeyConfig::Legacy(_)));
}

#[test]
fn test_huggingface_provider_type_parses() {
    let yaml = r#"
server: {}
routing: {}
providers:
  huggingface:
    type: huggingface
    base_url: https://router.huggingface.co/v1
    keys:
      - value: test-hf-token
        tier: free
"#;

    let config = Config::from_str_yaml(yaml).unwrap();
    let provider = &config.providers["huggingface"];

    assert_eq!(provider.provider_type, ProviderType::HuggingFace);
    assert_eq!(provider.base_url, "https://router.huggingface.co/v1");
    assert!(provider.health_check_model.is_empty());
}

#[test]
fn test_adaptive_routing_config_parses() {
    let yaml = r#"
server:
  host: "127.0.0.1"
  port: 9000
routing:
  auto_discover: true
fallback: ["openrouter"]
models: {}
providers:
  openrouter:
    type: "openai_compatible"
    enabled: true
    base_url: "https://openrouter.ai/api/v1"
    keys:
      - value: "test-key"
        tier: free
adaptive_routing:
  enabled: true
  mode: observe
  allow_paid: false
  candidate_limit: 12
  learning_window_days: 14
  hard_override_on_capability_mismatch: true
  auto_models:
    coding-auto:
      task: coding
  agent_profiles:
    coding_agent:
      default_auto_model: coding-auto
      preferred_tasks: ["coding", "tools"]
      provider_groups: ["coding"]
  routing_groups:
    coding:
      providers: ["openrouter"]
"#;

    let config = Config::from_str_yaml(yaml).unwrap();

    let adaptive = config.adaptive_routing;
    assert!(adaptive.enabled);
    assert_eq!(
        adaptive.mode,
        free_agent_gateway::config::AdaptiveMode::Observe
    );
    assert_eq!(adaptive.candidate_limit, 12);
    assert_eq!(adaptive.learning_window_days, 14);
    assert_eq!(adaptive.auto_models["coding-auto"].task, "coding");
    assert_eq!(
        adaptive.agent_profiles["coding_agent"].preferred_tasks,
        vec!["coding".to_string(), "tools".to_string()]
    );
    assert_eq!(
        adaptive.routing_groups["coding"].providers,
        vec!["openrouter".to_string()]
    );
}

#[test]
fn test_adaptive_routing_defaults_are_safe() {
    let yaml = r#"
server: {}
routing: {}
providers:
  test:
    type: openai_compatible
    base_url: https://example.test/v1
    keys:
      - value: test-key
        tier: free
"#;

    let config = Config::from_str_yaml(yaml).unwrap();

    assert!(!config.adaptive_routing.enabled);
    assert_eq!(
        config.adaptive_routing.mode,
        free_agent_gateway::config::AdaptiveMode::Observe
    );
    assert!(!config.adaptive_routing.allow_paid);
    assert_eq!(config.adaptive_routing.candidate_limit, 20);
    assert_eq!(config.adaptive_routing.learning_window_days, 7);
}

#[test]
fn test_context_compression_config_parses_rtk_path() {
    let yaml = r#"
server: {}
routing: {}
context_compression:
  enabled: true
  command: "G:\\ai\\AgentsTools\\rtk.exe"
  min_message_tokens: 256
  timeout_seconds: 2
providers:
  test:
    type: openai_compatible
    base_url: https://example.test/v1
    keys:
      - value: test-key
        tier: free
"#;

    let config = Config::from_str_yaml(yaml).unwrap();

    assert!(config.context_compression.enabled);
    assert_eq!(
        config.context_compression.command,
        "G:\\ai\\AgentsTools\\rtk.exe"
    );
    assert_eq!(config.context_compression.min_message_tokens, 256);
    assert_eq!(config.context_compression.timeout_seconds, 2);
}
