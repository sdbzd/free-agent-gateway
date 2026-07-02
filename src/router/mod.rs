/// Router: Routes requests to the appropriate provider and model.
///
/// Handles:
/// - Model alias resolution
/// - Agent-aware routing
/// - Provider fallback chain
/// - Routing strategies (round-robin, random, least-failed, priority)
use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use futures::StreamExt;
use parking_lot::RwLock;
use tokio::io::AsyncWriteExt;

use bytes::Bytes;

use crate::config::{Config, KeyTier, RoutingStrategy};
use crate::error::{GatewayError, GatewayResult, sanitize_diagnostic};
use crate::keyhub::{KeyHub, key_fingerprint};
use crate::metadata::{DeploymentStateRow, ModelMetaStore};
use crate::models::{ChatCompletionRequest, ChatMessage};
use crate::providers::BoxedProvider;
use crate::providers::traits::{ChatResponse, StreamResponse};

static ROUTER_CANDIDATE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn record_model_usage(
    model_meta: &Option<ModelMetaStore>,
    provider: &str,
    model: &str,
    success: bool,
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
    tokens_reported: bool,
) {
    if let Some(meta) = model_meta {
        meta.learn_from_request_with_token_source(
            provider,
            model,
            success,
            prompt_tokens.map(i64::from),
            completion_tokens.map(i64::from),
            tokens_reported,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn record_request_attempt(
    model_meta: &Option<ModelMetaStore>,
    request_id: &str,
    attempt_index: i64,
    provider: &str,
    model: &str,
    api_key: &str,
    success: bool,
    error_category: &str,
    http_status: Option<u16>,
    error_message: Option<&str>,
    cooldown_seconds: Option<i64>,
    fallback: bool,
) {
    let Some(meta) = model_meta else {
        return;
    };
    if let Err(error) = meta.record_request_attempt(
        request_id,
        attempt_index,
        provider,
        model,
        &key_fingerprint(api_key),
        success,
        error_category,
        http_status,
        error_message,
        cooldown_seconds,
        fallback,
    ) {
        tracing::warn!(
            request_id,
            provider,
            model,
            error = %sanitize_diagnostic(&error.to_string()),
            "Failed to record request attempt"
        );
    }
}

/// A resolved route: which provider and model to use.
#[derive(Debug, Clone)]
pub struct ResolvedRoute {
    pub provider_name: String,
    pub model: String,
}

#[derive(Debug, Clone)]
struct RouteCandidate {
    provider: String,
    key: String,
    model: String,
    provider_index: usize,
    usage_sort_key: (u32, u32, u32, u32, u32),
    fail_count: u32,
    total_fail_count: u64,
    deployment_penalty: u32,
}

#[derive(Debug, Clone)]
struct SelectedCandidate {
    provider: String,
    key: String,
    model: String,
}

type DeploymentStateKey = (String, String, String);

// 鈹€鈹€鈹€ Context Handoff 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€
/// When the gateway falls back to another provider, inject a system message
/// so the new model has awareness of the handoff. This prevents confusion
/// about "missing context" when a conversation was partially processed.
fn inject_context_handoff(
    mut request: ChatCompletionRequest,
    previous_provider: &str,
    new_provider: &str,
    model: &str,
) -> ChatCompletionRequest {
    let handoff_msg = format!(
        "[Context handoff: Previous provider \"{}\" failed. \
         Continuing with \"{}\" (model: {}). \
         The full conversation history is preserved below.]",
        previous_provider, new_provider, model,
    );
    request.messages.insert(
        0,
        ChatMessage {
            role: "system".into(),
            content: serde_json::Value::String(handoff_msg),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            extra: serde_json::Map::new(),
        },
    );
    request
}

/// Check if a model name suggests vision capability.
fn model_supports_vision(model: &str) -> bool {
    let lower = model.to_lowercase();
    // Common vision model patterns
    lower.contains("vision")
        || lower.contains("gpt-4o")
        || lower.contains("gpt-4.1")
        || lower.contains("claude-3")
        || lower.contains("claude-4")
        || lower.contains("gemini-1.5")
        || lower.contains("gemini-2")
        || lower.contains("llava")
        || lower.contains("qwen-vl")
        || lower.contains("qwen2-vl")
        || lower.contains("internvl")
        || lower.contains("cogvlm")
        || lower.contains("deepseek-vl")
        || lower.contains("idefics")
        || lower.contains("florence")
        || lower.contains("phi-3-vision")
        || lower.contains("phi-4-vision")
        || lower.contains("pixtral")
}

/// The main router.
pub struct Router {
    config: Arc<Config>,
    providers: Arc<DashMap<String, BoxedProvider>>,
    keyhub: Arc<KeyHub>,
    pub disabled_models: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    /// Model metadata store for auto-learning from failures (optional).
    model_meta: Option<ModelMetaStore>,
    /// Per-provider discovery backoff state.
    discovery_backoff: RwLock<HashMap<String, DiscoveryState>>,
}

/// Tracks model discovery attempts to implement backoff on repeated failures.
struct DiscoveryState {
    last_attempt: u64,
    fail_count: u32,
}

/// Backoff schedule for failed model discovery (seconds).
const DISCOVERY_BACKOFF_S: &[u64] = &[60, 300, 600, 1800];

/// Strip OpenRouter-style pricing suffixes from model names.
/// OpenRouter appends `:free`, `:paid`, `:extended` etc. to model IDs,
/// but backend providers (NVIDIA NIM, GitHub Models) use the bare model name.
fn strip_or_suffixes(model: &str) -> String {
    if let Some(pos) = model.rfind(':') {
        let suffix = &model[pos + 1..];
        if matches!(suffix, "free" | "paid" | "extended") {
            return model[..pos].to_string();
        }
    }
    model.to_string()
}

fn has_openrouter_suffix(model: &str) -> bool {
    model
        .rfind(':')
        .map(|pos| matches!(&model[pos + 1..], "free" | "paid" | "extended"))
        .unwrap_or(false)
}

fn is_openrouter_native_model(model: &str) -> bool {
    has_openrouter_suffix(model)
}

fn candidate_model_ids(model: &str) -> Vec<String> {
    if has_openrouter_suffix(model) {
        return vec![model.to_string()];
    }
    vec![model.to_string(), format!("{model}:free")]
}

fn model_for_resolved_provider(provider_name: &str, model: &str) -> String {
    if provider_name.eq_ignore_ascii_case("openrouter") {
        model.to_string()
    } else {
        strip_or_suffixes(model)
    }
}

async fn rtk_pipe_compact(command: &str, text: &str, timeout: Duration) -> Option<String> {
    let mut child = tokio::process::Command::new(command)
        .arg("pipe")
        .arg("--ultra-compact")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;

    let mut stdin = child.stdin.take()?;
    let input = text.as_bytes().to_vec();
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(&input).await;
    });

    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .ok()?
        .ok()?;
    let _ = writer.await;
    if !output.status.success() {
        return None;
    }
    let compacted = String::from_utf8(output.stdout).ok()?;
    let compacted = compacted.trim();
    if compacted.is_empty() || compacted.len() >= text.len() {
        None
    } else {
        Some(compacted.to_string())
    }
}

impl Router {
    /// Create a new router.
    pub fn new(
        config: Arc<Config>,
        providers: Arc<DashMap<String, BoxedProvider>>,
        keyhub: Arc<KeyHub>,
        disabled_models: Arc<RwLock<HashMap<String, HashSet<String>>>>,
        model_meta: Option<ModelMetaStore>,
    ) -> Self {
        Self {
            config,
            providers,
            keyhub,
            disabled_models,
            model_meta,
            discovery_backoff: RwLock::new(HashMap::new()),
        }
    }

    /// Resolve a model name to a concrete provider and model.
    ///
    /// Resolution order:
    /// 1. Check model aliases
    /// 2. If agent_name is set, check agent default model
    /// 3. Try to find a provider that serves this model directly
    /// 4. Try each fallback provider
    /// 5. Strip OpenRouter suffixes (`:free`, `:paid`, `:extended`) and retry
    pub fn resolve(&self, model: &str, agent_name: Option<&str>) -> GatewayResult<ResolvedRoute> {
        // Try the raw model name first
        if let Ok(route) = self.resolve_raw(model, agent_name) {
            return Ok(route);
        }

        // If resolution fails and model has an OpenRouter suffix, strip and retry
        let stripped = strip_or_suffixes(model);
        if stripped != model {
            tracing::debug!(
                original = %model,
                stripped = %stripped,
                "Retrying resolution after stripping OpenRouter suffix"
            );
            if let Ok(route) = self.resolve_raw(&stripped, agent_name) {
                return Ok(route);
            }
        }

        Err(GatewayError::ModelNotFound(model.to_string()))
    }

    async fn compress_context_if_enabled(
        &self,
        request: &ChatCompletionRequest,
    ) -> ChatCompletionRequest {
        let compression = &self.config.context_compression;
        if !compression.enabled || compression.command.trim().is_empty() {
            return request.clone();
        }

        let mut compressed = request.clone();
        for message in &mut compressed.messages {
            if message.role != "tool" {
                continue;
            }
            let text = crate::models::content_to_text(&message.content);
            if crate::models::estimate_text_tokens(&text) < compression.min_message_tokens {
                continue;
            }
            if let Some(compacted) = rtk_pipe_compact(
                &compression.command,
                &text,
                Duration::from_secs(compression.timeout_seconds.max(1)),
            )
            .await
            {
                message.content =
                    serde_json::Value::String(format!("[Compressed tool output]\n{compacted}"));
            }
        }
        compressed
    }

    /// Internal resolution without suffix retry logic.
    fn resolve_raw(&self, model: &str, agent_name: Option<&str>) -> GatewayResult<ResolvedRoute> {
        // 1. Check agent default model first (if agent name provided)
        if let Some(agent) = agent_name
            && let Some(agent_cfg) = self.config.agents.get(agent)
            && model == agent_cfg.default_model
            && let Some(alias) = self.config.models.get(&agent_cfg.default_model)
        {
            return Ok(ResolvedRoute {
                provider_name: alias.provider.clone(),
                model: alias.model.clone(),
            });
        }

        // 2. Check model aliases directly
        if let Some(alias) = self.config.models.get(model) {
            return Ok(ResolvedRoute {
                provider_name: alias.provider.clone(),
                model: alias.model.clone(),
            });
        }

        // 3. If model contains '/', treat as direct "provider/model" format
        if let Some(slash_pos) = model.find('/') {
            let provider = &model[..slash_pos];
            let model_name = &model[slash_pos + 1..];
            if self.providers.contains_key(provider) {
                return Ok(ResolvedRoute {
                    provider_name: provider.to_string(),
                    model: model_for_resolved_provider(provider, model_name),
                });
            }
        }

        // OpenRouter's pricing suffixes are part of the upstream model ID. If
        // such a model is requested directly, prefer OpenRouter before generic
        // fallback providers that need bare model names.
        if has_openrouter_suffix(model) && self.providers.contains_key("openrouter") {
            return Ok(ResolvedRoute {
                provider_name: "openrouter".to_string(),
                model: model.to_string(),
            });
        }

        // 4. Try each fallback provider to see if they can serve this model
        for provider_name in &self.config.fallback {
            if self.providers.contains_key(provider_name) {
                return Ok(ResolvedRoute {
                    provider_name: provider_name.clone(),
                    model: model_for_resolved_provider(provider_name, model),
                });
            }
        }

        Err(GatewayError::ModelNotFound(model.to_string()))
    }

    /// Build the provider fallback chain for a given resolved route.
    pub fn build_provider_chain(&self, primary: &str) -> Vec<String> {
        let mut chain = vec![primary.to_string()];
        for fb in &self.config.fallback {
            if fb != primary && !chain.contains(fb) {
                chain.push(fb.clone());
            }
        }
        chain
    }

    /// Determine the model name to send to a fallback provider.
    pub fn model_for_provider(&self, provider_name: &str, route: &ResolvedRoute) -> String {
        let _ = provider_name;
        route.model.clone()
    }

    fn provider_order(&self, preferred: &str) -> Vec<String> {
        let mut providers = Vec::new();
        if !preferred.is_empty() && self.providers.contains_key(preferred) {
            providers.push(preferred.to_string());
        }
        for provider in &self.config.fallback {
            if self.providers.contains_key(provider) && !providers.contains(provider) {
                providers.push(provider.clone());
            }
        }
        let mut remaining: Vec<String> = self
            .providers
            .iter()
            .map(|provider| provider.key().clone())
            .filter(|provider| !providers.contains(provider))
            .collect();
        remaining.sort();
        providers.extend(remaining);
        providers
    }

    fn candidates(
        &self,
        route: &ResolvedRoute,
        agent_name: Option<&str>,
    ) -> Vec<SelectedCandidate> {
        let disabled = self.disabled_models.read();
        let model_disabled = |provider: &str, model: &str| -> bool {
            disabled
                .get(provider)
                .map(|set| set.contains(model))
                .unwrap_or(false)
        };
        let route_models = candidate_model_ids(&route.model);
        let deployment_states = self.deployment_state_map();
        let now = chrono::Utc::now().timestamp();

        let mut candidates = Vec::<RouteCandidate>::new();
        for (provider_index, provider) in self
            .provider_order(&route.provider_name)
            .into_iter()
            .enumerate()
        {
            for model in &route_models {
                if model_disabled(&provider, model) {
                    tracing::debug!(
                        provider = %provider,
                        model = %model,
                        "Skipping provider: model is disabled"
                    );
                    continue;
                }
                candidates.extend(
                    self.free_candidate_infos_for_model(&provider, model)
                        .into_iter()
                        .filter_map(|candidate| {
                            let key_id = key_fingerprint(&candidate.key);
                            let state_key = (provider.clone(), model.clone(), key_id.clone());
                            let deployment_state = deployment_states.get(&state_key);
                            if deployment_in_active_cooldown(deployment_state, now) {
                                tracing::debug!(
                                    provider = %provider,
                                    model = %model,
                                    key_id = %key_id,
                                    cooldown_until = ?deployment_state.and_then(|state| state.cooldown_until),
                                    "Skipping candidate: deployment_state cooldown is active"
                                );
                                return None;
                            }
                            Some(RouteCandidate {
                                provider: provider.clone(),
                                key: candidate.key,
                                model: model.clone(),
                                provider_index,
                                usage_sort_key: candidate.usage_sort_key,
                                fail_count: candidate.fail_count,
                                total_fail_count: candidate.total_fail_count,
                                deployment_penalty: deployment_penalty(deployment_state),
                            })
                        }),
                );
            }
        }

        self.order_candidates(candidates, agent_name)
    }

    fn free_candidate_infos_for_model(
        &self,
        provider: &str,
        model: &str,
    ) -> Vec<crate::keyhub::FreeKeyCandidate> {
        if provider.eq_ignore_ascii_case("openrouter") && is_openrouter_native_model(model) {
            return self.keyhub.free_provider_candidate_infos(provider);
        }
        self.keyhub.free_candidate_infos(provider, model)
    }

    fn deployment_state_map(&self) -> HashMap<DeploymentStateKey, DeploymentStateRow> {
        let Some(meta) = self.model_meta.as_ref() else {
            return HashMap::new();
        };
        match meta.get_deployment_states() {
            Ok(states) => states
                .into_iter()
                .map(|state| {
                    (
                        (
                            state.provider.clone(),
                            state.model_id.clone(),
                            state.key_id.clone(),
                        ),
                        state,
                    )
                })
                .collect(),
            Err(error) => {
                tracing::warn!(
                    error = %sanitize_diagnostic(&error.to_string()),
                    "Failed to load deployment_state for routing"
                );
                HashMap::new()
            }
        }
    }

    fn is_openrouter_native_route(&self, route: &ResolvedRoute) -> bool {
        route.provider_name.eq_ignore_ascii_case("openrouter")
            && has_openrouter_suffix(&route.model)
    }

    fn order_candidates(
        &self,
        mut candidates: Vec<RouteCandidate>,
        agent_name: Option<&str>,
    ) -> Vec<SelectedCandidate> {
        match self.config.routing.strategy {
            RoutingStrategy::LeastRate => {
                candidates.sort_by_key(|candidate| {
                    (
                        candidate.deployment_penalty,
                        candidate.usage_sort_key,
                        candidate.provider_index,
                    )
                });
            }
            RoutingStrategy::LeastFailed => {
                candidates.sort_by_key(|candidate| {
                    (
                        candidate.deployment_penalty,
                        candidate.fail_count,
                        candidate.total_fail_count.min(u32::MAX as u64) as u32,
                        candidate.provider_index,
                    )
                });
            }
            RoutingStrategy::Priority => {
                candidates.sort_by_key(|candidate| {
                    (candidate.provider_index, candidate.deployment_penalty)
                });
            }
            RoutingStrategy::RoundRobin => {
                candidates.sort_by_key(|candidate| candidate.deployment_penalty);
                let healthy_len = lowest_penalty_prefix_len(&candidates);
                if healthy_len > 1 {
                    let counter = ROUTER_CANDIDATE_COUNTER.fetch_add(1, Ordering::Relaxed);
                    let shift = (counter as usize) % healthy_len;
                    candidates[..healthy_len].rotate_left(shift);
                }
            }
            RoutingStrategy::Random => {
                candidates.sort_by_key(|candidate| candidate.deployment_penalty);
                let healthy_len = lowest_penalty_prefix_len(&candidates);
                if healthy_len > 1 {
                    let shift = rand::random::<usize>() % healthy_len;
                    candidates[..healthy_len].rotate_left(shift);
                }
            }
        }

        if candidates.len() > 1
            && let Some(agent) = agent_name.filter(|agent| !agent.is_empty())
        {
            let hash: u32 = agent
                .bytes()
                .fold(0u32, |acc, byte| acc.wrapping_add(byte as u32));
            let shift = (hash as usize) % candidates.len();
            candidates.rotate_left(shift);
        }

        candidates
            .into_iter()
            .map(|candidate| SelectedCandidate {
                provider: candidate.provider,
                key: candidate.key,
                model: candidate.model,
            })
            .collect()
    }

    async fn refresh_free_models(&self, request_id: &str) {
        let provider_names = self.provider_order("");
        let now = chrono::Utc::now().timestamp() as u64;
        for provider_name in provider_names {
            // Check discovery backoff before attempting
            {
                let backoff_map = self.discovery_backoff.read();
                if let Some(state) = backoff_map.get(&provider_name) {
                    let tier = (state.fail_count as usize - 1).min(DISCOVERY_BACKOFF_S.len() - 1);
                    let backoff_s = DISCOVERY_BACKOFF_S[tier];
                    if now - state.last_attempt < backoff_s {
                        tracing::debug!(
                            request_id,
                            provider = %provider_name,
                            backoff_s,
                            "Skipping model discovery (backoff)"
                        );
                        continue;
                    }
                }
            }

            let Some(provider) = self.providers.get(&provider_name) else {
                continue;
            };
            let mut any_success = false;
            for (api_key, tier) in self.keyhub.discovery_keys(&provider_name) {
                if tier != KeyTier::Free {
                    continue;
                }
                match provider.list_models(&api_key).await {
                    Ok(models) => {
                        any_success = true;
                        self.keyhub.update_models(&provider_name, &api_key, models);
                    }
                    Err(error) => {
                        let status = error.http_status();
                        self.keyhub.record_model_error(
                            &provider_name,
                            &api_key,
                            &sanitize_diagnostic(&error.to_string()),
                        );
                        tracing::warn!(
                            request_id,
                            provider = %provider_name,
                            key = %mask_key(&api_key),
                            tier = %tier,
                            stage = "model_discovery",
                            http_status = status,
                            error_category = error.category(),
                            error = %sanitize_diagnostic(&error.to_string()),
                            "Free key model discovery failed"
                        );
                    }
                }
            }

            // Update discovery backoff state
            let mut backoff_map = self.discovery_backoff.write();
            if any_success {
                // Clear backoff on success
                backoff_map.remove(&provider_name);
            } else {
                // Record failure for backoff
                let state = backoff_map
                    .entry(provider_name.clone())
                    .or_insert(DiscoveryState {
                        last_attempt: 0,
                        fail_count: 0,
                    });
                state.last_attempt = now;
                state.fail_count += 1;
            }
        }
    }

    async fn candidates_with_refresh(
        &self,
        route: &ResolvedRoute,
        request_id: &str,
        agent_name: Option<&str>,
    ) -> Vec<SelectedCandidate> {
        let candidates = self.candidates(route, agent_name);
        if !candidates.is_empty() || !self.config.routing.auto_discover {
            return candidates;
        }
        self.refresh_free_models(request_id).await;
        self.candidates(route, agent_name)
    }

    /// Send a non-streaming chat completion request with automatic fallback.
    pub async fn chat(&self, request: &ChatCompletionRequest) -> GatewayResult<ChatResponse> {
        let request = self.compress_context_if_enabled(request).await;
        let route = self.resolve(&request.model, request.agent_name.as_deref())?;
        let request_id = request.request_id.as_deref().unwrap_or("unknown");
        let agent_name = request.agent_name.as_deref();
        let candidates = self
            .candidates_with_refresh(&route, request_id, agent_name)
            .await;
        if candidates.is_empty() {
            let available: Vec<String> = self.providers.iter().map(|p| p.key().clone()).collect();
            let model_summary = self.keyhub.free_model_summary();
            let openrouter_key_status = self.keyhub.provider_key_status_summary("openrouter");
            let openrouter_native = self.is_openrouter_native_route(&route);
            tracing::warn!(
                request_id,
                model = %route.model,
                provider = %route.provider_name,
                providers_checked = %available.join(", "),
                free_model_counts = %model_summary,
                openrouter_native,
                openrouter_key_status = %openrouter_key_status,
                "No free keys found for model 鈥?model may not exist in any provider's inventory"
            );
            if let Some(ref meta) = self.model_meta {
                meta.learn_from_failure(&route.provider_name, &route.model, "model_not_found", 404);
            }
            if openrouter_native {
                return Err(GatewayError::NoAvailableKeys(route.provider_name));
            }
            return Err(GatewayError::ModelNotFound(route.model));
        }

        // Vision detection
        let has_vision = crate::models::request_has_vision(&request.messages);
        if has_vision {
            tracing::info!(
                request_id,
                model = %route.model,
                "Request contains image content (vision)"
            );
            if !model_supports_vision(&route.model) {
                tracing::warn!(
                    request_id,
                    model = %route.model,
                    "Request has images but resolved model may not support vision; routing to first candidate"
                );
            }
        }

        // Track which providers had model-specific candidates
        let providers_with_candidates: std::collections::HashSet<String> = candidates
            .iter()
            .map(|candidate| candidate.provider.clone())
            .collect();
        let mut last_error: Option<GatewayError> = None;
        let attempt_count = candidates.len();
        let mut last_provider: Option<String> = None;

        for (attempt_index, candidate) in candidates.into_iter().enumerate() {
            let provider_name = candidate.provider;
            let api_key = candidate.key;
            let upstream_model = candidate.model;
            let provider = match self.providers.get(&provider_name) {
                Some(p) => p,
                None => {
                    tracing::debug!(provider = %provider_name, "Provider not registered, skipping");
                    continue;
                }
            };
            if !self.keyhub.reserve_key(&provider_name, &api_key) {
                tracing::debug!(
                    request_id,
                    provider = %provider_name,
                    key = %mask_key(&api_key),
                    "Skipping candidate: key could not be reserved"
                );
                continue;
            }

            let mut req = request.clone();
            // Inject context handoff if this is a fallback attempt
            if attempt_index > 0
                && let Some(ref prev) = last_provider
            {
                req = inject_context_handoff(req, prev, &provider_name, &upstream_model);
            }
            req.model = upstream_model.clone();
            req.stream = Some(false);
            let started = Instant::now();
            tracing::info!(
                request_id,
                provider = %provider_name,
                model = %req.model,
                key = %mask_key(&api_key),
                attempt = attempt_index + 1,
                stream = false,
                stage = "upstream_connect",
                "Starting provider attempt"
            );

            let prompt_estimate = crate::models::estimate_message_tokens(&req.messages);
            let result: GatewayResult<ChatResponse> = provider.chat(&api_key, req).await;
            match result {
                Ok(mut response) => {
                    crate::models::repair_tool_call_arguments(&mut response.body);
                    if !crate::models::response_has_useful_output(&response.body) {
                        let e = GatewayError::UpstreamError(
                            "empty chat completion response".to_string(),
                        );
                        self.keyhub
                            .report_gateway_error(&provider_name, &api_key, &e);
                        if let Some(ref meta) = self.model_meta {
                            meta.learn_from_failure(
                                &provider_name,
                                &upstream_model,
                                &e.to_string(),
                                e.http_status(),
                            );
                        }
                        record_request_attempt(
                            &self.model_meta,
                            request_id,
                            (attempt_index + 1) as i64,
                            &provider_name,
                            &upstream_model,
                            &api_key,
                            false,
                            e.category(),
                            Some(response.status),
                            Some(&e.to_string()),
                            Some(self.config.routing.cooldown_seconds as i64),
                            attempt_index + 1 < attempt_count,
                        );
                        last_provider = Some(provider_name.clone());
                        tracing::warn!(
                            request_id,
                            provider = %provider_name,
                            model = %upstream_model,
                            key = %mask_key(&api_key),
                            attempt = attempt_index + 1,
                            stream = false,
                            stage = "upstream_response",
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            http_status = response.status,
                            fallback = attempt_index + 1 < attempt_count,
                            "Provider returned empty completion, trying next fallback"
                        );
                        last_error = Some(e);
                        continue;
                    }
                    let (prompt_tokens, completion_tokens, tokens_reported) =
                        crate::models::extract_usage_or_estimate(&response.body, prompt_estimate);
                    self.keyhub.report_reserved_success(
                        &provider_name,
                        &api_key,
                        prompt_tokens,
                        completion_tokens,
                    );
                    record_model_usage(
                        &self.model_meta,
                        &provider_name,
                        &upstream_model,
                        true,
                        prompt_tokens,
                        completion_tokens,
                        tokens_reported,
                    );
                    record_request_attempt(
                        &self.model_meta,
                        request_id,
                        (attempt_index + 1) as i64,
                        &provider_name,
                        &upstream_model,
                        &api_key,
                        true,
                        "success",
                        Some(response.status),
                        None,
                        None,
                        false,
                    );
                    tracing::info!(
                        request_id,
                        provider = %provider_name,
                        model = %upstream_model,
                        key = %mask_key(&api_key),
                        attempt = attempt_index + 1,
                        stream = false,
                        stage = "upstream_response",
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        http_status = response.status,
                        "Provider attempt succeeded"
                    );
                    return Ok(response);
                }
                Err(e) => {
                    let status_code = e.http_status();
                    self.keyhub
                        .report_gateway_error(&provider_name, &api_key, &e);
                    // Record failure reason for metadata learning
                    if let Some(ref meta) = self.model_meta {
                        meta.learn_from_failure(
                            &provider_name,
                            &upstream_model,
                            &e.to_string(),
                            status_code,
                        );
                    }
                    record_request_attempt(
                        &self.model_meta,
                        request_id,
                        (attempt_index + 1) as i64,
                        &provider_name,
                        &upstream_model,
                        &api_key,
                        false,
                        e.category(),
                        Some(status_code),
                        Some(&e.to_string()),
                        Some(self.config.routing.cooldown_seconds as i64),
                        attempt_index + 1 < attempt_count,
                    );
                    last_provider = Some(provider_name.clone());
                    tracing::warn!(
                        request_id,
                        provider = %provider_name,
                        model = %upstream_model,
                        key = %mask_key(&api_key),
                        attempt = attempt_index + 1,
                        stream = false,
                        stage = "upstream_response",
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        http_status = status_code,
                        error_category = e.category(),
                        error = %sanitize_diagnostic(&e.to_string()),
                        fallback = attempt_index + 1 < attempt_count,
                        "Provider request failed, trying next fallback"
                    );
                    last_error = Some(e);
                    continue;
                }
            }
        }

        let error = last_error.unwrap_or(GatewayError::AllProvidersFailed);

        // 鈹€鈹€鈹€ Emergency cross-provider fallback 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€
        // When all model-specific candidates fail with server-side errors
        // (transport unreachable, rate-limited, timeout, 5xx), try fallback
        // providers that had NO model-specific candidates in the main list.
        // They might accept the model even if not in their advertised inventory.
        let is_server_error = matches!(
            &error,
            GatewayError::Reqwest(_)
                | GatewayError::HttpError { status: 429, .. }
                | GatewayError::RateLimited
                | GatewayError::Timeout(_)
        );

        if is_server_error {
            // 鈹€鈹€ Round 1: Cross-provider fallback 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€
            // Try providers that had NO free candidates in the main list.
            // They might accept the model even if not in their advertised inventory.
            for provider in self.provider_order(&route.provider_name) {
                if providers_with_candidates.contains(&provider) {
                    continue;
                }

                let Some(emergency_key) = self.keyhub.any_free_key(&provider) else {
                    continue;
                };
                let Some(emergency_provider) = self.providers.get(&provider) else {
                    continue;
                };
                if !self.keyhub.reserve_key(&provider, &emergency_key) {
                    continue;
                }

                let mut req = request.clone();
                req.model = route.model.clone();
                req.stream = Some(false);
                let prompt_estimate = crate::models::estimate_message_tokens(&req.messages);

                tracing::info!(
                    request_id,
                    provider = %provider,
                    model = %req.model,
                    stage = "emergency_fallback",
                    "Attempting emergency cross-provider fallback"
                );

                match emergency_provider.chat(&emergency_key, req).await {
                    Ok(mut response) => {
                        crate::models::repair_tool_call_arguments(&mut response.body);
                        if !crate::models::response_has_useful_output(&response.body) {
                            let e = GatewayError::UpstreamError(
                                "empty chat completion response".to_string(),
                            );
                            self.keyhub
                                .report_gateway_error(&provider, &emergency_key, &e);
                            if let Some(ref meta) = self.model_meta {
                                meta.learn_from_failure(
                                    &provider,
                                    &route.model,
                                    &e.to_string(),
                                    e.http_status(),
                                );
                            }
                            tracing::warn!(
                                request_id,
                                provider = %provider,
                                model = %route.model,
                                stage = "emergency_fallback_empty",
                                "Emergency fallback returned empty completion"
                            );
                            continue;
                        }
                        let (prompt_tokens, completion_tokens, tokens_reported) =
                            crate::models::extract_usage_or_estimate(
                                &response.body,
                                prompt_estimate,
                            );
                        self.keyhub.report_reserved_success(
                            &provider,
                            &emergency_key,
                            prompt_tokens,
                            completion_tokens,
                        );
                        record_model_usage(
                            &self.model_meta,
                            &provider,
                            &route.model,
                            true,
                            prompt_tokens,
                            completion_tokens,
                            tokens_reported,
                        );
                        tracing::info!(
                            request_id,
                            provider = %provider,
                            model = %route.model,
                            stage = "emergency_fallback_success",
                            "Emergency cross-provider fallback succeeded"
                        );
                        return Ok(response);
                    }
                    Err(e) => {
                        tracing::warn!(
                            request_id,
                            provider = %provider,
                            model = %route.model,
                            error_category = e.category(),
                            error = %sanitize_diagnostic(&e.to_string()),
                            stage = "emergency_fallback_failed",
                            "Emergency cross-provider fallback failed"
                        );
                    }
                }
            }

            // 鈹€鈹€ Round 2: Paid key escalation 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€
            // When all free keys from a provider hit 429, try paid keys
            // from the same providers as the last resort.
            if matches!(
                &error,
                GatewayError::HttpError { status: 429, .. } | GatewayError::RateLimited
            ) {
                for provider in &providers_with_candidates {
                    let Some(paid_key) =
                        self.keyhub.paid_candidate_for_model(provider, &route.model)
                    else {
                        continue;
                    };
                    let Some(emergency_provider) = self.providers.get(provider) else {
                        continue;
                    };
                    if !self.keyhub.reserve_key(provider, &paid_key) {
                        continue;
                    }

                    let mut req = request.clone();
                    req.model = route.model.clone();
                    req.stream = Some(false);
                    let prompt_estimate = crate::models::estimate_message_tokens(&req.messages);

                    tracing::info!(
                        request_id,
                        provider = %provider,
                        model = %req.model,
                        stage = "paid_key_escalation",
                        "Attempting paid key escalation after free keys exhausted"
                    );

                    match emergency_provider.chat(&paid_key, req).await {
                        Ok(mut response) => {
                            crate::models::repair_tool_call_arguments(&mut response.body);
                            if !crate::models::response_has_useful_output(&response.body) {
                                let e = GatewayError::UpstreamError(
                                    "empty chat completion response".to_string(),
                                );
                                self.keyhub.report_gateway_error(provider, &paid_key, &e);
                                if let Some(ref meta) = self.model_meta {
                                    meta.learn_from_failure(
                                        provider,
                                        &route.model,
                                        &e.to_string(),
                                        e.http_status(),
                                    );
                                }
                                tracing::warn!(
                                    request_id,
                                    provider = %provider,
                                    model = %route.model,
                                    stage = "paid_key_escalation_empty",
                                    "Paid key escalation returned empty completion"
                                );
                                continue;
                            }
                            let (prompt_tokens, completion_tokens, tokens_reported) =
                                crate::models::extract_usage_or_estimate(
                                    &response.body,
                                    prompt_estimate,
                                );
                            self.keyhub.report_reserved_success(
                                provider,
                                &paid_key,
                                prompt_tokens,
                                completion_tokens,
                            );
                            record_model_usage(
                                &self.model_meta,
                                provider,
                                &route.model,
                                true,
                                prompt_tokens,
                                completion_tokens,
                                tokens_reported,
                            );
                            tracing::info!(
                                request_id,
                                provider = %provider,
                                model = %route.model,
                                stage = "paid_key_escalation_success",
                                "Paid key escalation succeeded"
                            );
                            return Ok(response);
                        }
                        Err(e) => {
                            tracing::warn!(
                                request_id,
                                provider = %provider,
                                model = %route.model,
                                error_category = e.category(),
                                error = %sanitize_diagnostic(&e.to_string()),
                                stage = "paid_key_escalation_failed",
                                "Paid key escalation also failed"
                            );
                        }
                    }
                }
            }
        }

        tracing::error!(
            request_id,
            stage = "upstream_response",
            error_category = error.category(),
            error = %sanitize_diagnostic(&error.to_string()),
            "All providers failed"
        );
        Err(error)
    }

    /// Route a generic OpenAI-compatible JSON endpoint through provider/key
    /// fallback. This is used for compatibility endpoints such as embeddings.
    pub async fn post_openai_json(
        &self,
        model: &str,
        endpoint: &str,
        mut body: serde_json::Value,
        agent_name: Option<&str>,
    ) -> GatewayResult<ChatResponse> {
        let route = self.resolve(model, agent_name)?;
        let request_id = format!("req-{}", uuid::Uuid::new_v4());
        let mut last_error: Option<GatewayError> = None;

        for provider_name in self.provider_order(&route.provider_name) {
            let Some(provider) = self.providers.get(&provider_name) else {
                continue;
            };
            let upstream_model = model_for_resolved_provider(&provider_name, &route.model);
            let mut keys = self
                .keyhub
                .free_candidates(&provider_name, &upstream_model, agent_name);
            if keys.is_empty()
                && provider_name.eq_ignore_ascii_case(&route.provider_name)
                && let Some(key) = self.keyhub.any_free_key(&provider_name)
            {
                keys.push(key);
            }

            for api_key in keys {
                if !self.keyhub.reserve_key(&provider_name, &api_key) {
                    continue;
                }

                body["model"] = serde_json::Value::String(upstream_model.clone());
                tracing::info!(
                    request_id = %request_id,
                    provider = %provider_name,
                    endpoint = %endpoint,
                    model = %upstream_model,
                    key = %mask_key(&api_key),
                    "Starting provider generic OpenAI request"
                );

                match provider.post_json(&api_key, endpoint, body.clone()).await {
                    Ok(response) => {
                        self.keyhub
                            .report_reserved_success(&provider_name, &api_key, None, None);
                        record_model_usage(
                            &self.model_meta,
                            &provider_name,
                            &upstream_model,
                            true,
                            None,
                            None,
                            false,
                        );
                        return Ok(response);
                    }
                    Err(error) => {
                        let status_code = error.http_status();
                        self.keyhub
                            .report_gateway_error(&provider_name, &api_key, &error);
                        if let Some(ref meta) = self.model_meta {
                            meta.learn_from_failure(
                                &provider_name,
                                &upstream_model,
                                &error.to_string(),
                                status_code,
                            );
                        }
                        tracing::warn!(
                            request_id = %request_id,
                            provider = %provider_name,
                            endpoint = %endpoint,
                            model = %upstream_model,
                            key = %mask_key(&api_key),
                            http_status = status_code,
                            error_category = error.category(),
                            error = %sanitize_diagnostic(&error.to_string()),
                            "Provider generic OpenAI request failed, trying fallback"
                        );
                        last_error = Some(error);
                    }
                }
            }
        }

        Err(last_error.unwrap_or(GatewayError::NoAvailableKeys(route.provider_name)))
    }

    /// Send a streaming chat completion request with automatic fallback.
    pub async fn chat_stream(
        &self,
        request: &ChatCompletionRequest,
    ) -> GatewayResult<StreamResponse> {
        let request = self.compress_context_if_enabled(request).await;
        let route = self.resolve(&request.model, request.agent_name.as_deref())?;
        let request_id = request.request_id.as_deref().unwrap_or("unknown");
        let agent_name = request.agent_name.as_deref();
        let candidates = self
            .candidates_with_refresh(&route, request_id, agent_name)
            .await;
        let openrouter_native = self.is_openrouter_native_route(&route);
        let openrouter_key_status = self.keyhub.provider_key_status_summary("openrouter");
        if candidates.is_empty() {
            let available: Vec<String> = self.providers.iter().map(|p| p.key().clone()).collect();
            let model_summary = self.keyhub.free_model_summary();
            tracing::warn!(
                request_id,
                model = %route.model,
                provider = %route.provider_name,
                providers_checked = %available.join(", "),
                free_model_counts = %model_summary,
                "No free keys found for model (stream) 鈥?model may not exist in any provider's inventory"
            );
            if let Some(ref meta) = self.model_meta {
                meta.learn_from_failure(&route.provider_name, &route.model, "model_not_found", 404);
            }
            if openrouter_native {
                tracing::warn!(
                    request_id,
                    model = %route.model,
                    provider = %route.provider_name,
                    openrouter_key_status = %openrouter_key_status,
                    "OpenRouter native model has no available provider key"
                );
                return Err(GatewayError::NoAvailableKeys(route.provider_name));
            }
            return Err(GatewayError::ModelNotFound(route.model));
        }

        // Vision detection
        let has_vision = crate::models::request_has_vision(&request.messages);
        if has_vision {
            tracing::info!(
                request_id,
                model = %route.model,
                "Streaming request contains image content (vision)"
            );
        }

        // Track which providers had model-specific candidates
        let providers_with_candidates: std::collections::HashSet<String> = candidates
            .iter()
            .map(|candidate| candidate.provider.clone())
            .collect();
        let mut last_error: Option<GatewayError> = None;
        let attempt_count = candidates.len();
        let mut last_provider: Option<String> = None;

        for (attempt_index, candidate) in candidates.into_iter().enumerate() {
            let provider_name = candidate.provider;
            let api_key = candidate.key;
            let upstream_model = candidate.model;
            let provider = match self.providers.get(&provider_name) {
                Some(p) => p,
                None => continue,
            };
            if !self.keyhub.reserve_key(&provider_name, &api_key) {
                tracing::debug!(
                    request_id,
                    provider = %provider_name,
                    key = %mask_key(&api_key),
                    "Skipping stream candidate: key could not be reserved"
                );
                continue;
            }

            let mut req = request.clone();
            // Inject context handoff if this is a fallback attempt
            if attempt_index > 0
                && let Some(ref prev) = last_provider
            {
                req = inject_context_handoff(req, prev, &provider_name, &upstream_model);
            }
            req.model = upstream_model.clone();
            let upstream_model = req.model.clone();
            let started = Instant::now();
            tracing::info!(
                request_id,
                provider = %provider_name,
                model = %upstream_model,
                key = %mask_key(&api_key),
                attempt = attempt_index + 1,
                stream = true,
                stage = "upstream_connect",
                "Starting provider stream attempt"
            );

            let prompt_estimate = crate::models::estimate_message_tokens(&req.messages);
            let result: GatewayResult<StreamResponse> = provider.chat_stream(&api_key, req).await;
            match result {
                Ok(response) => {
                    record_request_attempt(
                        &self.model_meta,
                        request_id,
                        (attempt_index + 1) as i64,
                        &provider_name,
                        &upstream_model,
                        &api_key,
                        true,
                        "success",
                        None,
                        None,
                        None,
                        false,
                    );
                    tracing::info!(
                        request_id,
                        provider = %provider_name,
                        model = %upstream_model,
                        key = %mask_key(&api_key),
                        attempt = attempt_index + 1,
                        stream = true,
                        stage = "upstream_response",
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "Provider stream established"
                    );
                    return Ok(account_stream(
                        response,
                        self.keyhub.clone(),
                        self.model_meta.clone(),
                        provider_name,
                        api_key,
                        request_id.to_string(),
                        upstream_model,
                        prompt_estimate,
                        (attempt_index + 1) as i64,
                    ));
                }
                Err(e) => {
                    let status_code = e.http_status();
                    self.keyhub
                        .report_gateway_error(&provider_name, &api_key, &e);
                    // Record failure reason for metadata learning
                    if let Some(ref meta) = self.model_meta {
                        meta.learn_from_failure(
                            &provider_name,
                            &upstream_model,
                            &e.to_string(),
                            status_code,
                        );
                    }
                    record_request_attempt(
                        &self.model_meta,
                        request_id,
                        (attempt_index + 1) as i64,
                        &provider_name,
                        &upstream_model,
                        &api_key,
                        false,
                        e.category(),
                        Some(status_code),
                        Some(&e.to_string()),
                        Some(self.config.routing.cooldown_seconds as i64),
                        attempt_index + 1 < attempt_count,
                    );
                    last_provider = Some(provider_name.clone());
                    tracing::warn!(
                        request_id,
                        provider = %provider_name,
                        model = %upstream_model,
                        key = %mask_key(&api_key),
                        attempt = attempt_index + 1,
                        stream = true,
                        stage = "upstream_response",
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        http_status = status_code,
                        error_category = e.category(),
                        error = %sanitize_diagnostic(&e.to_string()),
                        fallback = attempt_index + 1 < attempt_count,
                        "Provider stream request failed, trying next fallback"
                    );
                    last_error = Some(e);
                    continue;
                }
            }
        }

        let error = last_error.unwrap_or(GatewayError::AllProvidersFailed);

        // 鈹€鈹€鈹€ Emergency cross-provider fallback (stream) 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€
        let is_server_error = matches!(
            &error,
            GatewayError::Reqwest(_)
                | GatewayError::HttpError { status: 429, .. }
                | GatewayError::RateLimited
                | GatewayError::Timeout(_)
        );

        if is_server_error {
            // 鈹€鈹€ Round 1: Cross-provider fallback (stream) 鈹€鈹€鈹€鈹€鈹€
            for provider in self.provider_order(&route.provider_name) {
                if providers_with_candidates.contains(&provider) {
                    continue;
                }

                let Some(emergency_key) = self.keyhub.any_free_key(&provider) else {
                    continue;
                };
                let Some(emergency_provider) = self.providers.get(&provider) else {
                    continue;
                };
                if !self.keyhub.reserve_key(&provider, &emergency_key) {
                    continue;
                }

                let mut req = request.clone();
                req.model = route.model.clone();
                let upstream_model = req.model.clone();
                let prompt_estimate = crate::models::estimate_message_tokens(&req.messages);

                tracing::info!(
                    request_id,
                    provider = %provider,
                    model = %upstream_model,
                    stage = "emergency_fallback_stream",
                    "Attempting emergency cross-provider stream fallback"
                );

                match emergency_provider.chat_stream(&emergency_key, req).await {
                    Ok(response) => {
                        record_request_attempt(
                            &self.model_meta,
                            request_id,
                            (attempt_count + 1) as i64,
                            &provider,
                            &upstream_model,
                            &emergency_key,
                            true,
                            "success",
                            None,
                            None,
                            None,
                            false,
                        );
                        tracing::info!(
                            request_id,
                            provider = %provider,
                            model = %upstream_model,
                            stage = "emergency_fallback_stream_success",
                            "Emergency stream fallback succeeded"
                        );
                        return Ok(account_stream(
                            response,
                            self.keyhub.clone(),
                            self.model_meta.clone(),
                            provider,
                            emergency_key,
                            request_id.to_string(),
                            upstream_model,
                            prompt_estimate,
                            (attempt_count + 1) as i64,
                        ));
                    }
                    Err(e) => {
                        record_request_attempt(
                            &self.model_meta,
                            request_id,
                            (attempt_count + 1) as i64,
                            &provider,
                            &upstream_model,
                            &emergency_key,
                            false,
                            e.category(),
                            Some(e.http_status()),
                            Some(&e.to_string()),
                            Some(self.config.routing.cooldown_seconds as i64),
                            true,
                        );
                        tracing::warn!(
                            request_id,
                            provider = %provider,
                            model = %upstream_model,
                            error_category = e.category(),
                            error = %sanitize_diagnostic(&e.to_string()),
                            stage = "emergency_fallback_stream_failed",
                            "Emergency stream fallback failed"
                        );
                    }
                }
            }

            // 鈹€鈹€ Round 2: Paid key escalation (stream) 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€
            if matches!(
                &error,
                GatewayError::HttpError { status: 429, .. } | GatewayError::RateLimited
            ) {
                for provider in &providers_with_candidates {
                    let Some(paid_key) =
                        self.keyhub.paid_candidate_for_model(provider, &route.model)
                    else {
                        continue;
                    };
                    let Some(emergency_provider) = self.providers.get(provider) else {
                        continue;
                    };
                    if !self.keyhub.reserve_key(provider, &paid_key) {
                        continue;
                    }

                    let mut req = request.clone();
                    req.model = route.model.clone();
                    let upstream_model = req.model.clone();
                    let prompt_estimate = crate::models::estimate_message_tokens(&req.messages);

                    tracing::info!(
                        request_id,
                        provider = %provider,
                        model = %upstream_model,
                        stage = "paid_key_escalation_stream",
                        "Attempting paid key escalation (stream)"
                    );

                    match emergency_provider.chat_stream(&paid_key, req).await {
                        Ok(response) => {
                            record_request_attempt(
                                &self.model_meta,
                                request_id,
                                (attempt_count + 2) as i64,
                                provider,
                                &upstream_model,
                                &paid_key,
                                true,
                                "success",
                                None,
                                None,
                                None,
                                false,
                            );
                            tracing::info!(
                                request_id,
                                provider = %provider,
                                model = %upstream_model,
                                stage = "paid_key_escalation_stream_success",
                                "Paid key stream escalation succeeded"
                            );
                            return Ok(account_stream(
                                response,
                                self.keyhub.clone(),
                                self.model_meta.clone(),
                                provider.clone(),
                                paid_key,
                                request_id.to_string(),
                                upstream_model,
                                prompt_estimate,
                                (attempt_count + 2) as i64,
                            ));
                        }
                        Err(e) => {
                            record_request_attempt(
                                &self.model_meta,
                                request_id,
                                (attempt_count + 2) as i64,
                                provider,
                                &upstream_model,
                                &paid_key,
                                false,
                                e.category(),
                                Some(e.http_status()),
                                Some(&e.to_string()),
                                Some(self.config.routing.cooldown_seconds as i64),
                                true,
                            );
                            tracing::warn!(
                                request_id,
                                provider = %provider,
                                model = %upstream_model,
                                error_category = e.category(),
                                error = %sanitize_diagnostic(&e.to_string()),
                                stage = "paid_key_escalation_stream_failed",
                                "Paid key stream escalation also failed"
                            );
                        }
                    }
                }
            }
        }

        tracing::error!(
            request_id,
            stage = "upstream_response",
            error_category = error.category(),
            error = %sanitize_diagnostic(&error.to_string()),
            "All providers failed for stream"
        );
        Err(error)
    }
}

/// Try to extract token usage from the final SSE chunk of a streaming response.
/// OpenAI-compatible providers include `usage` in the last data chunk (the one
/// with `finish_reason`). Format: `data: {"usage":{"prompt_tokens":...,"completion_tokens":...}}\n\n`
fn extract_stream_usage(bytes: &Bytes) -> (Option<u32>, Option<u32>) {
    let result = (|| -> Option<(u32, u32)> {
        let text = std::str::from_utf8(bytes).ok()?;
        let json_str = text.strip_prefix("data: ")?.trim();
        if json_str == "[DONE]" {
            return None;
        }
        let value: serde_json::Value = serde_json::from_str(json_str).ok()?;
        let usage = value.get("usage")?;
        let prompt = usage.get("prompt_tokens")?.as_u64()? as u32;
        let completion = usage.get("completion_tokens")?.as_u64()? as u32;
        Some((prompt, completion))
    })();
    match result {
        Some((p, c)) => (Some(p), Some(c)),
        None => (None, None),
    }
}

#[derive(Debug, Default)]
struct StreamChunkInspection {
    estimated_tokens: u32,
    saw_text: bool,
    saw_tool_call: bool,
}

#[derive(Debug, Default)]
struct ToolCallFragment {
    name: Option<String>,
    arguments: String,
}

#[derive(Debug, Default)]
struct ToolCallStreamState {
    calls: HashMap<(usize, usize), ToolCallFragment>,
}

impl ToolCallStreamState {
    fn observe_call(
        &mut self,
        choice_index: usize,
        fallback_call_index: usize,
        call: &serde_json::Value,
    ) {
        let call_index = call
            .get("index")
            .and_then(|value| value.as_u64())
            .map(|value| value as usize)
            .unwrap_or(fallback_call_index);
        let fragment = self.calls.entry((choice_index, call_index)).or_default();
        if let Some(name) = call
            .get("function")
            .and_then(|function| function.get("name"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
        {
            fragment.name = Some(name.to_string());
        }
        if let Some(arguments) = call
            .get("function")
            .and_then(|function| function.get("arguments"))
        {
            fragment
                .arguments
                .push_str(&stream_tool_arguments_fragment(arguments));
        }
    }

    fn validation_error(&self) -> Option<String> {
        for fragment in self.calls.values() {
            if fragment.arguments.trim().is_empty() {
                continue;
            }
            if serde_json::from_str::<serde_json::Value>(&fragment.arguments).is_err() {
                return Some("incomplete streaming tool call arguments".to_string());
            }
        }
        None
    }
}

fn stream_tool_arguments_fragment(arguments: &serde_json::Value) -> String {
    if let Some(value) = arguments.as_str() {
        value.to_string()
    } else if arguments.is_null() {
        "{}".to_string()
    } else {
        arguments.to_string()
    }
}

fn inspect_stream_chunk(
    bytes: &Bytes,
    tool_state: &mut ToolCallStreamState,
) -> StreamChunkInspection {
    let Some(text) = std::str::from_utf8(bytes).ok() else {
        return StreamChunkInspection::default();
    };
    let mut inspection = StreamChunkInspection::default();
    for line in text.lines() {
        let Some(json_str) = line.strip_prefix("data: ") else {
            continue;
        };
        let json_str = json_str.trim();
        if json_str.is_empty() || json_str == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str) else {
            inspection.saw_text = true;
            inspection.estimated_tokens += crate::models::estimate_text_tokens(json_str);
            continue;
        };
        let Some(choices) = value.get("choices").and_then(|value| value.as_array()) else {
            continue;
        };
        for (choice_index, choice) in choices.iter().enumerate() {
            let Some(delta) = choice.get("delta") else {
                continue;
            };
            if let Some(content) = delta.get("content") {
                let text = crate::models::content_to_text(content);
                if !text.trim().is_empty() {
                    inspection.saw_text = true;
                    inspection.estimated_tokens += crate::models::estimate_text_tokens(&text);
                }
            }
            if let Some(tool_calls) = delta.get("tool_calls").and_then(|value| value.as_array()) {
                inspection.saw_tool_call = true;
                let tool_calls_text =
                    serde_json::to_string(tool_calls).unwrap_or_else(|_| "[]".to_string());
                inspection.estimated_tokens +=
                    crate::models::estimate_text_tokens(&tool_calls_text);
                for (call_index, call) in tool_calls.iter().enumerate() {
                    tool_state.observe_call(choice_index, call_index, call);
                }
            } else if delta.get("tool_calls").is_some() {
                inspection.saw_tool_call = true;
                inspection.estimated_tokens += crate::models::estimate_text_tokens(
                    &delta.get("tool_calls").unwrap().to_string(),
                );
            }
        }
    }
    inspection
}

fn repair_stream_tool_call_chunk(bytes: Bytes) -> Bytes {
    let Some(text) = std::str::from_utf8(&bytes).ok() else {
        return bytes;
    };
    let mut changed = false;
    let mut output = String::with_capacity(text.len());
    for line in text.lines() {
        let Some(json_str) = line.strip_prefix("data: ") else {
            output.push_str(line);
            output.push('\n');
            continue;
        };
        let trimmed = json_str.trim();
        if trimmed.is_empty() || trimmed == "[DONE]" {
            output.push_str(line);
            output.push('\n');
            continue;
        }
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            output.push_str(line);
            output.push('\n');
            continue;
        };
        let Some(choices) = value
            .get_mut("choices")
            .and_then(|value| value.as_array_mut())
        else {
            output.push_str(line);
            output.push('\n');
            continue;
        };
        let mut line_changed = false;
        for choice in choices {
            let Some(tool_calls) = choice
                .get_mut("delta")
                .and_then(|delta| delta.get_mut("tool_calls"))
                .and_then(|value| value.as_array_mut())
            else {
                continue;
            };
            for call in tool_calls {
                let Some(arguments) = call
                    .get_mut("function")
                    .and_then(|function| function.get_mut("arguments"))
                else {
                    continue;
                };
                if !arguments.is_string() {
                    let repaired = stream_tool_arguments_fragment(arguments);
                    *arguments = serde_json::Value::String(repaired);
                    changed = true;
                    line_changed = true;
                }
            }
        }
        if line_changed {
            output.push_str("data: ");
            output.push_str(&value.to_string());
            output.push('\n');
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    if changed { Bytes::from(output) } else { bytes }
}

#[allow(clippy::too_many_arguments)]
fn account_stream(
    stream: StreamResponse,
    keyhub: Arc<KeyHub>,
    model_meta: Option<ModelMetaStore>,
    provider_name: String,
    api_key: String,
    request_id: String,
    model: String,
    prompt_estimate: u32,
    attempt_index: i64,
) -> StreamResponse {
    struct State {
        stream: StreamResponse,
        keyhub: Arc<KeyHub>,
        model_meta: Option<ModelMetaStore>,
        provider_name: String,
        api_key: String,
        request_id: String,
        model: String,
        prompt_estimate: u32,
        attempt_index: i64,
        completion_estimate: u32,
        saw_output: bool,
        tool_call_state: ToolCallStreamState,
        terminal: bool,
        started: Instant,
        /// Last SSE chunk buffered so we can extract `usage` when the stream ends.
        last_chunk: Option<Bytes>,
    }

    let state = State {
        stream,
        keyhub,
        model_meta,
        provider_name,
        api_key,
        request_id,
        model,
        prompt_estimate,
        attempt_index,
        completion_estimate: 0,
        saw_output: false,
        tool_call_state: ToolCallStreamState::default(),
        terminal: false,
        started: Instant::now(),
        last_chunk: None,
    };

    Box::pin(futures::stream::unfold(state, |mut state| async move {
        if state.terminal {
            return None;
        }

        match state.stream.next().await {
            Some(Ok(bytes)) => {
                let inspection = inspect_stream_chunk(&bytes, &mut state.tool_call_state);
                state.completion_estimate += inspection.estimated_tokens;
                state.saw_output |= inspection.saw_text || inspection.saw_tool_call;
                let repaired = repair_stream_tool_call_chunk(bytes);
                state.last_chunk = Some(repaired.clone());
                Some((Ok(repaired), state))
            }
            Some(Err(error)) => {
                state.keyhub.force_transient_cooldown(
                    &state.provider_name,
                    &state.api_key,
                    error.http_status(),
                    error.category(),
                );
                if let Some(meta) = state.model_meta.as_ref() {
                    meta.learn_from_failure(
                        &state.provider_name,
                        &state.model,
                        &error.to_string(),
                        error.http_status(),
                    );
                }
                record_request_attempt(
                    &state.model_meta,
                    &state.request_id,
                    state.attempt_index,
                    &state.provider_name,
                    &state.model,
                    &state.api_key,
                    false,
                    error.category(),
                    Some(error.http_status()),
                    Some(&error.to_string()),
                    None,
                    false,
                );
                tracing::error!(
                    request_id = %state.request_id,
                    provider = %state.provider_name,
                    model = %state.model,
                    key = %mask_key(&state.api_key),
                    stream = true,
                    stage = "stream_body",
                    elapsed_ms = state.started.elapsed().as_millis() as u64,
                    error_category = error.category(),
                    error = %sanitize_diagnostic(&error.to_string()),
                    "Provider stream body failed"
                );
                state.terminal = true;
                Some((Err(error), state))
            }
            None => {
                // Extract token usage from the last SSE chunk (if present).
                // Providers like OpenAI/OpenRouter include usage in the final
                // chunk with `finish_reason: "stop"`.
                let (mut pt, mut ct): (Option<u32>, Option<u32>) = state
                    .last_chunk
                    .as_ref()
                    .map(extract_stream_usage)
                    .unwrap_or((None, None));
                let tokens_reported = pt.is_some() || ct.is_some();
                if !tokens_reported {
                    pt = Some(state.prompt_estimate);
                    ct = Some(state.completion_estimate);
                }
                if !state.saw_output {
                    let error = GatewayError::UpstreamError(
                        "empty streaming chat completion response".to_string(),
                    );
                    state.keyhub.force_transient_cooldown(
                        &state.provider_name,
                        &state.api_key,
                        error.http_status(),
                        error.category(),
                    );
                    if let Some(meta) = state.model_meta.as_ref() {
                        meta.learn_from_failure(
                            &state.provider_name,
                            &state.model,
                            &error.to_string(),
                            error.http_status(),
                        );
                    }
                    record_request_attempt(
                        &state.model_meta,
                        &state.request_id,
                        state.attempt_index,
                        &state.provider_name,
                        &state.model,
                        &state.api_key,
                        false,
                        error.category(),
                        Some(error.http_status()),
                        Some(&error.to_string()),
                        None,
                        false,
                    );
                    tracing::warn!(
                        request_id = %state.request_id,
                        provider = %state.provider_name,
                        model = %state.model,
                        key = %mask_key(&state.api_key),
                        stream = true,
                        stage = "stream_body",
                        elapsed_ms = state.started.elapsed().as_millis() as u64,
                        "Provider stream completed with empty output"
                    );
                    return None;
                }
                if let Some(message) = state.tool_call_state.validation_error() {
                    let error = GatewayError::UpstreamError(message);
                    state.keyhub.force_transient_cooldown(
                        &state.provider_name,
                        &state.api_key,
                        error.http_status(),
                        error.category(),
                    );
                    if let Some(meta) = state.model_meta.as_ref() {
                        meta.learn_from_failure(
                            &state.provider_name,
                            &state.model,
                            &error.to_string(),
                            error.http_status(),
                        );
                    }
                    record_request_attempt(
                        &state.model_meta,
                        &state.request_id,
                        state.attempt_index,
                        &state.provider_name,
                        &state.model,
                        &state.api_key,
                        false,
                        error.category(),
                        Some(error.http_status()),
                        Some(&error.to_string()),
                        None,
                        false,
                    );
                    tracing::warn!(
                        request_id = %state.request_id,
                        provider = %state.provider_name,
                        model = %state.model,
                        key = %mask_key(&state.api_key),
                        stream = true,
                        stage = "stream_body",
                        elapsed_ms = state.started.elapsed().as_millis() as u64,
                        error_category = error.category(),
                        error = %sanitize_diagnostic(&error.to_string()),
                        "Provider stream completed with invalid tool call arguments"
                    );
                    state.terminal = true;
                    return Some((Err(error), state));
                }
                if tokens_reported {
                    tracing::debug!(
                        request_id = %state.request_id,
                        provider = %state.provider_name,
                        model = %state.model,
                        prompt_tokens = ?pt,
                        completion_tokens = ?ct,
                        "Stream token usage extracted from final chunk"
                    );
                }
                state
                    .keyhub
                    .report_reserved_success(&state.provider_name, &state.api_key, pt, ct);
                record_model_usage(
                    &state.model_meta,
                    &state.provider_name,
                    &state.model,
                    true,
                    pt,
                    ct,
                    tokens_reported,
                );
                tracing::info!(
                    request_id = %state.request_id,
                    provider = %state.provider_name,
                    model = %state.model,
                    key = %mask_key(&state.api_key),
                    stream = true,
                    stage = "stream_body",
                    elapsed_ms = state.started.elapsed().as_millis() as u64,
                    "Provider stream completed"
                );
                None
            }
        }
    }))
}

fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        return "****".into();
    }
    format!("{}...{}", &key[..4], &key[key.len() - 4..])
}

fn deployment_in_active_cooldown(state: Option<&DeploymentStateRow>, now: i64) -> bool {
    state
        .and_then(|state| state.cooldown_until)
        .is_some_and(|cooldown_until| cooldown_until > now)
}

fn deployment_penalty(state: Option<&DeploymentStateRow>) -> u32 {
    let Some(state) = state else {
        return 0;
    };
    let consecutive = state.consecutive_failures.max(0) as u32;
    let errors = state.error_count.max(0) as u32;
    let successes = state.success_count.max(0) as u32;
    consecutive
        .saturating_mul(100)
        .saturating_add(errors.saturating_sub(successes).min(100))
}

fn lowest_penalty_prefix_len(candidates: &[RouteCandidate]) -> usize {
    let Some(first) = candidates.first() else {
        return 0;
    };
    candidates
        .iter()
        .take_while(|candidate| candidate.deployment_penalty == first.deployment_penalty)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentConfig, ModelAlias, ProviderConfig, ProviderType};
    use parking_lot::RwLock;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    fn make_router(
        config: Arc<Config>,
        providers: Arc<DashMap<String, BoxedProvider>>,
        keyhub: Arc<KeyHub>,
    ) -> Router {
        let disabled_models = Arc::new(RwLock::new(HashMap::<String, HashSet<String>>::new()));
        Router::new(config, providers, keyhub, disabled_models, None)
    }

    fn sse(json: serde_json::Value) -> Bytes {
        Bytes::from(format!("data: {}\n\n", json))
    }

    #[test]
    fn stream_tool_call_state_accepts_complete_fragmented_arguments() {
        let mut state = ToolCallStreamState::default();
        let chunks = [
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
        ];

        for chunk in &chunks {
            let inspection = inspect_stream_chunk(chunk, &mut state);
            assert!(inspection.saw_tool_call);
        }

        assert!(state.validation_error().is_none());
    }

    #[test]
    fn stream_tool_call_state_rejects_incomplete_fragmented_arguments() {
        let mut state = ToolCallStreamState::default();
        let chunks = [
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
        ];

        for chunk in &chunks {
            inspect_stream_chunk(chunk, &mut state);
        }

        assert_eq!(
            state.validation_error().as_deref(),
            Some("incomplete streaming tool call arguments")
        );
    }

    #[test]
    fn deployment_state_helpers_skip_cooldown_and_penalize_failures() {
        let now = 1_700_000_000;
        let cooled = DeploymentStateRow {
            provider: "openrouter".to_string(),
            model_id: "model-a".to_string(),
            key_id: "key-a".to_string(),
            success_count: 0,
            error_count: 3,
            consecutive_failures: 2,
            last_success_at: None,
            last_error_at: Some(now - 10),
            last_error_category: Some("rate_limited".to_string()),
            last_http_status: Some(429),
            cooldown_until: Some(now + 60),
            updated_at: now,
        };
        let recovered = DeploymentStateRow {
            provider: "openrouter".to_string(),
            model_id: "model-a".to_string(),
            key_id: "key-b".to_string(),
            success_count: 3,
            error_count: 1,
            consecutive_failures: 0,
            last_success_at: Some(now),
            last_error_at: Some(now - 100),
            last_error_category: Some("upstream_error".to_string()),
            last_http_status: Some(500),
            cooldown_until: Some(now - 1),
            updated_at: now,
        };

        assert!(deployment_in_active_cooldown(Some(&cooled), now));
        assert!(!deployment_in_active_cooldown(Some(&recovered), now));
        assert_eq!(deployment_penalty(Some(&cooled)), 203);
        assert_eq!(deployment_penalty(Some(&recovered)), 0);
    }

    #[test]
    fn lowest_penalty_prefix_excludes_degraded_candidates() {
        let mut candidates = vec![
            RouteCandidate {
                provider: "a".to_string(),
                key: "key-a".to_string(),
                model: "model".to_string(),
                provider_index: 0,
                usage_sort_key: (0, 0, 0, 0, 0),
                fail_count: 0,
                total_fail_count: 0,
                deployment_penalty: 0,
            },
            RouteCandidate {
                provider: "b".to_string(),
                key: "key-b".to_string(),
                model: "model".to_string(),
                provider_index: 1,
                usage_sort_key: (0, 0, 0, 0, 0),
                fail_count: 0,
                total_fail_count: 0,
                deployment_penalty: 0,
            },
            RouteCandidate {
                provider: "c".to_string(),
                key: "key-c".to_string(),
                model: "model".to_string(),
                provider_index: 2,
                usage_sort_key: (0, 0, 0, 0, 0),
                fail_count: 0,
                total_fail_count: 0,
                deployment_penalty: 100,
            },
        ];
        candidates.sort_by_key(|candidate| candidate.deployment_penalty);

        assert_eq!(lowest_penalty_prefix_len(&candidates), 2);
    }

    fn test_config() -> Config {
        Config {
            server: crate::config::ServerConfig {
                host: "127.0.0.1".into(),
                port: 9000,
                log_level: "info".into(),
                request_timeout: 30,
                sse_keepalive: 15,
            },
            routing: crate::config::RoutingConfig {
                strategy: crate::config::RoutingStrategy::LeastFailed,
                fail_threshold: 3,
                cooldown_seconds: 60,
                auto_discover: true,
            },
            fallback: vec!["github".into(), "nvidia".into(), "ollama".into()],
            agents: {
                let mut m = HashMap::new();
                m.insert(
                    "hermes".into(),
                    AgentConfig {
                        default_model: "coding".into(),
                    },
                );
                m
            },
            models: {
                let mut m = HashMap::new();
                m.insert(
                    "coding".into(),
                    ModelAlias {
                        provider: "github".into(),
                        model: "openai/gpt-4.1-mini".into(),
                    },
                );
                m.insert(
                    "chat".into(),
                    ModelAlias {
                        provider: "nvidia".into(),
                        model: "meta/llama-3.1-70b-instruct".into(),
                    },
                );
                m
            },
            providers: {
                let mut m = HashMap::new();
                m.insert(
                    "github".into(),
                    ProviderConfig {
                        provider_type: ProviderType::GithubModels,
                        enabled: true,
                        base_url: "https://models.inference.ai.azure.com".into(),
                        proxy_url: None,
                        keys: vec!["test-key".into()],
                        health_check_model: "openai/gpt-4.1-mini".into(),
                        timeout_seconds: 30,
                        priority: 0,
                    },
                );
                m.insert(
                    "nvidia".into(),
                    ProviderConfig {
                        provider_type: ProviderType::Nvidia,
                        enabled: true,
                        base_url: "https://integrate.api.nvidia.com/v1".into(),
                        proxy_url: None,
                        keys: vec!["test-key".into()],
                        health_check_model: "meta/llama-3.1-70b-instruct".into(),
                        timeout_seconds: 30,
                        priority: 0,
                    },
                );
                m
            },
            watcher: Default::default(),
            state: Default::default(),
            cors: Default::default(),
            adaptive_routing: Default::default(),
            context_compression: Default::default(),
        }
    }

    #[test]
    fn test_resolve_alias() {
        let config = Arc::new(test_config());
        let providers = Arc::new(DashMap::new());
        let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
        let router = make_router(config, providers, keyhub);

        let route = router.resolve("coding", None).unwrap();
        assert_eq!(route.provider_name, "github");
        assert_eq!(route.model, "openai/gpt-4.1-mini");
    }

    #[test]
    fn test_resolve_chat_alias() {
        let config = Arc::new(test_config());
        let providers = Arc::new(DashMap::new());
        let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
        let router = make_router(config, providers, keyhub);

        let route = router.resolve("chat", None).unwrap();
        assert_eq!(route.provider_name, "nvidia");
        assert_eq!(route.model, "meta/llama-3.1-70b-instruct");
    }

    #[test]
    fn test_resolve_agent_default() {
        let config = Arc::new(test_config());
        let providers = Arc::new(DashMap::new());
        let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
        let router = make_router(config, providers, keyhub);

        let route = router.resolve("coding", Some("hermes")).unwrap();
        assert_eq!(route.provider_name, "github");
        assert_eq!(route.model, "openai/gpt-4.1-mini");
    }

    #[test]
    fn test_resolve_with_registered_provider() {
        let config = Arc::new(test_config());
        let providers = Arc::new(DashMap::new());
        let keyhub = Arc::new(KeyHub::new(config.routing.clone()));

        // Register github so it's discoverable
        let github_config = config.providers.get("github").unwrap().clone();
        let provider = crate::providers::create_provider("github", &github_config).unwrap();
        providers.insert("github".into(), provider);

        let router = make_router(config, providers, keyhub);

        // Now an unknown model should fall back to the first registered provider
        let route = router.resolve("some-unknown-model", None).unwrap();
        assert_eq!(route.provider_name, "github");
    }

    #[test]
    fn test_resolve_unknown_model_no_providers() {
        let config = Arc::new(test_config());
        let providers = Arc::new(DashMap::new());
        let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
        let router = make_router(config, providers, keyhub);

        let result = router.resolve("nonexistent-model", None);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_provider_chain() {
        let config = Arc::new(test_config());
        let providers = Arc::new(DashMap::new());
        let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
        let router = make_router(config, providers, keyhub);

        let chain = router.build_provider_chain("github");
        assert_eq!(chain, vec!["github", "nvidia", "ollama"]);

        let chain = router.build_provider_chain("nvidia");
        assert_eq!(chain, vec!["nvidia", "github", "ollama"]);
    }
}
