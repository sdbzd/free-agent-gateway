pub mod observations;
pub mod profile;
pub mod scoring;

use axum::http::StatusCode;

use crate::AppState;
use crate::config::KeyTier;
use crate::error::{GatewayError, GatewayResult};
use crate::models::{ChatCompletionRequest, ModelInfo};
use crate::providers::traits::ChatResponse;

pub use observations::record_profile_observations;
use profile::build_task_profile;
use scoring::{AdaptiveCandidate, CandidateTier, RouteConstraints, score_candidates};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdaptiveScope {
    Auto,
    Agent(String),
    Provider(String),
    ProviderGroup(String),
}

#[derive(Debug, Clone)]
pub struct AdaptiveSelection {
    pub provider: String,
    pub model: String,
    pub api_key: String,
    pub score: i32,
    pub task_kinds: Vec<profile::TaskKind>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RoutingDiagnostics {
    pub adaptive_enabled: bool,
    pub scope: String,
    pub requested_model: String,
    pub concrete_model: Option<String>,
    pub agent: Option<String>,
    pub task_kinds: Vec<String>,
    pub estimated_prompt_tokens: u32,
    pub candidates: Vec<DiagnosticCandidate>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DiagnosticCandidate {
    pub provider: String,
    pub model: String,
    pub tier: String,
    pub score: i32,
    pub selected: bool,
    pub supports_vision: Option<bool>,
    pub supports_tools: Option<bool>,
    pub supports_reasoning: Option<bool>,
    pub context_window: Option<i64>,
    pub recent_successes: i64,
    pub recent_errors: i64,
    pub recent_429s: i64,
    pub recent_timeouts: i64,
    pub quota_headroom: u32,
    pub breakdown: DiagnosticScoreBreakdown,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DiagnosticScoreBreakdown {
    pub capability_match: i32,
    pub reliability: i32,
    pub quota: i32,
    pub provider_priority: i32,
    pub cost: i32,
    pub penalty: i32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RoutingGroupSummary {
    pub name: String,
    pub route_prefix: String,
    pub provider_names: Vec<String>,
    pub agents: Vec<String>,
    pub providers: Vec<RoutingGroupProviderSummary>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RoutingGroupProviderSummary {
    pub name: String,
    pub configured: bool,
    pub enabled: bool,
    pub health_status: Option<String>,
    pub available_keys: usize,
    pub total_keys: usize,
    pub models_count: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RoutingRouteSummary {
    pub kind: String,
    pub name: String,
    pub route_prefix: String,
    pub models_route: String,
    pub chat_route: String,
    pub enabled: bool,
    pub providers: Vec<String>,
    pub agents: Vec<String>,
}

const RESERVED_PROVIDER_PREFIXES: &[&str] = &[
    "v1",
    "auto",
    "agents",
    "provider-groups",
    "admin",
    "health",
    "status",
    "metrics",
];

pub fn is_reserved_provider_prefix(value: &str) -> bool {
    RESERVED_PROVIDER_PREFIXES
        .iter()
        .any(|reserved| value.eq_ignore_ascii_case(reserved))
}

pub async fn chat(
    state: &AppState,
    scope: AdaptiveScope,
    mut request: ChatCompletionRequest,
) -> GatewayResult<ChatResponse> {
    ensure_enabled(state)?;
    if request.stream.unwrap_or(false) {
        return Err(GatewayError::InvalidRequest(
            "adaptive streaming is not implemented yet; use non-streaming or /v1".into(),
        ));
    }

    let selection = select_model(state, &scope, &request)?;
    let route_task = route_task(state, &scope, &request.model);
    let agent = scope_agent(&scope).or(request.agent_name.as_deref());
    let profile = build_task_profile(agent, &request, route_task.as_deref());
    let Some(provider) = state.providers.get(&selection.provider) else {
        return Err(GatewayError::ProviderNotFound(selection.provider));
    };
    if !state
        .keyhub
        .reserve_key(&selection.provider, &selection.api_key)
    {
        return Err(GatewayError::NoAvailableKeys(selection.provider));
    }

    request.model = selection.model.clone();
    request.agent_name = scope_agent(&scope)
        .map(str::to_string)
        .or(request.agent_name);
    request.stream = Some(false);
    let started = std::time::Instant::now();

    tracing::info!(
        provider = %selection.provider,
        model = %selection.model,
        score = selection.score,
        task_kinds = ?selection.task_kinds,
        stage = "adaptive_route_decision",
        "Adaptive route selected candidate"
    );

    match provider.chat(&selection.api_key, request).await {
        Ok(response) => {
            let (prompt_tokens, completion_tokens) = crate::models::extract_usage(&response.body);
            state.keyhub.report_reserved_success(
                &selection.provider,
                &selection.api_key,
                prompt_tokens,
                completion_tokens,
            );
            if let Some(meta) = state.model_meta.as_ref() {
                for task in &selection.task_kinds {
                    let _ = meta.record_task_usage(
                        &selection.provider,
                        &selection.model,
                        scope_agent(&scope),
                        &format!("{task:?}").to_lowercase(),
                        true,
                        started.elapsed().as_millis() as i64,
                        prompt_tokens.map(i64::from),
                        completion_tokens.map(i64::from),
                    );
                }
            }
            record_profile_observations(
                state,
                &selection.provider,
                &selection.model,
                &profile,
                "success",
            );
            Ok(response)
        }
        Err(error) => {
            state
                .keyhub
                .report_gateway_error(&selection.provider, &selection.api_key, &error);
            if let Some(meta) = state.model_meta.as_ref() {
                for task in &selection.task_kinds {
                    let _ = meta.record_task_usage(
                        &selection.provider,
                        &selection.model,
                        scope_agent(&scope),
                        &format!("{task:?}").to_lowercase(),
                        false,
                        started.elapsed().as_millis() as i64,
                        None,
                        None,
                    );
                }
            }
            record_profile_observations(
                state,
                &selection.provider,
                &selection.model,
                &profile,
                "failure",
            );
            Err(error)
        }
    }
}

pub fn routing_diagnostics(
    state: &AppState,
    scope: &AdaptiveScope,
    request: &ChatCompletionRequest,
) -> GatewayResult<RoutingDiagnostics> {
    ensure_enabled(state)?;
    if let AdaptiveScope::Provider(provider) = scope
        && is_reserved_provider_prefix(provider)
    {
        return Err(GatewayError::InvalidRequest(format!(
            "'{provider}' is a reserved gateway route prefix"
        )));
    }

    let route_task = route_task(state, scope, &request.model);
    let agent = scope_agent(scope).or(request.agent_name.as_deref());
    let profile = build_task_profile(agent, request, route_task.as_deref());
    let constraints = constraints_for_scope(state, scope)?;
    let concrete_model = concrete_requested_model(state, &request.model);
    let candidates = collect_candidates(state, concrete_model.as_deref(), &constraints);
    let scored = score_candidates(&profile, candidates, &constraints);

    Ok(RoutingDiagnostics {
        adaptive_enabled: state.config.adaptive_routing.enabled,
        scope: scope_label(scope),
        requested_model: request.model.clone(),
        concrete_model,
        agent: agent.map(str::to_string),
        task_kinds: profile.task_kinds.iter().map(task_kind_name).collect(),
        estimated_prompt_tokens: profile.estimated_prompt_tokens,
        candidates: scored
            .into_iter()
            .take(state.config.adaptive_routing.candidate_limit)
            .enumerate()
            .map(|(index, scored)| DiagnosticCandidate {
                provider: scored.candidate.provider,
                model: scored.candidate.model,
                tier: candidate_tier_name(scored.candidate.tier).into(),
                score: scored.score,
                selected: index == 0,
                supports_vision: scored.candidate.supports_vision,
                supports_tools: scored.candidate.supports_tools,
                supports_reasoning: scored.candidate.supports_reasoning,
                context_window: scored.candidate.context_window,
                recent_successes: scored.candidate.recent_successes,
                recent_errors: scored.candidate.recent_errors,
                recent_429s: scored.candidate.recent_429s,
                recent_timeouts: scored.candidate.recent_timeouts,
                quota_headroom: scored.candidate.quota_headroom,
                breakdown: DiagnosticScoreBreakdown {
                    capability_match: scored.breakdown.capability_match,
                    reliability: scored.breakdown.reliability,
                    quota: scored.breakdown.quota,
                    provider_priority: scored.breakdown.provider_priority,
                    cost: scored.breakdown.cost,
                    penalty: scored.breakdown.penalty,
                },
            })
            .collect(),
    })
}

pub fn routing_groups_summary(state: &AppState) -> Vec<RoutingGroupSummary> {
    let snapshot = state.keyhub.snapshot();
    let health = state
        .health_registry
        .snapshot()
        .into_iter()
        .map(|health| (health.provider.clone(), health))
        .collect::<std::collections::HashMap<_, _>>();
    let now = chrono::Utc::now().timestamp() as u64;

    let mut groups = state
        .config
        .adaptive_routing
        .routing_groups
        .iter()
        .map(|(name, group)| {
            let providers = group
                .providers
                .iter()
                .map(|provider| {
                    let configured = state.config.providers.contains_key(provider);
                    let enabled = state
                        .config
                        .providers
                        .get(provider)
                        .map(|config| config.enabled)
                        .unwrap_or(false);
                    let keys = snapshot
                        .iter()
                        .find(|(name, _)| name == provider)
                        .map(|(_, keys)| keys.as_slice())
                        .unwrap_or(&[]);
                    let available_keys = keys
                        .iter()
                        .filter(|key| {
                            key.status == crate::models::KeyStatus::Available
                                && !key.is_rate_limited(now)
                        })
                        .count();
                    let models_count = keys
                        .iter()
                        .flat_map(|key| key.models.iter())
                        .collect::<std::collections::BTreeSet<_>>()
                        .len();

                    RoutingGroupProviderSummary {
                        name: provider.clone(),
                        configured,
                        enabled,
                        health_status: health.get(provider).map(|row| row.status.clone()),
                        available_keys,
                        total_keys: keys.len(),
                        models_count,
                    }
                })
                .collect::<Vec<_>>();
            let agents = state
                .config
                .adaptive_routing
                .agent_profiles
                .iter()
                .filter(|(_, profile)| profile.provider_groups.iter().any(|group| group == name))
                .map(|(agent, _)| agent.clone())
                .collect::<Vec<_>>();

            RoutingGroupSummary {
                name: name.clone(),
                route_prefix: format!("/provider-groups/{name}/v1"),
                provider_names: group.providers.clone(),
                agents,
                providers,
            }
        })
        .collect::<Vec<_>>();

    groups.sort_by(|left, right| left.name.cmp(&right.name));
    groups
}

pub fn routing_routes_summary(state: &AppState) -> Vec<RoutingRouteSummary> {
    let adaptive_enabled = state.config.adaptive_routing.enabled;
    let mut routes = vec![RoutingRouteSummary {
        kind: "auto".into(),
        name: "auto".into(),
        route_prefix: "/auto/v1".into(),
        models_route: "/auto/v1/models".into(),
        chat_route: "/auto/v1/chat/completions".into(),
        enabled: adaptive_enabled,
        providers: Vec::new(),
        agents: Vec::new(),
    }];

    let mut agents = state
        .config
        .adaptive_routing
        .agent_profiles
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    agents.sort();
    for agent in agents {
        let prefix = format!("/agents/{agent}/v1");
        let providers = state
            .config
            .adaptive_routing
            .agent_profiles
            .get(&agent)
            .map(|profile| {
                profile
                    .provider_groups
                    .iter()
                    .filter_map(|group| state.config.adaptive_routing.routing_groups.get(group))
                    .flat_map(|group| group.providers.clone())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        routes.push(RoutingRouteSummary {
            kind: "agent".into(),
            name: agent.clone(),
            route_prefix: prefix.clone(),
            models_route: format!("{prefix}/models"),
            chat_route: format!("{prefix}/chat/completions"),
            enabled: adaptive_enabled,
            providers,
            agents: vec![agent],
        });
    }

    let mut providers = state.config.providers.keys().cloned().collect::<Vec<_>>();
    providers.sort();
    for provider in providers {
        if is_reserved_provider_prefix(&provider) {
            continue;
        }
        let prefix = format!("/{provider}/v1");
        routes.push(RoutingRouteSummary {
            kind: "provider".into(),
            name: provider.clone(),
            route_prefix: prefix.clone(),
            models_route: format!("{prefix}/models"),
            chat_route: format!("{prefix}/chat/completions"),
            enabled: adaptive_enabled
                && state
                    .config
                    .providers
                    .get(&provider)
                    .map(|config| config.enabled)
                    .unwrap_or(false),
            providers: vec![provider],
            agents: Vec::new(),
        });
    }

    for group in routing_groups_summary(state) {
        let prefix = group.route_prefix.clone();
        routes.push(RoutingRouteSummary {
            kind: "provider_group".into(),
            name: group.name,
            route_prefix: prefix.clone(),
            models_route: format!("{prefix}/models"),
            chat_route: format!("{prefix}/chat/completions"),
            enabled: adaptive_enabled,
            providers: group.provider_names,
            agents: group.agents,
        });
    }

    routes
}

pub fn scoped_models(state: &AppState, scope: &AdaptiveScope) -> GatewayResult<Vec<ModelInfo>> {
    ensure_enabled(state)?;
    if let AdaptiveScope::Provider(provider) = scope
        && is_reserved_provider_prefix(provider)
    {
        return Err(GatewayError::InvalidRequest(format!(
            "'{provider}' is a reserved gateway route prefix"
        )));
    }

    let constraints = constraints_for_scope(state, scope)?;
    let disabled = state.disabled_models.read();
    let now = chrono::Utc::now().timestamp() as u64;
    let mut models = Vec::new();

    for (provider, keys) in state.keyhub.snapshot() {
        if !provider_allowed(&provider, &constraints) {
            continue;
        }
        let disabled_for_provider = disabled.get(&provider);
        for key in keys {
            if key.tier != KeyTier::Free
                || key.status != crate::models::KeyStatus::Available
                || key.is_rate_limited(now)
            {
                continue;
            }
            for model in &key.models {
                if disabled_for_provider
                    .map(|set| set.contains(model))
                    .unwrap_or(false)
                {
                    continue;
                }
                models.push(ModelInfo {
                    id: model.clone(),
                    object: "model".into(),
                    created: chrono::Utc::now().timestamp(),
                    owned_by: provider.clone(),
                    provider: Some(provider.clone()),
                    context_window: None,
                    supports_vision: None,
                    supports_tools: None,
                    supports_reasoning: None,
                    pricing_prompt: None,
                    pricing_completion: None,
                });
            }
        }
    }

    models = crate::api::models::merge_model_families(models);
    models.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then(left.owned_by.cmp(&right.owned_by))
    });
    models.dedup_by(|left, right| left.id == right.id && left.owned_by == right.owned_by);
    Ok(models)
}

pub fn select_model(
    state: &AppState,
    scope: &AdaptiveScope,
    request: &ChatCompletionRequest,
) -> GatewayResult<AdaptiveSelection> {
    ensure_enabled(state)?;
    if let AdaptiveScope::Provider(provider) = scope
        && is_reserved_provider_prefix(provider)
    {
        return Err(GatewayError::InvalidRequest(format!(
            "'{provider}' is a reserved gateway route prefix"
        )));
    }

    let route_task = route_task(state, scope, &request.model);
    let agent = scope_agent(scope).or(request.agent_name.as_deref());
    let profile = build_task_profile(agent, request, route_task.as_deref());
    let constraints = constraints_for_scope(state, scope)?;
    let exact_model = concrete_requested_model(state, &request.model);
    let candidates = collect_candidates(state, exact_model.as_deref(), &constraints);
    let scored = score_candidates(&profile, candidates, &constraints);
    let Some(best) = scored.into_iter().next() else {
        return Err(GatewayError::ModelNotFound(request.model.clone()));
    };
    let api_key = best
        .candidate
        .api_key
        .clone()
        .ok_or_else(|| GatewayError::NoAvailableKeys(best.candidate.provider.clone()))?;

    Ok(AdaptiveSelection {
        provider: best.candidate.provider,
        model: best.candidate.model,
        api_key,
        score: best.score,
        task_kinds: profile.task_kinds,
    })
}

fn ensure_enabled(state: &AppState) -> GatewayResult<()> {
    if state.config.adaptive_routing.enabled {
        Ok(())
    } else {
        Err(GatewayError::InvalidRequest(
            "adaptive routing is disabled in config".into(),
        ))
    }
}

fn scope_agent(scope: &AdaptiveScope) -> Option<&str> {
    match scope {
        AdaptiveScope::Agent(agent) => Some(agent),
        _ => None,
    }
}

fn scope_label(scope: &AdaptiveScope) -> String {
    match scope {
        AdaptiveScope::Auto => "auto".into(),
        AdaptiveScope::Agent(agent) => format!("agent:{agent}"),
        AdaptiveScope::Provider(provider) => format!("provider:{provider}"),
        AdaptiveScope::ProviderGroup(group) => format!("provider_group:{group}"),
    }
}

fn task_kind_name(task: &profile::TaskKind) -> String {
    format!("{task:?}").to_lowercase()
}

fn candidate_tier_name(tier: CandidateTier) -> &'static str {
    match tier {
        CandidateTier::Free => "free",
        CandidateTier::Paid => "paid",
        CandidateTier::Unknown => "unknown",
    }
}

fn route_task(state: &AppState, scope: &AdaptiveScope, requested_model: &str) -> Option<String> {
    state
        .config
        .adaptive_routing
        .auto_models
        .get(requested_model)
        .map(|auto| auto.task.clone())
        .or_else(|| match scope {
            AdaptiveScope::Agent(agent) => state
                .config
                .adaptive_routing
                .agent_profiles
                .get(agent)
                .and_then(|profile| {
                    state
                        .config
                        .adaptive_routing
                        .auto_models
                        .get(&profile.default_auto_model)
                        .map(|auto| auto.task.clone())
                }),
            _ => None,
        })
}

fn constraints_for_scope(
    state: &AppState,
    scope: &AdaptiveScope,
) -> GatewayResult<RouteConstraints> {
    let allow_paid = state.config.adaptive_routing.allow_paid;
    match scope {
        AdaptiveScope::Auto => Ok(RouteConstraints {
            allow_paid,
            ..RouteConstraints::default()
        }),
        AdaptiveScope::Agent(agent) => {
            let groups = state
                .config
                .adaptive_routing
                .agent_profiles
                .get(agent)
                .map(|profile| profile.provider_groups.clone())
                .unwrap_or_default();
            let providers = groups
                .iter()
                .filter_map(|group| state.config.adaptive_routing.routing_groups.get(group))
                .flat_map(|group| group.providers.clone())
                .collect::<Vec<_>>();
            Ok(RouteConstraints {
                provider_group: if providers.is_empty() {
                    None
                } else {
                    Some(providers)
                },
                allow_paid,
                ..RouteConstraints::default()
            })
        }
        AdaptiveScope::Provider(provider) => Ok(RouteConstraints {
            provider: Some(provider.clone()),
            allow_paid,
            ..RouteConstraints::default()
        }),
        AdaptiveScope::ProviderGroup(group) => {
            let Some(group) = state.config.adaptive_routing.routing_groups.get(group) else {
                return Err(GatewayError::InvalidRequest(
                    "unknown provider group".into(),
                ));
            };
            Ok(RouteConstraints {
                provider_group: Some(group.providers.clone()),
                allow_paid,
                ..RouteConstraints::default()
            })
        }
    }
}

fn concrete_requested_model(state: &AppState, requested_model: &str) -> Option<String> {
    if requested_model == "auto"
        || state
            .config
            .adaptive_routing
            .auto_models
            .contains_key(requested_model)
    {
        return None;
    }
    state
        .config
        .models
        .get(requested_model)
        .map(|alias| alias.model.clone())
        .or_else(|| Some(requested_model.to_string()))
}

fn collect_candidates(
    state: &AppState,
    exact_model: Option<&str>,
    constraints: &RouteConstraints,
) -> Vec<AdaptiveCandidate> {
    let disabled = state.disabled_models.read();
    let now = chrono::Utc::now().timestamp() as u64;
    let mut candidates = Vec::new();

    for (provider, keys) in state.keyhub.snapshot() {
        if !provider_allowed(&provider, constraints) {
            continue;
        }
        let disabled_for_provider = disabled.get(&provider);
        for key in keys {
            if key.status != crate::models::KeyStatus::Available || key.is_rate_limited(now) {
                continue;
            }
            for model in &key.models {
                if exact_model.is_some_and(|requested| requested != model) {
                    continue;
                }
                if disabled_for_provider
                    .map(|set| set.contains(model))
                    .unwrap_or(false)
                {
                    continue;
                }
                let meta = state
                    .model_meta
                    .as_ref()
                    .and_then(|store| store.try_get_model(&provider, model).ok().flatten());
                candidates.push(AdaptiveCandidate {
                    provider: provider.clone(),
                    model: model.clone(),
                    api_key: Some(key.key.clone()),
                    tier: match key.tier {
                        KeyTier::Free => CandidateTier::Free,
                        KeyTier::Paid => CandidateTier::Paid,
                        KeyTier::Unknown => CandidateTier::Unknown,
                    },
                    supports_vision: meta.as_ref().and_then(|row| row.supports_vision),
                    supports_tools: meta.as_ref().and_then(|row| row.supports_tools),
                    supports_reasoning: meta.as_ref().and_then(|row| row.supports_reasoning),
                    context_window: meta.as_ref().and_then(|row| row.context_window),
                    recent_successes: key.success_count.min(i64::MAX as u64) as i64,
                    recent_errors: key.total_fail_count.min(i64::MAX as u64) as i64,
                    recent_429s: u64::from(key.last_error_status == Some(429)) as i64,
                    recent_timeouts: 0,
                    quota_headroom: 100,
                    provider_priority: state
                        .config
                        .providers
                        .get(&provider)
                        .map(|provider| provider.priority.min(i32::MAX as u32) as i32)
                        .unwrap_or(0),
                    prompt_price: meta.as_ref().and_then(|row| row.pricing_prompt),
                });
            }
        }
    }
    candidates
}

fn provider_allowed(provider: &str, constraints: &RouteConstraints) -> bool {
    if let Some(only_provider) = &constraints.provider
        && only_provider != provider
    {
        return false;
    }
    if let Some(group) = &constraints.provider_group
        && !group.iter().any(|candidate| candidate == provider)
    {
        return false;
    }
    true
}

pub fn status_for_adaptive_error(error: &GatewayError) -> StatusCode {
    StatusCode::from_u16(error.http_status()).unwrap_or(StatusCode::BAD_GATEWAY)
}
