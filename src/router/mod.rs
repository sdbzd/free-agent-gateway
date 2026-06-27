/// Router: Routes requests to the appropriate provider and model.
///
/// Handles:
/// - Model alias resolution
/// - Agent-aware routing
/// - Provider fallback chain
/// - Routing strategies (round-robin, random, least-failed, priority)
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use futures::StreamExt;
use parking_lot::RwLock;

use bytes::Bytes;

use crate::config::{Config, KeyTier};
use crate::error::{GatewayError, GatewayResult, sanitize_diagnostic};
use crate::keyhub::KeyHub;
use crate::metadata::ModelMetaStore;
use crate::models::{ChatCompletionRequest, ChatMessage};
use crate::providers::BoxedProvider;
use crate::providers::traits::{ChatResponse, StreamResponse};

/// A resolved route: which provider and model to use.
#[derive(Debug, Clone)]
pub struct ResolvedRoute {
    pub provider_name: String,
    pub model: String,
}

// ─── Context Handoff ─────────────────────────────────────────────────
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
                // Also strip suffix from the model_name part
                return Ok(ResolvedRoute {
                    provider_name: provider.to_string(),
                    model: strip_or_suffixes(model_name),
                });
            }
        }

        // 4. Try each fallback provider to see if they can serve this model
        for provider_name in &self.config.fallback {
            if self.providers.contains_key(provider_name) {
                return Ok(ResolvedRoute {
                    provider_name: provider_name.clone(),
                    model: strip_or_suffixes(model),
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

    fn candidates(&self, route: &ResolvedRoute, agent_name: Option<&str>) -> Vec<(String, String)> {
        let disabled = self.disabled_models.read();
        let model_disabled = |provider: &str| -> bool {
            disabled
                .get(provider)
                .map(|set| set.contains(&route.model))
                .unwrap_or(false)
        };

        let mut candidates = Vec::new();
        for provider in self.provider_order(&route.provider_name) {
            if model_disabled(&provider) {
                tracing::debug!(
                    provider = %provider,
                    model = %route.model,
                    "Skipping provider: model is disabled"
                );
                continue;
            }
            candidates.extend(
                self.keyhub
                    .free_candidates(&provider, &route.model, agent_name)
                    .into_iter()
                    .map(|key| (provider.clone(), key)),
            );
        }
        candidates
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
                        if matches!(status, 401 | 403 | 429) {
                            self.keyhub.report_failure(&provider_name, &api_key, status);
                        }
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
    ) -> Vec<(String, String)> {
        let candidates = self.candidates(route, agent_name);
        if !candidates.is_empty() || !self.config.routing.auto_discover {
            return candidates;
        }
        self.refresh_free_models(request_id).await;
        self.candidates(route, agent_name)
    }

    /// Send a non-streaming chat completion request with automatic fallback.
    pub async fn chat(&self, request: &ChatCompletionRequest) -> GatewayResult<ChatResponse> {
        let route = self.resolve(&request.model, request.agent_name.as_deref())?;
        let request_id = request.request_id.as_deref().unwrap_or("unknown");
        let agent_name = request.agent_name.as_deref();
        let candidates = self.candidates_with_refresh(&route, request_id, agent_name).await;
        if candidates.is_empty() {
            let available: Vec<String> = self.providers.iter().map(|p| p.key().clone()).collect();
            let model_summary = self.keyhub.free_model_summary();
            tracing::warn!(
                request_id,
                model = %route.model,
                provider = %route.provider_name,
                providers_checked = %available.join(", "),
                free_model_counts = %model_summary,
                "No free keys found for model — model may not exist in any provider's inventory"
            );
            if let Some(ref meta) = self.model_meta {
                meta.learn_from_failure(&route.provider_name, &route.model, "model_not_found", 404);
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
        let providers_with_candidates: std::collections::HashSet<String> =
            candidates.iter().map(|(p, _)| p.clone()).collect();
        let mut last_error: Option<GatewayError> = None;
        let attempt_count = candidates.len();
        let mut last_provider: Option<String> = None;

        for (attempt_index, (provider_name, api_key)) in candidates.into_iter().enumerate() {
            let provider = match self.providers.get(&provider_name) {
                Some(p) => p,
                None => {
                    tracing::debug!(provider = %provider_name, "Provider not registered, skipping");
                    continue;
                }
            };

            let mut req = request.clone();
            // Inject context handoff if this is a fallback attempt
            if attempt_index > 0 {
                if let Some(ref prev) = last_provider {
                    req = inject_context_handoff(
                        req,
                        prev,
                        &provider_name,
                        &route.model,
                    );
                }
            }
            req.model = route.model.clone();
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

            let result: GatewayResult<ChatResponse> = provider.chat(&api_key, req).await;
            match result {
                Ok(response) => {
                    let (prompt_tokens, completion_tokens) =
                        crate::models::extract_usage(&response.body);
                    self.keyhub
                        .report_success(&provider_name, &api_key, prompt_tokens, completion_tokens);
                    tracing::info!(
                        request_id,
                        provider = %provider_name,
                        model = %route.model,
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
                        .report_failure(&provider_name, &api_key, status_code);
                    // Record failure reason for metadata learning
                    if let Some(ref meta) = self.model_meta {
                        meta.learn_from_failure(
                            &provider_name, &route.model,
                            &e.to_string(), status_code,
                        );
                    }
                    last_provider = Some(provider_name.clone());
                    tracing::warn!(
                        request_id,
                        provider = %provider_name,
                        model = %route.model,
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

        // ─── Emergency cross-provider fallback ──────────────────────
        // When all model-specific candidates fail with server-side errors
        // (transport unreachable, rate-limited, timeout, 5xx), try fallback
        // providers that had NO model-specific candidates in the main list.
        // They might accept the model even if not in their advertised inventory.
        let is_server_error = matches!(&error,
            GatewayError::Reqwest(_)
            | GatewayError::HttpError { status: 429, .. }
            | GatewayError::RateLimited
            | GatewayError::Timeout(_)
        );

        if is_server_error {
            // ── Round 1: Cross-provider fallback ──────────────
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

                let mut req = request.clone();
                req.model = route.model.clone();
                req.stream = Some(false);

                tracing::info!(
                    request_id,
                    provider = %provider,
                    model = %req.model,
                    stage = "emergency_fallback",
                    "Attempting emergency cross-provider fallback"
                );

                match emergency_provider.chat(&emergency_key, req).await {
                    Ok(response) => {
                        let (prompt_tokens, completion_tokens) =
                            crate::models::extract_usage(&response.body);
                        self.keyhub.report_success(
                            &provider, &emergency_key,
                            prompt_tokens, completion_tokens,
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

            // ── Round 2: Paid key escalation ────────────────
            // When all free keys from a provider hit 429, try paid keys
            // from the same providers as the last resort.
            if matches!(&error, GatewayError::HttpError { status: 429, .. } | GatewayError::RateLimited) {
                for provider in &providers_with_candidates {
                    let Some(paid_key) = self.keyhub.any_available_key(provider) else {
                        continue;
                    };
                    // Skip if the key is the same tier as what already failed
                    let Some(emergency_provider) = self.providers.get(provider) else {
                        continue;
                    };

                    let mut req = request.clone();
                    req.model = route.model.clone();
                    req.stream = Some(false);

                    tracing::info!(
                        request_id,
                        provider = %provider,
                        model = %req.model,
                        stage = "paid_key_escalation",
                        "Attempting paid key escalation after free keys exhausted"
                    );

                    match emergency_provider.chat(&paid_key, req).await {
                        Ok(response) => {
                            let (prompt_tokens, completion_tokens) =
                                crate::models::extract_usage(&response.body);
                            self.keyhub.report_success(
                                provider, &paid_key,
                                prompt_tokens, completion_tokens,
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

    /// Send a streaming chat completion request with automatic fallback.
    pub async fn chat_stream(
        &self,
        request: &ChatCompletionRequest,
    ) -> GatewayResult<StreamResponse> {
        let route = self.resolve(&request.model, request.agent_name.as_deref())?;
        let request_id = request.request_id.as_deref().unwrap_or("unknown");
        let agent_name = request.agent_name.as_deref();
        let candidates = self.candidates_with_refresh(&route, request_id, agent_name).await;
        if candidates.is_empty() {
            let available: Vec<String> = self.providers.iter().map(|p| p.key().clone()).collect();
            let model_summary = self.keyhub.free_model_summary();
            tracing::warn!(
                request_id,
                model = %route.model,
                provider = %route.provider_name,
                providers_checked = %available.join(", "),
                free_model_counts = %model_summary,
                "No free keys found for model (stream) — model may not exist in any provider's inventory"
            );
            if let Some(ref meta) = self.model_meta {
                meta.learn_from_failure(&route.provider_name, &route.model, "model_not_found", 404);
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
        let providers_with_candidates: std::collections::HashSet<String> =
            candidates.iter().map(|(p, _)| p.clone()).collect();
        let mut last_error: Option<GatewayError> = None;
        let attempt_count = candidates.len();
        let mut last_provider: Option<String> = None;

        for (attempt_index, (provider_name, api_key)) in candidates.into_iter().enumerate() {
            let provider = match self.providers.get(&provider_name) {
                Some(p) => p,
                None => continue,
            };

            let mut req = request.clone();
            // Inject context handoff if this is a fallback attempt
            if attempt_index > 0 {
                if let Some(ref prev) = last_provider {
                    req = inject_context_handoff(
                        req,
                        prev,
                        &provider_name,
                        &route.model,
                    );
                }
            }
            req.model = route.model.clone();
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

            let result: GatewayResult<StreamResponse> = provider.chat_stream(&api_key, req).await;
            match result {
                Ok(response) => {
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
                        provider_name,
                        api_key,
                        request_id.to_string(),
                        upstream_model,
                    ));
                }
                Err(e) => {
                    let status_code = e.http_status();
                    self.keyhub
                        .report_failure(&provider_name, &api_key, status_code);
                    // Record failure reason for metadata learning
                    if let Some(ref meta) = self.model_meta {
                        meta.learn_from_failure(
                            &provider_name, &upstream_model,
                            &e.to_string(), status_code,
                        );
                    }
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

        // ─── Emergency cross-provider fallback (stream) ────────────
        let is_server_error = matches!(&error,
            GatewayError::Reqwest(_)
            | GatewayError::HttpError { status: 429, .. }
            | GatewayError::RateLimited
            | GatewayError::Timeout(_)
        );

        if is_server_error {
            // ── Round 1: Cross-provider fallback (stream) ─────
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

                let mut req = request.clone();
                req.model = route.model.clone();
                let upstream_model = req.model.clone();

                tracing::info!(
                    request_id,
                    provider = %provider,
                    model = %upstream_model,
                    stage = "emergency_fallback_stream",
                    "Attempting emergency cross-provider stream fallback"
                );

                match emergency_provider.chat_stream(&emergency_key, req).await {
                    Ok(response) => {
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
                            provider,
                            emergency_key,
                            request_id.to_string(),
                            upstream_model,
                        ));
                    }
                    Err(e) => {
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

            // ── Round 2: Paid key escalation (stream) ────────
            if matches!(&error, GatewayError::HttpError { status: 429, .. } | GatewayError::RateLimited) {
                for provider in &providers_with_candidates {
                    let Some(paid_key) = self.keyhub.any_available_key(provider) else {
                        continue;
                    };
                    let Some(emergency_provider) = self.providers.get(provider) else {
                        continue;
                    };

                    let mut req = request.clone();
                    req.model = route.model.clone();
                    let upstream_model = req.model.clone();

                    tracing::info!(
                        request_id,
                        provider = %provider,
                        model = %upstream_model,
                        stage = "paid_key_escalation_stream",
                        "Attempting paid key escalation (stream)"
                    );

                    match emergency_provider.chat_stream(&paid_key, req).await {
                        Ok(response) => {
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
                                provider.clone(),
                                paid_key,
                                request_id.to_string(),
                                upstream_model,
                            ));
                        }
                        Err(e) => {
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

fn account_stream(
    stream: StreamResponse,
    keyhub: Arc<KeyHub>,
    provider_name: String,
    api_key: String,
    request_id: String,
    model: String,
) -> StreamResponse {
    struct State {
        stream: StreamResponse,
        keyhub: Arc<KeyHub>,
        provider_name: String,
        api_key: String,
        request_id: String,
        model: String,
        terminal: bool,
        started: Instant,
        /// Last SSE chunk buffered so we can extract `usage` when the stream ends.
        last_chunk: Option<Bytes>,
    }

    let state = State {
        stream,
        keyhub,
        provider_name,
        api_key,
        request_id,
        model,
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
                state.last_chunk = Some(bytes.clone());
                Some((Ok(bytes), state))
            }
            Some(Err(error)) => {
                state.keyhub.report_failure(
                    &state.provider_name,
                    &state.api_key,
                    error.http_status(),
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
                let (pt, ct): (Option<u32>, Option<u32>) = state
                    .last_chunk
                    .as_ref()
                    .map(|bytes| extract_stream_usage(bytes))
                    .unwrap_or((None, None));
                if pt.is_some() || ct.is_some() {
                    tracing::debug!(
                        request_id = %state.request_id,
                        provider = %state.provider_name,
                        model = %state.model,
                        prompt_tokens = ?pt,
                        completion_tokens = ?ct,
                        "Stream token usage extracted from final chunk"
                    );
                }
                state.keyhub.report_success(
                    &state.provider_name,
                    &state.api_key,
                    pt,
                    ct,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentConfig, ModelAlias, ProviderConfig, ProviderType};
    use parking_lot::RwLock;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    fn make_router(config: Arc<Config>, providers: Arc<DashMap<String, BoxedProvider>>, keyhub: Arc<KeyHub>) -> Router {
        let disabled_models = Arc::new(RwLock::new(HashMap::<String, HashSet<String>>::new()));
        Router::new(config, providers, keyhub, disabled_models, None)
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
