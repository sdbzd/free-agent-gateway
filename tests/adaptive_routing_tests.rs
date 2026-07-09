use dashmap::DashMap;
use free_agent_gateway::AppState;
use free_agent_gateway::adaptive::profile::{TaskKind, build_task_profile};
use free_agent_gateway::adaptive::scoring::{
    AdaptiveCandidate, CandidateTier, RouteConstraints, score_candidates,
};
use free_agent_gateway::adaptive::{
    AdaptiveScope, is_reserved_provider_prefix, record_profile_observations, routing_diagnostics,
    routing_groups_summary, routing_routes_summary, select_model,
};
use free_agent_gateway::config::{
    AdaptiveRoutingConfig, AutoModelConfig, Config, KeyConfig, KeyTier, ProviderConfig,
    ProviderType, RoutingConfig, RoutingGroupConfig, RoutingStrategy, ServerConfig,
};
use free_agent_gateway::health::HealthRegistry;
use free_agent_gateway::keyhub::KeyHub;
use free_agent_gateway::metadata::ModelMetaStore;
use free_agent_gateway::models::{ChatCompletionRequest, ChatMessage};
use free_agent_gateway::router::Router;
use free_agent_gateway::state::PersistedState;
use parking_lot::{Mutex as ParkingMutex, RwLock};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;

fn request_with_content(content: serde_json::Value) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "auto".into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content,
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
    }
}

#[test]
fn profile_detects_vision_from_image_parts() {
    let req = request_with_content(serde_json::json!([
        {"type":"text","text":"what is this?"},
        {"type":"image_url","image_url":{"url":"data:image/png;base64,abc"}}
    ]));

    let profile = build_task_profile(Some("document"), &req, None);

    assert!(profile.needs_vision);
    assert!(profile.task_kinds.contains(&TaskKind::Vision));
}

#[test]
fn profile_detects_coding_from_agent_and_text() {
    let req = request_with_content(serde_json::json!(
        "Fix this Rust panic:\n```rust\npanic!(\"boom\")\n```"
    ));

    let profile = build_task_profile(Some("coding_agent"), &req, None);

    assert!(profile.needs_coding);
    assert!(profile.needs_reasoning);
    assert!(profile.task_kinds.contains(&TaskKind::Coding));
}

#[test]
fn profile_detects_tools_from_extra_fields() {
    let mut req = request_with_content(serde_json::json!("call a tool"));
    req.extra
        .insert("tools".into(), serde_json::json!([{"type":"function"}]));

    let profile = build_task_profile(Some("hermes"), &req, None);

    assert!(profile.needs_tools);
    assert!(profile.task_kinds.contains(&TaskKind::Tools));
}

fn adaptive_config() -> Arc<Config> {
    let mut adaptive = AdaptiveRoutingConfig {
        enabled: true,
        ..Default::default()
    };
    adaptive.auto_models.insert(
        "coding-auto".into(),
        AutoModelConfig {
            task: "coding".into(),
        },
    );
    adaptive.routing_groups.insert(
        "cloud".into(),
        RoutingGroupConfig {
            providers: vec!["openrouter".into()],
        },
    );

    Arc::new(Config {
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
            cooldown_seconds: 600,
            auto_discover: true,
        },
        fallback: vec!["openrouter".into(), "nvidia".into()],
        model_fallbacks: HashMap::new(),
        agents: HashMap::new(),
        models: HashMap::new(),
        providers: HashMap::from([
            (
                "openrouter".into(),
                ProviderConfig {
                    provider_type: ProviderType::OpenaiCompatible,
                    enabled: true,
                    base_url: "https://openrouter.ai/api/v1".into(),
                    proxy_url: None,
                    keys: vec![KeyConfig::detailed("or-key", KeyTier::Free)],
                    health_check_model: "qwen/qwen3-coder:free".into(),
                    timeout_seconds: 30,
                    priority: 0,
                },
            ),
            (
                "nvidia".into(),
                ProviderConfig {
                    provider_type: ProviderType::OpenaiCompatible,
                    enabled: true,
                    base_url: "https://integrate.api.nvidia.com/v1".into(),
                    proxy_url: None,
                    keys: vec![KeyConfig::detailed("nv-key", KeyTier::Free)],
                    health_check_model: "meta/llama".into(),
                    timeout_seconds: 30,
                    priority: 0,
                },
            ),
        ]),
        watcher: Default::default(),
        state: Default::default(),
        cors: Default::default(),
        adaptive_routing: adaptive,
        context_compression: Default::default(),
        logging: Default::default(),
    })
}

fn adaptive_state() -> AppState {
    let config = adaptive_config();
    let providers = Arc::new(DashMap::new());
    let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
    for (name, provider) in &config.providers {
        keyhub.register_provider(name, provider.keys.clone());
    }
    keyhub.update_models(
        "openrouter",
        "or-key",
        vec!["qwen/qwen3-coder:free".into(), "general-model".into()],
    );
    keyhub.update_models("nvidia", "nv-key", vec!["nvidia-chat".into()]);
    let disabled_models = Arc::new(RwLock::new(HashMap::<String, HashSet<String>>::new()));
    let router = Arc::new(Router::new(
        config.clone(),
        providers.clone(),
        keyhub.clone(),
        disabled_models.clone(),
        None,
    ));
    let (sse_tx, _) = tokio::sync::broadcast::channel::<String>(16);

    AppState {
        config,
        state: Arc::new(RwLock::new(PersistedState::new())),
        http_client: reqwest::Client::new(),
        providers,
        keyhub,
        router,
        health_registry: Arc::new(HealthRegistry::new()),
        request_counter: Arc::new(AtomicU64::new(0)),
        error_counter: Arc::new(AtomicU64::new(0)),
        start_time: Instant::now(),
        sse_tx,
        disabled_models,
        model_meta: None,
        _sync_handle: Arc::new(ParkingMutex::new(None)),
    }
}

fn adaptive_state_with_meta() -> (AppState, std::path::PathBuf) {
    let temp = std::env::temp_dir().join(format!(
        "free-agent-gateway-adaptive-test-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&temp).unwrap();
    let meta = ModelMetaStore::open(temp.join("models.db")).unwrap();
    let mut state = adaptive_state();
    state.model_meta = Some(meta);
    (state, temp)
}

#[test]
fn adaptive_auto_model_selects_coding_candidate() {
    let state = adaptive_state();
    let req = request_with_content(serde_json::json!(
        "Fix this Rust panic:\n```rust\npanic!(\"boom\")\n```"
    ));

    let selection = select_model(&state, &AdaptiveScope::Auto, &req).unwrap();

    assert_eq!(selection.provider, "openrouter");
    assert_eq!(selection.model, "qwen/qwen3-coder:free");
}

#[test]
fn adaptive_provider_scope_restricts_candidates() {
    let state = adaptive_state();
    let req = request_with_content(serde_json::json!("hello"));

    let selection = select_model(&state, &AdaptiveScope::Provider("nvidia".into()), &req).unwrap();

    assert_eq!(selection.provider, "nvidia");
    assert_eq!(selection.model, "nvidia-chat");
}

#[test]
fn adaptive_provider_group_scope_restricts_candidates() {
    let state = adaptive_state();
    let req = request_with_content(serde_json::json!("hello"));

    let selection =
        select_model(&state, &AdaptiveScope::ProviderGroup("cloud".into()), &req).unwrap();

    assert_eq!(selection.provider, "openrouter");
}

#[test]
fn provider_prefix_rejects_reserved_names() {
    assert!(is_reserved_provider_prefix("admin"));
    assert!(
        select_model(
            &adaptive_state(),
            &AdaptiveScope::Provider("admin".into()),
            &request_with_content(serde_json::json!("hello")),
        )
        .is_err()
    );
}

fn base_candidate(provider: &str, model: &str) -> AdaptiveCandidate {
    AdaptiveCandidate {
        provider: provider.into(),
        model: model.into(),
        api_key: None,
        tier: CandidateTier::Free,
        supports_vision: None,
        supports_tools: None,
        supports_reasoning: None,
        context_window: None,
        recent_successes: 0,
        recent_errors: 0,
        recent_429s: 0,
        recent_timeouts: 0,
        quota_headroom: 100,
        provider_priority: 0,
        prompt_price: None,
    }
}

#[test]
fn scoring_prefers_tool_capable_model_for_tool_request() {
    let mut req = request_with_content(serde_json::json!("call a tool"));
    req.extra
        .insert("tools".into(), serde_json::json!([{"type":"function"}]));
    let profile = build_task_profile(Some("hermes"), &req, None);
    let mut capable = base_candidate("openrouter", "tool-model");
    capable.supports_tools = Some(true);
    let mut unknown = base_candidate("openrouter", "unknown-model");
    unknown.supports_tools = None;

    let scored = score_candidates(
        &profile,
        vec![unknown, capable],
        &RouteConstraints::default(),
    );

    assert_eq!(scored[0].candidate.model, "tool-model");
    assert!(scored[0].breakdown.capability_match > scored[1].breakdown.capability_match);
}

#[test]
fn scoring_penalizes_recent_errors() {
    let req = request_with_content(serde_json::json!("hello"));
    let profile = build_task_profile(None, &req, None);
    let healthy = base_candidate("openrouter", "healthy");
    let mut noisy = base_candidate("openrouter", "noisy");
    noisy.recent_errors = 10;
    noisy.recent_429s = 4;
    noisy.recent_timeouts = 2;

    let scored = score_candidates(&profile, vec![noisy, healthy], &RouteConstraints::default());

    assert_eq!(scored[0].candidate.model, "healthy");
    assert!(scored[1].breakdown.penalty > 0);
}

#[test]
fn scoring_applies_provider_constraint() {
    let req = request_with_content(serde_json::json!("hello"));
    let profile = build_task_profile(None, &req, None);
    let openrouter = base_candidate("openrouter", "model-a");
    let nvidia = base_candidate("nvidia", "model-b");
    let constraints = RouteConstraints {
        provider: Some("nvidia".into()),
        ..RouteConstraints::default()
    };

    let scored = score_candidates(&profile, vec![openrouter, nvidia], &constraints);

    assert_eq!(scored.len(), 1);
    assert_eq!(scored[0].candidate.provider, "nvidia");
}

#[test]
fn scoring_excludes_paid_candidates_by_default() {
    let req = request_with_content(serde_json::json!("hello"));
    let profile = build_task_profile(None, &req, None);
    let free = base_candidate("openrouter", "free");
    let mut paid = base_candidate("openrouter", "paid");
    paid.tier = CandidateTier::Paid;

    let scored = score_candidates(&profile, vec![paid, free], &RouteConstraints::default());

    assert_eq!(scored.len(), 1);
    assert_eq!(scored[0].candidate.model, "free");
}

#[test]
fn adaptive_selection_requires_enabled_config() {
    let mut config = (*adaptive_config()).clone();
    config.adaptive_routing.enabled = false;
    let mut state = adaptive_state();
    state.config = Arc::new(config);

    let result = select_model(
        &state,
        &AdaptiveScope::Auto,
        &request_with_content(serde_json::json!("hello")),
    );

    assert!(result.is_err());
}

#[test]
fn profile_observations_record_required_capability_failures() {
    let (state, _temp) = adaptive_state_with_meta();
    let mut req = request_with_content(serde_json::json!("call a tool with this code"));
    req.extra
        .insert("tools".into(), serde_json::json!([{"type":"function"}]));
    let profile = build_task_profile(Some("coding_agent"), &req, None);

    record_profile_observations(&state, "openrouter", "tool-shy", &profile, "failure");

    let meta = state.model_meta.as_ref().unwrap();
    assert_eq!(
        meta.get_capability_observation_count("openrouter", "tool-shy", "tools", "failure")
            .unwrap(),
        1
    );
    assert_eq!(
        meta.get_capability_observation_count("openrouter", "tool-shy", "coding", "failure")
            .unwrap(),
        1
    );
}

#[test]
fn routing_diagnostics_explain_profile_and_candidates() {
    let state = adaptive_state();
    let req = request_with_content(serde_json::json!(
        "Fix this Rust panic:\n```rust\npanic!(\"boom\")\n```"
    ));

    let diagnostics = routing_diagnostics(&state, &AdaptiveScope::Auto, &req).unwrap();

    assert!(diagnostics.adaptive_enabled);
    assert!(diagnostics.task_kinds.contains(&"coding".to_string()));
    assert_eq!(diagnostics.candidates[0].provider, "openrouter");
    assert_eq!(diagnostics.candidates[0].model, "qwen/qwen3-coder:free");
    assert!(diagnostics.candidates[0].score > 0);
}

#[test]
fn routing_groups_summary_exposes_configured_groups_and_agent_usage() {
    let state = adaptive_state();

    let groups = routing_groups_summary(&state);

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "cloud");
    assert_eq!(groups[0].providers.len(), 1);
    assert_eq!(groups[0].providers[0].name, "openrouter");
    assert_eq!(groups[0].provider_names, vec!["openrouter"]);
    assert_eq!(groups[0].route_prefix, "/provider-groups/cloud/v1");
}

#[test]
fn routing_routes_summary_exposes_auto_agent_provider_and_group_routes() {
    let mut config = (*adaptive_config()).clone();
    config.adaptive_routing.agent_profiles.insert(
        "coding_agent".into(),
        free_agent_gateway::config::AdaptiveAgentProfile {
            default_auto_model: "coding-auto".into(),
            preferred_tasks: vec!["coding".into()],
            provider_groups: vec!["cloud".into()],
        },
    );
    let mut state = adaptive_state();
    state.config = Arc::new(config);

    let routes = routing_routes_summary(&state);

    assert!(
        routes
            .iter()
            .any(|route| route.kind == "auto" && route.route_prefix == "/auto/v1")
    );
    assert!(routes.iter().any(|route| route.kind == "agent"
        && route.name == "coding_agent"
        && route.route_prefix == "/agents/coding_agent/v1"));
    assert!(routes.iter().any(|route| route.kind == "provider"
        && route.name == "openrouter"
        && route.route_prefix == "/openrouter/v1"));
    assert!(routes.iter().any(|route| route.kind == "provider_group"
        && route.name == "cloud"
        && route.route_prefix == "/provider-groups/cloud/v1"));
}
