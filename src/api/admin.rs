/// Admin endpoints: configuration management, provider testing, SSE events.
use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

use axum::{
    extract::{Path, Query, State},
    response::{
        Json,
        sse::{Event, Sse},
    },
};
use futures::stream::Stream;
use serde_json::json;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::AppState;
use crate::keyhub::key_fingerprint;
use crate::metadata::ModelMetaStore;
use crate::models::KeyStatus;
use crate::providers::traits::{ChatResponse, Provider};

/// GET /admin/config — Return masked configuration.
pub async fn admin_config_get(State(state): State<AppState>) -> Json<serde_json::Value> {
    let config = &state.config;
    let mut providers = serde_json::Map::new();

    for (name, pc) in &config.providers {
        let masked_keys: Vec<serde_json::Value> = pc
            .keys
            .iter()
            .map(|k| {
                let v = k.value();
                if v.len() > 8 {
                    json!(format!("{}...{}", &v[..4], &v[v.len() - 4..]))
                } else {
                    json!("****")
                }
            })
            .collect();

        providers.insert(
            name.clone(),
            json!({
                "type": format!("{:?}", pc.provider_type),
                "enabled": pc.enabled,
                "base_url": pc.base_url,
                "proxy_url": pc.proxy_url,
                "keys": masked_keys,
                "keys_count": pc.keys.len(),
                "health_check_model": pc.health_check_model,
                "timeout_seconds": pc.timeout_seconds,
                "priority": pc.priority,
            }),
        );
    }

    Json(json!({
        "server": {
            "host": config.server.host,
            "port": config.server.port,
            "log_level": config.server.log_level,
            "request_timeout": config.server.request_timeout,
            "sse_keepalive": config.server.sse_keepalive,
        },
        "routing": {
            "strategy": format!("{:?}", config.routing.strategy),
            "fail_threshold": config.routing.fail_threshold,
            "cooldown_seconds": config.routing.cooldown_seconds,
            "auto_discover": config.routing.auto_discover,
        },
        "fallback": config.fallback,
        "agents": config.agents,
        "models": config.models,
        "providers": providers,
        "watcher": {
            "enabled": config.watcher.enabled,
            "interval_seconds": config.watcher.interval_seconds,
            "check_timeout_seconds": config.watcher.check_timeout_seconds,
        },
        "adaptive_routing": config.adaptive_routing,
    }))
}

/// PUT /admin/config — Update configuration (partial update).
pub async fn admin_config_put(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    // Update provider settings in the running config
    if let Some(providers_update) = body.get("providers").and_then(|p| p.as_object()) {
        for (name, updates) in providers_update {
            if let Some(pc) = state.config.providers.get(name) {
                let mut new_config = pc.clone();
                if let Some(enabled) = updates.get("enabled").and_then(|v| v.as_bool()) {
                    new_config.enabled = enabled;
                }
                if let Some(timeout) = updates.get("timeout_seconds").and_then(|v| v.as_u64()) {
                    new_config.timeout_seconds = timeout;
                }
                if let Some(base_url) = updates.get("base_url").and_then(|v| v.as_str()) {
                    new_config.base_url = base_url.to_string();
                }
                if let Some(keys) = updates.get("keys").and_then(|v| v.as_array()) {
                    let new_keys: Vec<String> = keys
                        .iter()
                        .filter_map(|k| k.as_str())
                        .map(|s| s.to_string())
                        .collect();
                    if !new_keys.is_empty() {
                        new_config.keys = new_keys.into_iter().map(|s| s.into()).collect();
                    }
                }

                // Re-register provider if needed
                if (new_config.enabled != pc.enabled || new_config.base_url != pc.base_url)
                    && let Ok(provider) = crate::providers::create_provider(name, &new_config)
                {
                    state.providers.insert(name.clone(), provider);
                    state
                        .keyhub
                        .register_provider(name, new_config.keys.clone());
                }

                // Config is immutable after startup; this endpoint applies runtime
                // provider/key changes only. Persist durable config changes in config.yaml.

                // Broadcast config update event
                let _ = state.sse_tx.send(
                    json!({
                        "type": "config_update",
                        "data": { "provider": name, "enabled": new_config.enabled },
                        "timestamp": chrono::Utc::now().timestamp(),
                    })
                    .to_string(),
                );
            }
        }
    }

    // Return updated config
    admin_config_get(State(state)).await
}

/// GET /admin/status — Full gateway status with per-key rate data.
pub async fn admin_status(State(state): State<AppState>) -> Json<serde_json::Value> {
    let health_states = state.health_registry.snapshot();
    let uptime = state.start_time.elapsed().as_secs();
    let total_requests = state
        .request_counter
        .load(std::sync::atomic::Ordering::Relaxed);
    let total_errors = state
        .error_counter
        .load(std::sync::atomic::Ordering::Relaxed);

    // Build a lookup: provider_name -> key snapshots (real-time from keyhub)
    let key_snapshots: std::collections::HashMap<String, Vec<crate::models::KeyState>> = state
        .keyhub
        .snapshot()
        .into_iter()
        .map(|(name, keys)| (name.clone(), keys))
        .collect();

    // Build real-time available key count from keyhub snapshot
    use crate::models::KeyStatus;
    let now_secs = chrono::Utc::now().timestamp() as u64;
    let real_available: std::collections::HashMap<String, usize> = key_snapshots
        .iter()
        .map(|(name, keys)| {
            let avail = keys
                .iter()
                .filter(|k| k.status == KeyStatus::Available && !k.is_rate_limited(now_secs))
                .count();
            (name.clone(), avail)
        })
        .collect();

    let mut providers_detail = Vec::new();
    let mut healthy = 0usize;
    let mut degraded = 0usize;
    let mut exhausted = 0usize;
    let mut unhealthy = 0usize;

    for hs in &health_states {
        let config = state.config.providers.get(&hs.provider);
        let keys_for_provider = key_snapshots.get(&hs.provider).cloned().unwrap_or_default();
        let real_available_keys = real_available.get(&hs.provider).copied().unwrap_or(0);
        let total_keys = hs.total_keys;

        // Compute live status: health_registry knows about provider reachability,
        // keyhub knows about real-time key availability.
        let computed_status = match hs.status.as_str() {
            "unhealthy" => {
                unhealthy += 1;
                "unhealthy"
            }
            "disabled" => {
                unhealthy += 1;
                "disabled"
            }
            _ if real_available_keys == 0 && total_keys > 0 => {
                exhausted += 1;
                "exhausted"
            }
            _ if real_available_keys > 0 && real_available_keys < total_keys => {
                degraded += 1;
                "degraded"
            }
            _ if real_available_keys > 0 => {
                healthy += 1;
                "healthy"
            }
            _ => {
                healthy += 1;
                &hs.status
            }
        };

        providers_detail.push(json!({
            "name": hs.provider,
            "status": hs.status,
            "computed_status": computed_status,
            "latency_ms": hs.latency_ms,
            "success_count": hs.success_count,
            "fail_count": hs.fail_count,
            "models_count": hs.models_count,
            "available_keys": real_available_keys,
            "total_keys": total_keys,
            "type": config.map(|c| format!("{:?}", c.provider_type)).unwrap_or_default(),
            "base_url": config.map(|c| &c.base_url),
            "priority": config.map(|c| c.priority).unwrap_or(0),
            "last_error": if hs.last_error.is_empty() { serde_json::Value::Null } else { json!(hs.last_error) },
            "keys": keys_for_provider.into_iter().map(|k| json!({
                "key_id": k.key_id,
                "key": k.masked_key(),
                "tier": k.tier,
                "status": k.status,
                "success_count": k.success_count,
                "fail_count": k.fail_count,
                "total_fail_count": k.total_fail_count,
                "last_success_at": k.last_success_at,
                "last_error_at": k.last_error_at,
                "last_error_status": k.last_error_status,
                "status_updated_at": k.status_updated_at,
                "last_recovered_at": k.last_recovered_at,
                "rpm_limit": k.rpm_limit,
                "rpd_limit": k.rpd_limit,
                "rpm_limit_source": k.rpm_limit_source.clone(),
                "rpd_limit_source": k.rpd_limit_source.clone(),
                "tpm_limit": k.tpm_limit,
                "tpd_limit": k.tpd_limit,
                "rpm_count": k.rpm_count,
                "rpd_count": k.rpd_count,
                "tpm_total": k.tpm_prompt_count + k.tpm_completion_count,
                "tpd_total": k.tpd_prompt_count + k.tpd_completion_count,
                "rate_usage": key_rate_usage_json(&k),
                "cooldown_until": k.cooldown_until,
                "models": k.models,
            })).collect::<Vec<_>>(),
        }));
    }

    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_seconds": uptime,
        "total_requests": total_requests,
        "total_errors": total_errors,
        "providers": providers_detail,
        "fallback_chain": state.config.fallback,
        "healthy_count": healthy + degraded,
        "unhealthy_count": unhealthy,
        "total_providers": health_states.len(),
        "exhausted_count": exhausted,
        "routing_strategy": format!("{:?}", state.config.routing.strategy),
    }))
}

/// POST /admin/providers/:name/refresh — Refresh a provider's model list and health.
pub async fn admin_provider_refresh(
    State(state): State<AppState>,
    Path(provider_name): Path<String>,
) -> Json<serde_json::Value> {
    // Find and check this provider
    let provider = match state.providers.get(&provider_name) {
        Some(p) => p,
        None => {
            return Json(json!({
                "success": false,
                "error": format!("Provider '{}' not found", provider_name),
            }));
        }
    };

    // Run health check for this provider
    let check_timeout = std::time::Duration::from_secs(state.config.watcher.check_timeout_seconds);
    let mut successful_keys = 0usize;
    let mut total_latency = 0u64;
    let mut provider_models = std::collections::BTreeSet::new();
    let mut last_error = String::new();

    for (api_key, _tier) in state.keyhub.model_probe_keys(&provider_name) {
        let is_disabled_probe = matches!(
            state.keyhub.key_status(&provider_name, &api_key),
            Some(KeyStatus::Disabled)
        );
        let reserved = if is_disabled_probe {
            false
        } else {
            state.keyhub.reserve_key(&provider_name, &api_key)
        };
        if !reserved && !is_disabled_probe {
            continue;
        }
        let started = Instant::now();
        match tokio::time::timeout(check_timeout, provider.list_models(&api_key)).await {
            Ok(Ok(models)) => {
                total_latency += started.elapsed().as_millis() as u64;
                successful_keys += 1;
                provider_models.extend(models.iter().cloned());
                state.keyhub.update_models(&provider_name, &api_key, models);
                if reserved {
                    state
                        .keyhub
                        .report_reserved_success(&provider_name, &api_key, None, None);
                }
            }
            Ok(Err(error)) => {
                state
                    .keyhub
                    .report_gateway_error(&provider_name, &api_key, &error);
                last_error = error.to_string();
                state.keyhub.record_model_error(
                    &provider_name,
                    &api_key,
                    &crate::error::sanitize_diagnostic(&last_error),
                );
            }
            Err(_) => {
                last_error = "Model discovery timed out".into();
                state
                    .keyhub
                    .record_model_error(&provider_name, &api_key, &last_error);
            }
        }
    }

    let available = state.keyhub.available_count(&provider_name);
    let total = state
        .config
        .providers
        .get(&provider_name)
        .map(|pc| pc.keys.len())
        .unwrap_or(0);

    if successful_keys > 0 {
        let avg_latency = total_latency / successful_keys as u64;
        state.health_registry.update(
            &provider_name,
            "healthy",
            avg_latency,
            provider_models.len(),
            available,
            total,
        );
    } else {
        state.health_registry.record_error_with_counts(
            &provider_name,
            if last_error.is_empty() {
                "No configured keys"
            } else {
                &last_error
            },
            available,
            total,
        );
    }

    // Broadcast event
    let _ = state.sse_tx.send(json!({
        "type": "health_update",
        "data": { "provider": &provider_name, "status": if successful_keys > 0 { "healthy" } else { "unhealthy" } },
        "timestamp": chrono::Utc::now().timestamp(),
    }).to_string());

    // Get updated health state
    let health_snapshot = state.health_registry.snapshot();
    let updated = health_snapshot
        .into_iter()
        .find(|h| h.provider == provider_name);

    Json(json!({
        "success": true,
        "provider": provider_name,
        "models_found": provider_models.len(),
        "health": updated,
    }))
}

/// POST /admin/providers/:name/test — Test a provider with a real chat completion.
fn admin_test_model_candidates(
    state: &AppState,
    provider_name: &str,
    api_key: Option<&str>,
    key_id: Option<&str>,
) -> Vec<String> {
    let disabled = state
        .disabled_models
        .read()
        .get(provider_name)
        .cloned()
        .unwrap_or_default();
    let mut candidates = Vec::new();
    if let Some(key_id) = key_id {
        candidates.extend(state.keyhub.models_for_key_id(provider_name, key_id));
    }
    if let Some(api_key) = api_key {
        candidates.extend(state.keyhub.models_for_key(provider_name, api_key));
    }
    if let Some(model) = state
        .config
        .providers
        .get(provider_name)
        .map(|pc| pc.health_check_model.trim())
        .filter(|model| !model.is_empty())
    {
        candidates.push(model.to_string());
    }
    dedupe_enabled_models(candidates, &disabled)
}

fn dedupe_enabled_models(candidates: Vec<String>, disabled: &HashSet<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(|model| !model.trim().is_empty())
        .filter(|model| !disabled.contains(model))
        .filter(|model| seen.insert(model.clone()))
        .collect()
}

fn validation_test_request(model: &str) -> crate::models::ChatCompletionRequest {
    crate::models::ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![crate::models::ChatMessage {
            role: "user".to_string(),
            content: serde_json::Value::String("Reply with OK".to_string()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            extra: serde_json::Map::new(),
        }],
        temperature: None,
        top_p: None,
        n: None,
        stream: Some(false),
        stop: None,
        max_tokens: Some(20),
        presence_penalty: None,
        frequency_penalty: None,
        user: None,
        request_id: None,
        agent_name: None,
        extra: serde_json::Map::new(),
    }
}

async fn send_admin_probe(
    provider: &dyn Provider,
    api_key: &str,
    model: &str,
    timeout_dur: std::time::Duration,
) -> Result<(ChatResponse, u64), crate::error::GatewayError> {
    let started = Instant::now();
    let request = validation_test_request(model);
    let response = tokio::time::timeout(timeout_dur, provider.chat(api_key, request))
        .await
        .map_err(|_| crate::error::GatewayError::Timeout("admin validation timed out".into()))??;
    Ok((response, started.elapsed().as_millis() as u64))
}

fn is_validation_model_mismatch(error: &crate::error::GatewayError) -> bool {
    let status = error.http_status();
    let message = error.to_string().to_lowercase();
    status == 404
        || (status == 403
            && (message.contains("not available in your region")
                || message.contains("model is not available")
                || message.contains("model not available")
                || message.contains("model_not_found")))
}

fn validation_error_should_update_key_state(error: &crate::error::GatewayError) -> bool {
    error.http_status() == 429 || error.is_auth_failure()
}

fn validation_failure_status(error: &crate::error::GatewayError) -> &'static str {
    if validation_error_should_update_key_state(error) {
        "key_limited_or_invalid"
    } else {
        "inconclusive"
    }
}

#[allow(clippy::too_many_arguments)]
fn record_admin_validation_attempt(
    model_meta: &Option<ModelMetaStore>,
    request_id: &str,
    attempt_index: i64,
    provider_name: &str,
    model: &str,
    api_key: &str,
    result: Result<u16, &crate::error::GatewayError>,
    fallback: bool,
) {
    let Some(meta) = model_meta else {
        return;
    };
    let (success, category, status, message, cooldown_seconds) = match result {
        Ok(status) => (true, "success", Some(status), None, None),
        Err(error) => (
            false,
            error.category(),
            Some(error.http_status()),
            Some(error.to_string()),
            error.retry_after_seconds().map(|seconds| seconds as i64),
        ),
    };
    if let Err(error) = meta.record_request_attempt(
        request_id,
        attempt_index,
        provider_name,
        model,
        &key_fingerprint(api_key),
        success,
        category,
        status,
        message.as_deref(),
        cooldown_seconds,
        fallback,
    ) {
        tracing::warn!(
            request_id,
            provider = %provider_name,
            model,
            error = %crate::error::sanitize_diagnostic(&error.to_string()),
            "Failed to record admin validation attempt"
        );
    }
}

fn response_model_and_preview(
    body: &serde_json::Value,
    fallback_model: &str,
) -> (String, Option<String>) {
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or(fallback_model)
        .to_string();
    let content_preview = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_str())
        .map(|s| {
            if s.len() > 100 {
                format!("{}...", &s[..100])
            } else {
                s.to_string()
            }
        });
    (model, content_preview)
}

pub async fn admin_provider_test(
    State(state): State<AppState>,
    Path(provider_name): Path<String>,
) -> Json<serde_json::Value> {
    let provider = match state.providers.get(&provider_name) {
        Some(p) => p,
        None => {
            return Json(json!({
                "success": false,
                "error": format!("Provider '{}' not found", provider_name),
            }));
        }
    };

    // Get an API key
    let api_key = match state.keyhub.acquire_key(&provider_name) {
        Ok(key) => key,
        Err(e) => {
            return Json(json!({
                "success": false,
                "error": format!("No available key: {}", e),
            }));
        }
    };
    if !state.keyhub.reserve_key(&provider_name, &api_key) {
        return Json(json!({
            "success": false,
            "error": "Selected key is no longer available",
        }));
    }

    let timeout_dur = std::time::Duration::from_secs(
        state
            .config
            .providers
            .get(&provider_name)
            .map(|pc| pc.timeout_seconds)
            .unwrap_or(30),
    );
    let candidates = admin_test_model_candidates(&state, &provider_name, Some(&api_key), None);
    let validation_request_id = format!(
        "admin-test-{}-{}",
        provider_name,
        chrono::Utc::now().timestamp_millis()
    );
    let mut skipped_models = Vec::new();
    for (attempt_index, test_model) in candidates.iter().enumerate() {
        match send_admin_probe(provider.as_ref(), &api_key, test_model, timeout_dur).await {
            Ok((response, latency)) => {
                record_admin_validation_attempt(
                    &state.model_meta,
                    &validation_request_id,
                    (attempt_index + 1) as i64,
                    &provider_name,
                    test_model,
                    &api_key,
                    Ok(response.status),
                    false,
                );
                state
                    .keyhub
                    .report_reserved_success(&provider_name, &api_key, None, None);

                let _ = state.sse_tx.send(
                    json!({
                        "type": "provider_test",
                        "data": { "provider": &provider_name, "success": true, "latency_ms": latency, "model": test_model },
                        "timestamp": chrono::Utc::now().timestamp(),
                    })
                    .to_string(),
                );

                let (model, content_preview) =
                    response_model_and_preview(&response.body, test_model);
                return Json(json!({
                    "success": true,
                    "provider": provider_name,
                    "model": model,
                    "attempted_models": candidates,
                    "skipped_models": skipped_models,
                    "latency_ms": latency,
                    "status": response.status,
                    "response_preview": content_preview,
                }));
            }
            Err(e) if is_validation_model_mismatch(&e) => {
                record_admin_validation_attempt(
                    &state.model_meta,
                    &validation_request_id,
                    (attempt_index + 1) as i64,
                    &provider_name,
                    test_model,
                    &api_key,
                    Err(&e),
                    attempt_index + 1 < candidates.len(),
                );
                skipped_models.push(json!({
                    "model": test_model,
                    "http_status": e.http_status(),
                    "error": e.to_string(),
                }));
                continue;
            }
            Err(e) => {
                record_admin_validation_attempt(
                    &state.model_meta,
                    &validation_request_id,
                    (attempt_index + 1) as i64,
                    &provider_name,
                    test_model,
                    &api_key,
                    Err(&e),
                    false,
                );
                let status = e.http_status();
                let key_state_updated = validation_error_should_update_key_state(&e);
                if validation_error_should_update_key_state(&e) {
                    state
                        .keyhub
                        .report_gateway_error(&provider_name, &api_key, &e);
                }
                return Json(json!({
                    "success": false,
                    "provider": provider_name,
                    "model": test_model,
                    "attempted_models": candidates,
                    "skipped_models": skipped_models,
                    "error": e.to_string(),
                    "http_status": status,
                    "validation_status": validation_failure_status(&e),
                    "key_state_updated": key_state_updated,
                }));
            }
        }
    }

    if let Some(last) = skipped_models.last() {
        Json(json!({
            "success": false,
            "provider": provider_name,
            "attempted_models": candidates,
            "skipped_models": skipped_models,
            "error": last.get("error").cloned().unwrap_or_else(|| json!("No validation model succeeded")),
            "http_status": last.get("http_status").cloned().unwrap_or_else(|| json!(400)),
        }))
    } else {
        let e = crate::error::GatewayError::InvalidRequest(
            "No enabled validation models available".into(),
        );
        state
            .keyhub
            .report_gateway_error(&provider_name, &api_key, &e);
        Json(json!({
            "success": false,
            "provider": provider_name,
            "error": e.to_string(),
            "http_status": e.http_status(),
        }))
    }
}

/// GET /admin/providers/{name}/models — List models with enabled/disabled status.
pub async fn admin_provider_models_get(
    State(state): State<AppState>,
    Path(provider_name): Path<String>,
) -> Json<serde_json::Value> {
    // Collect all models for this provider from the keyhub snapshot,
    // aggregating per-key rate limits for each model.
    let snapshot = state.keyhub.snapshot();
    let disabled = state
        .disabled_models
        .read()
        .get(&provider_name)
        .cloned()
        .unwrap_or_default();

    // model -> aggregated info
    use std::collections::BTreeMap;
    let mut model_map: BTreeMap<String, serde_json::Value> = BTreeMap::new();

    for (pname, keys) in &snapshot {
        if pname != &provider_name {
            continue;
        }
        for key in keys {
            for model in &key.models {
                let entry = model_map.entry(model.clone()).or_insert_with(|| {
                    json!({
                        "id": model,
                        "enabled": !disabled.contains(model),
                        "key_count": 0,
                        "rpm_limit": null,
                        "rpd_limit": null,
                        "tpm_limit": null,
                        "tpd_limit": null,
                        "effective_rpm_limit": null,
                        "effective_rpd_limit": null,
                        "effective_tpm_limit": null,
                        "effective_tpd_limit": null,
                        "rpm_remaining": null,
                        "rpd_remaining": null,
                        "tpm_remaining": null,
                        "tpd_remaining": null,
                        "rpm_unconstrained": false,
                        "rpd_unconstrained": false,
                        "tpm_unconstrained": false,
                        "tpd_unconstrained": false,
                        "keys_healthy": 0,
                        "available": false,
                        "unavailable_reason": null,
                    })
                });
                entry["key_count"] = json!(entry["key_count"].as_u64().unwrap_or(0) + 1);
                // Sum rate limits from all keys serving this model
                if let Some(rpm) = key.rpm_limit {
                    let current = entry["rpm_limit"].as_u64().unwrap_or(0);
                    entry["rpm_limit"] = json!(current + rpm as u64);
                }
                if let Some(rpd) = key.rpd_limit {
                    let current = entry["rpd_limit"].as_u64().unwrap_or(0);
                    entry["rpd_limit"] = json!(current + rpd as u64);
                }
                if let Some(tpm) = key.tpm_limit {
                    let current = entry["tpm_limit"].as_u64().unwrap_or(0);
                    entry["tpm_limit"] = json!(current + tpm as u64);
                }
                if let Some(tpd) = key.tpd_limit {
                    let current = entry["tpd_limit"].as_u64().unwrap_or(0);
                    entry["tpd_limit"] = json!(current + tpd as u64);
                }
                let now_secs = chrono::Utc::now().timestamp() as u64;
                if key.status == crate::models::KeyStatus::Available
                    && !key.is_rate_limited(now_secs)
                {
                    entry["keys_healthy"] = json!(entry["keys_healthy"].as_u64().unwrap_or(0) + 1);
                    add_effective_model_capacity(entry, key, now_secs);
                }
            }
        }
    }

    for model in model_map.values_mut() {
        let enabled = model["enabled"].as_bool().unwrap_or(false);
        let healthy = model["keys_healthy"].as_u64().unwrap_or(0);
        let available = enabled && healthy > 0;
        model["available"] = json!(available);
        model["unavailable_reason"] = if !enabled {
            json!("disabled")
        } else if healthy == 0 {
            json!("no_available_key")
        } else {
            serde_json::Value::Null
        };
    }

    let models: Vec<serde_json::Value> = model_map.into_values().collect();
    let enabled_count = models
        .iter()
        .filter(|m| m["enabled"].as_bool().unwrap_or(false))
        .count();
    let disabled_count = models.len() - enabled_count;

    Json(json!({
        "provider": provider_name,
        "models": models,
        "total": models.len(),
        "enabled_count": enabled_count,
        "disabled_count": disabled_count,
    }))
}

fn add_effective_model_capacity(
    entry: &mut serde_json::Value,
    key: &crate::models::KeyState,
    now_secs: u64,
) {
    let now_min = now_secs / 60;
    let now_day = now_secs / 86400;
    let axes = [
        (
            "rpm",
            key.rpm_limit,
            if key.rpm_window_start == now_min {
                key.rpm_count
            } else {
                0
            },
        ),
        (
            "rpd",
            key.rpd_limit,
            if key.rpd_window_start == now_day {
                key.rpd_count
            } else {
                0
            },
        ),
        (
            "tpm",
            key.tpm_limit,
            if key.rpm_window_start == now_min {
                key.tpm_prompt_count
                    .saturating_add(key.tpm_completion_count)
            } else {
                0
            },
        ),
        (
            "tpd",
            key.tpd_limit,
            if key.rpd_window_start == now_day {
                key.tpd_prompt_count
                    .saturating_add(key.tpd_completion_count)
            } else {
                0
            },
        ),
    ];

    for (axis, limit, used) in axes {
        if let Some(limit) = limit {
            add_u64_field(entry, &format!("effective_{axis}_limit"), limit as u64);
            add_u64_field(
                entry,
                &format!("{axis}_remaining"),
                limit.saturating_sub(used) as u64,
            );
        } else {
            entry[format!("{axis}_unconstrained")] = json!(true);
        }
    }
}

fn add_u64_field(entry: &mut serde_json::Value, field: &str, value: u64) {
    let current = entry[field].as_u64().unwrap_or(0);
    entry[field] = json!(current + value);
}

/// POST /admin/providers/{name}/models/{model}/toggle — Toggle a model's enabled/disabled status.
pub async fn admin_provider_models_toggle(
    State(state): State<AppState>,
    Path((provider_name, model_id)): Path<(String, String)>,
) -> Json<serde_json::Value> {
    let mut disabled = state.disabled_models.write();
    let entry = disabled.entry(provider_name.clone()).or_default();

    let was_disabled = entry.remove(&model_id);
    let now_enabled = was_disabled; // if we removed from disabled set, it's now enabled
    if !was_disabled {
        // Was enabled → now disabled: add to disabled set
        entry.insert(model_id.clone());
    }

    // Broadcast SSE event
    let _ = state.sse_tx.send(
        json!({
            "type": "model_toggle",
            "data": {
                "provider": &provider_name,
                "model": &model_id,
                "enabled": now_enabled,
            },
            "timestamp": chrono::Utc::now().timestamp(),
        })
        .to_string(),
    );

    tracing::info!(
        provider = %provider_name,
        model = %model_id,
        enabled = now_enabled,
        "Model visibility toggled"
    );

    Json(json!({
        "success": true,
        "provider": provider_name,
        "model": model_id,
        "enabled": now_enabled,
    }))
}

/// POST /admin/providers/{name}/keys/{key_id}/restore — Manually restore a disabled key.
pub async fn admin_provider_key_restore(
    State(state): State<AppState>,
    Path((provider_name, key_id)): Path<(String, String)>,
) -> Json<serde_json::Value> {
    match state.keyhub.restore_key(&provider_name, &key_id) {
        Ok(key) => {
            let _ = state.sse_tx.send(
                json!({
                    "type": "key_restore",
                    "data": {
                        "provider": &provider_name,
                        "key_id": &key_id,
                        "status": key.status,
                    },
                    "timestamp": chrono::Utc::now().timestamp(),
                })
                .to_string(),
            );

            Json(json!({
                "success": true,
                "provider": provider_name,
                "key_id": key_id,
                "key": {
                    "key_id": key.key_id,
                    "key": key.masked_key(),
                    "status": key.status,
                    "fail_count": key.fail_count,
                    "last_error_at": key.last_error_at,
                    "last_error_status": key.last_error_status,
                    "status_updated_at": key.status_updated_at,
                    "last_recovered_at": key.last_recovered_at,
                },
            }))
        }
        Err(error) => Json(json!({
            "success": false,
            "provider": provider_name,
            "key_id": key_id,
            "error": error.to_string(),
        })),
    }
}

/// POST /admin/providers/{name}/keys/{key_id}/validate — Validate one key with a real chat request.
pub async fn admin_provider_key_validate(
    State(state): State<AppState>,
    Path((provider_name, key_id)): Path<(String, String)>,
) -> Json<serde_json::Value> {
    let provider = match state.providers.get(&provider_name) {
        Some(p) => p,
        None => {
            return Json(json!({
                "success": false,
                "provider": provider_name,
                "key_id": key_id,
                "error": format!("Provider '{}' not found", provider_name),
            }));
        }
    };

    let api_key = match state.keyhub.key_by_id(&provider_name, &key_id) {
        Ok(key) => key,
        Err(error) => {
            return Json(json!({
                "success": false,
                "provider": provider_name,
                "key_id": key_id,
                "error": error.to_string(),
            }));
        }
    };

    let timeout_dur = std::time::Duration::from_secs(
        state
            .config
            .providers
            .get(&provider_name)
            .map(|pc| pc.timeout_seconds)
            .unwrap_or(30)
            .clamp(5, 60),
    );
    let candidates =
        admin_test_model_candidates(&state, &provider_name, Some(&api_key), Some(&key_id));
    let validation_request_id = format!(
        "admin-validate-{}-{}-{}",
        provider_name,
        key_id,
        chrono::Utc::now().timestamp_millis()
    );
    let mut skipped_models = Vec::new();
    for (attempt_index, test_model) in candidates.iter().enumerate() {
        match send_admin_probe(provider.as_ref(), &api_key, test_model, timeout_dur).await {
            Ok((response, latency)) => {
                record_admin_validation_attempt(
                    &state.model_meta,
                    &validation_request_id,
                    (attempt_index + 1) as i64,
                    &provider_name,
                    test_model,
                    &api_key,
                    Ok(response.status),
                    false,
                );
                state
                    .keyhub
                    .report_success(&provider_name, &api_key, None, None);

                let _ = state.sse_tx.send(
                    json!({
                        "type": "key_validate",
                        "data": {
                            "provider": &provider_name,
                            "key_id": &key_id,
                            "success": true,
                            "latency_ms": latency,
                            "model": test_model,
                        },
                        "timestamp": chrono::Utc::now().timestamp(),
                    })
                    .to_string(),
                );

                let (model, content_preview) =
                    response_model_and_preview(&response.body, test_model);
                return Json(json!({
                    "success": true,
                    "provider": provider_name,
                    "key_id": key_id,
                    "model": model,
                    "attempted_models": candidates,
                    "skipped_models": skipped_models,
                    "latency_ms": latency,
                    "status": response.status,
                    "response_preview": content_preview,
                }));
            }
            Err(error) if is_validation_model_mismatch(&error) => {
                record_admin_validation_attempt(
                    &state.model_meta,
                    &validation_request_id,
                    (attempt_index + 1) as i64,
                    &provider_name,
                    test_model,
                    &api_key,
                    Err(&error),
                    attempt_index + 1 < candidates.len(),
                );
                skipped_models.push(json!({
                    "model": test_model,
                    "http_status": error.http_status(),
                    "error": error.to_string(),
                }));
                continue;
            }
            Err(error) => {
                record_admin_validation_attempt(
                    &state.model_meta,
                    &validation_request_id,
                    (attempt_index + 1) as i64,
                    &provider_name,
                    test_model,
                    &api_key,
                    Err(&error),
                    false,
                );
                let status = error.http_status();
                let key_state_updated = validation_error_should_update_key_state(&error);
                if validation_error_should_update_key_state(&error) {
                    state
                        .keyhub
                        .report_gateway_error(&provider_name, &api_key, &error);
                }
                return Json(json!({
                    "success": false,
                    "provider": provider_name,
                    "key_id": key_id,
                    "model": test_model,
                    "attempted_models": candidates,
                    "skipped_models": skipped_models,
                    "error": error.to_string(),
                    "http_status": status,
                    "validation_status": validation_failure_status(&error),
                    "key_state_updated": key_state_updated,
                }));
            }
        }
    }

    if let Some(last) = skipped_models.last() {
        Json(json!({
            "success": false,
            "provider": provider_name,
            "key_id": key_id,
            "attempted_models": candidates,
            "skipped_models": skipped_models,
            "error": last.get("error").cloned().unwrap_or_else(|| json!("No validation model succeeded")),
            "http_status": last.get("http_status").cloned().unwrap_or_else(|| json!(400)),
        }))
    } else {
        let error = crate::error::GatewayError::InvalidRequest(
            "No enabled validation models available".into(),
        );
        state
            .keyhub
            .report_gateway_error(&provider_name, &api_key, &error);
        Json(json!({
            "success": false,
            "provider": provider_name,
            "key_id": key_id,
            "error": error.to_string(),
            "http_status": error.http_status(),
        }))
    }
}

/// POST /admin/save — Persist current state (disabled_models, keyhub states) to state.json.
pub async fn admin_save(State(state): State<AppState>) -> Json<serde_json::Value> {
    let keyhub_snapshot = state.keyhub.snapshot();

    let mut persisted = crate::state::PersistedState::new();
    for (provider, keys) in keyhub_snapshot {
        persisted
            .providers
            .insert(provider, crate::state::ProviderKeyState { keys });
    }

    // Collect disabled models
    {
        let dm = state.disabled_models.read();
        persisted.disabled_models = dm
            .iter()
            .map(|(provider, models)| (provider.clone(), models.iter().cloned().collect()))
            .collect();
    }

    // Update the in-memory cached state too
    {
        let mut guard = state.state.write();
        *guard = persisted.clone();
    }

    match persisted.save(&state.config.state.state_file) {
        Ok(_) => {
            tracing::info!("State saved to {}", state.config.state.state_file);
            Json(json!({ "success": true, "message": "State saved" }))
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to save state");
            Json(json!({ "success": false, "message": format!("Save failed: {}", e) }))
        }
    }
}

/// GET /admin/keys — Detailed per-key rate and quota information.
pub async fn admin_keys(State(state): State<AppState>) -> Json<serde_json::Value> {
    let snapshot = state.keyhub.snapshot();
    let mut providers = serde_json::Map::new();
    for (provider_name, keys) in snapshot {
        let key_list: Vec<serde_json::Value> = keys
            .into_iter()
            .map(|k| {
                let now_secs = chrono::Utc::now().timestamp() as u64;
                let now_min = now_secs / 60;
                let now_day = now_secs / 86400;
                let rpm_window_active = k.rpm_window_start == now_min;
                let rpd_window_active = k.rpd_window_start == now_day;
                let rpm_used = if rpm_window_active { k.rpm_count } else { 0 };
                let rpd_used = if rpd_window_active { k.rpd_count } else { 0 };
                let tpm_used = if rpm_window_active {
                    k.tpm_prompt_count + k.tpm_completion_count
                } else {
                    0
                };
                let tpd_used = if rpd_window_active {
                    k.tpd_prompt_count + k.tpd_completion_count
                } else {
                    0
                };

                let rpm_pct = k.rpm_limit.map(|lim| {
                    if lim > 0 {
                        (rpm_used as f64 / lim as f64) * 100.0
                    } else {
                        100.0
                    }
                });
                let rpd_pct = k.rpd_limit.map(|lim| {
                    if lim > 0 {
                        (rpd_used as f64 / lim as f64) * 100.0
                    } else {
                        100.0
                    }
                });
                let tpm_pct = k.tpm_limit.map(|lim| {
                    if lim > 0 {
                        (tpm_used as f64 / lim as f64) * 100.0
                    } else {
                        100.0
                    }
                });
                let tpd_pct = k.tpd_limit.map(|lim| {
                    if lim > 0 {
                        (tpd_used as f64 / lim as f64) * 100.0
                    } else {
                        100.0
                    }
                });

                json!({
                    "key_id": k.key_id,
                    "key": k.masked_key(),
                    "tier": k.tier,
                    "status": k.status,
                    "success_count": k.success_count,
                    "fail_count": k.fail_count,
                    "total_fail_count": k.total_fail_count,
                    "last_success_at": k.last_success_at,
                    "last_error_at": k.last_error_at,
                    "last_error_status": k.last_error_status,
                    "status_updated_at": k.status_updated_at,
                    "last_recovered_at": k.last_recovered_at,
                    "cooldown_until": k.cooldown_until,
                    "models": k.models,
                    "rate_limits": {
                        "rpm": { "limit": k.rpm_limit, "used": rpm_used, "percent": rpm_pct, "source": k.rpm_limit_source.clone() },
                        "rpd": { "limit": k.rpd_limit, "used": rpd_used, "percent": rpd_pct, "source": k.rpd_limit_source.clone() },
                        "tpm": { "limit": k.tpm_limit, "used": tpm_used, "percent": tpm_pct },
                        "tpd": { "limit": k.tpd_limit, "used": tpd_used, "percent": tpd_pct },
                    },
                    "rate_usage": key_rate_usage_json(&k),
                })
            })
            .collect();
        providers.insert(provider_name, json!(key_list));
    }

    Json(json!({
        "providers": providers,
    }))
}

fn key_rate_usage_json(k: &crate::models::KeyState) -> serde_json::Value {
    let now_secs = chrono::Utc::now().timestamp() as u64;
    let now_min = now_secs / 60;
    let now_day = now_secs / 86400;
    let blocked_by_status = matches!(
        k.status,
        crate::models::KeyStatus::RateLimited | crate::models::KeyStatus::Cooldown
    );
    let rpm_used = if k.rpm_window_start == now_min {
        k.rpm_count
    } else {
        0
    };
    let rpd_used = if k.rpd_window_start == now_day {
        k.rpd_count
    } else {
        0
    };
    let tpm_used = if k.rpm_window_start == now_min {
        k.tpm_prompt_count.saturating_add(k.tpm_completion_count)
    } else {
        0
    };
    let tpd_used = if k.rpd_window_start == now_day {
        k.tpd_prompt_count.saturating_add(k.tpd_completion_count)
    } else {
        0
    };

    let axes = [
        ("rpm", k.rpm_limit, rpm_used, k.rpm_window_start == now_min),
        ("rpd", k.rpd_limit, rpd_used, k.rpd_window_start == now_day),
        ("tpm", k.tpm_limit, tpm_used, k.rpm_window_start == now_min),
        ("tpd", k.tpd_limit, tpd_used, k.rpd_window_start == now_day),
    ];
    let mut axis_json = serde_json::Map::new();
    let mut max_percent = 0.0_f64;
    let mut constrained = false;
    let mut exhausted = false;

    for (name, limit, used, window_active) in axes {
        let remaining = limit.map(|limit| limit.saturating_sub(used));
        let percent = limit.map(|limit| {
            if limit == 0 {
                100.0
            } else {
                ((used as f64 / limit as f64) * 100.0).min(100.0)
            }
        });
        if let Some(percent) = percent {
            constrained = true;
            max_percent = max_percent.max(percent);
        }
        let axis_exhausted = limit.is_some_and(|limit| used >= limit);
        exhausted = exhausted || axis_exhausted;
        axis_json.insert(
            name.to_string(),
            json!({
                "limit": limit,
                "used": used,
                "remaining": remaining,
                "percent": percent,
                "window_active": window_active,
                "exhausted": axis_exhausted,
            }),
        );
    }

    let display_percent = if blocked_by_status {
        Some(100.0)
    } else if constrained {
        Some(max_percent)
    } else {
        None
    };
    let exhausted_for_display = exhausted || blocked_by_status;

    json!({
        "axes": axis_json,
        "constrained": constrained,
        "exhausted": exhausted_for_display,
        "counter_exhausted": exhausted,
        "blocked_by_status": blocked_by_status,
        "display_percent": display_percent,
        "max_percent": if constrained { Some(max_percent) } else { None },
        "headroom_percent": if blocked_by_status {
            Some(0.0)
        } else if constrained {
            Some((100.0 - max_percent).max(0.0))
        } else {
            None
        },
    })
}

/// GET /admin/events — SSE endpoint for real-time dashboard updates.
pub async fn admin_events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let rx = state.sse_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| {
        match result {
            Ok(msg) => Some(Ok(Event::default().data(msg))),
            Err(_) => None, // Lagged events: skip
        }
    });

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("ping"),
    )
}

// ─── Model Metadata API endpoints ─────────────────────────────────────

/// GET /admin/metadata — Summary stats about learned model metadata.
pub async fn admin_metadata_stats(State(state): State<AppState>) -> Json<serde_json::Value> {
    let Some(ref meta) = state.model_meta else {
        return Json(json!({ "enabled": false, "message": "Metadata DB not available" }));
    };

    let stats = match meta.get_stats() {
        Ok(s) => s,
        Err(e) => return Json(json!({ "enabled": true, "error": e.to_string() })),
    };

    let error_summary = meta.get_error_summary(30).unwrap_or_default();
    let mut error_categories: std::collections::BTreeMap<String, i64> =
        std::collections::BTreeMap::new();
    let mut error_total = 0i64;
    let mut top_failing_models: Vec<serde_json::Value> = Vec::new();
    let mut model_fails: std::collections::HashMap<String, i64> = std::collections::HashMap::new();

    for e in &error_summary {
        *error_categories.entry(e.category.clone()).or_insert(0) += e.total;
        error_total += e.total;
        let key = format!("{}/{}", e.provider, e.model_id);
        *model_fails.entry(key).or_insert(0) += e.total;
    }

    let mut model_fails_vec: Vec<(String, i64)> = model_fails.into_iter().collect();
    model_fails_vec.sort_by(|a, b| b.1.cmp(&a.1));
    for (key, total) in model_fails_vec.iter().take(10) {
        top_failing_models.push(json!({ "model": key, "errors": total }));
    }

    Json(json!({
        "enabled": true,
        "total_models": stats.total_models,
        "with_context_window": stats.with_context_window,
        "with_vision": stats.with_vision,
        "with_pricing": stats.with_pricing,
        "synced_sources": stats.synced_sources,
        "usage_records": stats.usage_records,
        "learned_rate_limits": stats.learned_rate_limits,
        "error_total": error_total,
        "error_categories": error_categories,
        "top_failing_models": top_failing_models,
    }))
}

/// GET /admin/metadata/models — List all learned model metadata.
pub async fn admin_metadata_models(State(state): State<AppState>) -> Json<serde_json::Value> {
    let Some(ref meta) = state.model_meta else {
        return Json(json!({ "models": [], "total": 0 }));
    };

    match meta.list_models(None) {
        Ok(rows) => {
            let models: Vec<serde_json::Value> = rows
                .iter()
                .map(|m| {
                    json!({
                        "provider": m.provider,
                        "model_id": m.model_id,
                        "display_name": m.display_name,
                        "context_window": m.context_window,
                        "max_completion_tokens": m.max_completion_tokens,
                        "supports_vision": m.supports_vision,
                        "supports_tools": m.supports_tools,
                        "supports_reasoning": m.supports_reasoning,
                        "pricing_prompt": m.pricing_prompt,
                        "pricing_completion": m.pricing_completion,
                        "architecture_modality": m.architecture_modality,
                        "rpm_limit": m.rpm_limit,
                        "rpd_limit": m.rpd_limit,
                        "tpm_limit": m.tpm_limit,
                        "tpd_limit": m.tpd_limit,
                        "source": m.source,
                        "first_seen_at": m.first_seen_at,
                        "last_updated_at": m.last_updated_at,
                        "update_count": m.update_count,
                    })
                })
                .collect();
            Json(json!({ "models": models, "total": models.len() }))
        }
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

/// GET /admin/metadata/usage — Usage summary. Defaults to all known history.
#[derive(serde::Deserialize)]
pub struct AttemptsQuery {
    limit: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AttemptCategorySummary {
    pub category: String,
    pub count: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AttemptDeploymentSummary {
    pub deployment: String,
    pub provider: String,
    pub model_id: String,
    pub key_id: String,
    pub attempts: usize,
    pub failures: usize,
    pub last_error_category: Option<String>,
    pub last_http_status: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AttemptRoutingAnalysis {
    pub total_attempts: usize,
    pub successful_attempts: usize,
    pub failed_attempts: usize,
    pub fallback_attempts: usize,
    pub top_error_categories: Vec<AttemptCategorySummary>,
    pub hot_deployments: Vec<AttemptDeploymentSummary>,
    pub recommendations: Vec<String>,
}

#[derive(serde::Deserialize)]
pub struct AttemptAnalysisQuery {
    limit: Option<i64>,
    #[serde(default)]
    use_model: bool,
    model: Option<String>,
}

pub fn summarize_attempts_for_routing(
    attempts: &[crate::metadata::RequestAttemptRow],
) -> AttemptRoutingAnalysis {
    let mut category_counts: HashMap<String, usize> = HashMap::new();
    let mut deployments: HashMap<String, AttemptDeploymentSummary> = HashMap::new();
    let mut provider_model_errors: HashMap<(String, String, String), HashSet<String>> =
        HashMap::new();
    let mut successful_attempts = 0;
    let mut fallback_attempts = 0;

    for attempt in attempts {
        if attempt.success {
            successful_attempts += 1;
        } else if let Some(category) = attempt.error_category.as_deref() {
            *category_counts.entry(category.to_string()).or_default() += 1;
            provider_model_errors
                .entry((
                    attempt.provider.clone(),
                    attempt.model_id.clone(),
                    category.to_string(),
                ))
                .or_default()
                .insert(attempt.key_id.clone());
        }
        if attempt.fallback {
            fallback_attempts += 1;
        }

        let deployment = format!(
            "{}/{}/{}",
            attempt.provider, attempt.model_id, attempt.key_id
        );
        let entry =
            deployments
                .entry(deployment.clone())
                .or_insert_with(|| AttemptDeploymentSummary {
                    deployment,
                    provider: attempt.provider.clone(),
                    model_id: attempt.model_id.clone(),
                    key_id: attempt.key_id.clone(),
                    attempts: 0,
                    failures: 0,
                    last_error_category: None,
                    last_http_status: None,
                });
        entry.attempts += 1;
        if !attempt.success {
            entry.failures += 1;
            entry.last_error_category = attempt.error_category.clone();
            entry.last_http_status = attempt.http_status;
        }
    }

    let mut top_error_categories: Vec<_> = category_counts
        .into_iter()
        .map(|(category, count)| AttemptCategorySummary { category, count })
        .collect();
    top_error_categories.sort_by(|a, b| b.count.cmp(&a.count).then(a.category.cmp(&b.category)));

    let mut hot_deployments: Vec<_> = deployments
        .into_values()
        .filter(|deployment| deployment.failures > 0)
        .collect();
    hot_deployments.sort_by(|a, b| {
        b.failures
            .cmp(&a.failures)
            .then(b.attempts.cmp(&a.attempts))
            .then(a.deployment.cmp(&b.deployment))
    });
    hot_deployments.truncate(10);

    let mut recommendations = Vec::new();
    let mut saturated: Vec<_> = provider_model_errors
        .into_iter()
        .filter(|((_, _, category), key_ids)| category == "rate_limited" && key_ids.len() >= 2)
        .collect();
    saturated.sort_by(|a, b| {
        b.1.len()
            .cmp(&a.1.len())
            .then(a.0.0.cmp(&b.0.0))
            .then(a.0.1.cmp(&b.0.1))
    });
    for ((provider, model_id, _category), key_ids) in saturated.into_iter().take(5) {
        recommendations.push(format!(
            "{provider}/{model_id} has {} independent account keys rate-limited in the sample; switch provider/model family first, then add more independent accounts if this model must stay primary",
            key_ids.len()
        ));
    }
    for deployment in &hot_deployments {
        match deployment.last_error_category.as_deref() {
            Some("rate_limited") => recommendations.push(format!(
                "{} should stay in cooldown/probing until a successful half-open request proves recovery",
                deployment.deployment
            )),
            Some("upstream_error" | "malformed_stream" | "empty_response") => {
                recommendations.push(format!(
                    "{} should be deprioritized and retried only after healthier deployments",
                    deployment.deployment
                ));
            }
            Some("region_forbidden" | "model_forbidden") => recommendations.push(format!(
                "{} should be treated as model/provider access restricted, not generic key exhaustion",
                deployment.deployment
            )),
            Some("auth_failed") => recommendations.push(format!(
                "{} should be disabled until credentials are manually fixed",
                deployment.deployment
            )),
            _ => recommendations.push(format!(
                "{} needs more evidence before changing global routing",
                deployment.deployment
            )),
        }
    }
    if recommendations.is_empty() {
        recommendations.push(
            "No failing deployments in the sampled attempts; keep routing by quota headroom and latency"
                .to_string(),
        );
    }

    AttemptRoutingAnalysis {
        total_attempts: attempts.len(),
        successful_attempts,
        failed_attempts: attempts.len().saturating_sub(successful_attempts),
        fallback_attempts,
        top_error_categories,
        hot_deployments,
        recommendations,
    }
}

pub fn build_attempt_analysis_prompt(
    attempts: &[crate::metadata::RequestAttemptRow],
    limit: i64,
) -> String {
    let mut lines = vec![
        "You are analyzing free LLM gateway routing logs.".to_string(),
        "This diagnostic model call is a real upstream request and consumes the same key/provider quota budget as user traffic.".to_string(),
        "Keys below are stable fingerprints, not raw secrets.".to_string(),
        format!("Analyze the newest {limit} routing attempts and recommend cooldown, probing, provider preference, and model fallback changes."),
        "Return concise JSON with: findings, likely_root_causes, routing_actions, confidence.".to_string(),
        String::new(),
    ];
    for attempt in attempts.iter().take(limit as usize) {
        lines.push(format!(
            "request_id={} attempt={} provider={} model={} key_id={} success={} category={} status={} fallback={} cooldown_s={}",
            attempt.request_id,
            attempt.attempt_index,
            attempt.provider,
            attempt.model_id,
            attempt.key_id,
            attempt.success,
            attempt.error_category.as_deref().unwrap_or("success"),
            attempt
                .http_status
                .map(|status| status.to_string())
                .unwrap_or_else(|| "none".to_string()),
            attempt.fallback,
            attempt
                .cooldown_seconds
                .map(|seconds| seconds.to_string())
                .unwrap_or_else(|| "none".to_string()),
        ));
    }
    lines.join("\n")
}

/// GET /admin/metadata/attempts - Recent structured routing attempts.
pub async fn admin_metadata_attempts(
    State(state): State<AppState>,
    Query(query): Query<AttemptsQuery>,
) -> Json<serde_json::Value> {
    let Some(ref meta) = state.model_meta else {
        return Json(json!({ "attempts": [], "total": 0 }));
    };

    let limit = query.limit.unwrap_or(100).clamp(1, 1000);
    match meta.get_recent_attempts(limit) {
        Ok(attempts) => Json(json!({
            "attempts": attempts,
            "total": attempts.len(),
            "limit": limit,
        })),
        Err(e) => Json(json!({ "error": e.to_string(), "attempts": [], "total": 0 })),
    }
}

/// GET /admin/metadata/attempts/analyze - Analyze recent routing attempts locally or with a model.
pub async fn admin_metadata_attempts_analyze(
    State(state): State<AppState>,
    Query(query): Query<AttemptAnalysisQuery>,
) -> Json<serde_json::Value> {
    let Some(ref meta) = state.model_meta else {
        return Json(json!({
            "analysis": summarize_attempts_for_routing(&[]),
            "attempts": [],
            "total": 0,
            "model_analysis": null,
        }));
    };

    let limit = query.limit.unwrap_or(100).clamp(1, 200);
    let attempts = match meta.get_recent_attempts(limit) {
        Ok(attempts) => attempts,
        Err(error) => {
            return Json(json!({
                "error": error.to_string(),
                "analysis": summarize_attempts_for_routing(&[]),
                "attempts": [],
                "total": 0,
                "model_analysis": null,
            }));
        }
    };
    let analysis = summarize_attempts_for_routing(&attempts);

    if !query.use_model {
        return Json(json!({
            "analysis": analysis,
            "attempts": attempts,
            "total": attempts.len(),
            "limit": limit,
            "model_analysis": null,
            "model_call_costs_quota": false,
        }));
    }

    let model = query.model.unwrap_or_else(|| "chat".to_string());
    let prompt = build_attempt_analysis_prompt(&attempts, limit);
    let request = crate::models::ChatCompletionRequest {
        model: model.clone(),
        messages: vec![crate::models::ChatMessage {
            role: "user".to_string(),
            content: serde_json::Value::String(prompt),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            extra: serde_json::Map::new(),
        }],
        temperature: Some(0.1),
        top_p: None,
        n: None,
        stream: Some(false),
        stop: None,
        max_tokens: Some(700),
        presence_penalty: None,
        frequency_penalty: None,
        user: Some("admin-log-analysis".to_string()),
        request_id: Some(format!("admin-log-analysis-{}", uuid::Uuid::new_v4())),
        agent_name: Some("admin".to_string()),
        extra: serde_json::Map::new(),
    };

    match state.router.chat(&request).await {
        Ok(response) => Json(json!({
            "analysis": analysis,
            "attempts": attempts,
            "total": attempts.len(),
            "limit": limit,
            "model_analysis": crate::models::content_to_text(
                &response.body["choices"][0]["message"]["content"]
            ),
            "model": model,
            "model_call_costs_quota": true,
        })),
        Err(error) => Json(json!({
            "analysis": analysis,
            "attempts": attempts,
            "total": attempts.len(),
            "limit": limit,
            "model_analysis": null,
            "model": model,
            "model_call_costs_quota": true,
            "model_error": error.to_string(),
        })),
    }
}

/// GET /admin/metadata/deployments - Provider/model/key health learned from attempts.
pub async fn admin_metadata_deployments(State(state): State<AppState>) -> Json<serde_json::Value> {
    let Some(ref meta) = state.model_meta else {
        return Json(json!({ "deployments": [], "total": 0 }));
    };

    match meta.get_deployment_states() {
        Ok(deployments) => Json(json!({
            "deployments": deployments,
            "total": deployments.len(),
        })),
        Err(e) => Json(json!({ "error": e.to_string(), "deployments": [], "total": 0 })),
    }
}

pub async fn admin_metadata_usage(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let Some(ref meta) = state.model_meta else {
        return Json(json!({ "usage": [], "total": 0 }));
    };

    let days = query
        .get("days")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);

    match meta.get_usage_summary(days) {
        Ok(rows) => {
            let total_requests: i64 = rows.iter().map(|u| u.total_requests).sum();
            let total_prompt_tokens: i64 = rows.iter().map(|u| u.total_prompt_tokens).sum();
            let total_completion_tokens: i64 = rows.iter().map(|u| u.total_completion_tokens).sum();
            let reported_prompt_tokens: i64 = rows.iter().map(|u| u.reported_prompt_tokens).sum();
            let reported_completion_tokens: i64 =
                rows.iter().map(|u| u.reported_completion_tokens).sum();
            let estimated_prompt_tokens: i64 = rows.iter().map(|u| u.estimated_prompt_tokens).sum();
            let estimated_completion_tokens: i64 =
                rows.iter().map(|u| u.estimated_completion_tokens).sum();
            let total_success: i64 = rows.iter().map(|u| u.total_success).sum();
            let total_errors: i64 = rows.iter().map(|u| u.total_errors).sum();
            let token_reported_requests: i64 = rows.iter().map(|u| u.token_reported_requests).sum();
            let token_estimated_requests = total_requests.saturating_sub(token_reported_requests);
            let usage: Vec<serde_json::Value> = rows
                .iter()
                .map(|u| {
                    let token_reporting_coverage = if u.total_requests > 0 {
                        Some(u.token_reported_requests as f64 / u.total_requests as f64)
                    } else {
                        None
                    };
                    json!({
                        "provider": u.provider,
                        "model_id": u.model_id,
                        "total_requests": u.total_requests,
                        "total_prompt_tokens": u.total_prompt_tokens,
                        "total_completion_tokens": u.total_completion_tokens,
                        "reported_prompt_tokens": u.reported_prompt_tokens,
                        "reported_completion_tokens": u.reported_completion_tokens,
                        "reported_tokens": u.reported_prompt_tokens + u.reported_completion_tokens,
                        "estimated_prompt_tokens": u.estimated_prompt_tokens,
                        "estimated_completion_tokens": u.estimated_completion_tokens,
                        "estimated_tokens": u.estimated_prompt_tokens + u.estimated_completion_tokens,
                        "token_reported_requests": u.token_reported_requests,
                        "token_estimated_requests": u.total_requests.saturating_sub(u.token_reported_requests),
                        "token_reporting_coverage": token_reporting_coverage,
                        "total_success": u.total_success,
                        "total_errors": u.total_errors,
                        "last_used_at": u.last_used_at,
                    })
                })
                .collect();
            let lifetime = meta.get_usage_lifetime().ok();
            Json(json!({
                "usage": usage,
                "total": usage.len(),
                "lifetime": lifetime,
                "summary": {
                    "window_days": if days > 0 { serde_json::Value::from(days) } else { serde_json::Value::Null },
                    "total_requests": total_requests,
                    "total_prompt_tokens": total_prompt_tokens,
                    "total_completion_tokens": total_completion_tokens,
                    "total_tokens": total_prompt_tokens + total_completion_tokens,
                    "reported_prompt_tokens": reported_prompt_tokens,
                    "reported_completion_tokens": reported_completion_tokens,
                    "reported_tokens": reported_prompt_tokens + reported_completion_tokens,
                    "estimated_prompt_tokens": estimated_prompt_tokens,
                    "estimated_completion_tokens": estimated_completion_tokens,
                    "estimated_tokens": estimated_prompt_tokens + estimated_completion_tokens,
                    "total_success": total_success,
                    "total_errors": total_errors,
                    "token_reported_requests": token_reported_requests,
                    "token_estimated_requests": token_estimated_requests,
                    "token_reporting_coverage": if total_requests > 0 { serde_json::Value::from(token_reported_requests as f64 / total_requests as f64) } else { serde_json::Value::Null },
                    "token_source_note": "reported_tokens come from upstream usage fields; estimated_tokens are local approximations",
                }
            }))
        }
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

/// GET /admin/metadata/tasks — Adaptive routing task performance summary.
/// GET /admin/metadata/usage/daily — Dense daily usage buckets.
pub async fn admin_metadata_usage_daily(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let Some(ref meta) = state.model_meta else {
        return Json(json!({ "days": [], "total": 0 }));
    };

    let days = query
        .get("days")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(7)
        .clamp(1, 366);

    match meta.get_usage_daily_summary(days) {
        Ok(rows) => Json(json!({
            "days": rows,
            "total": rows.len(),
            "window_days": days,
        })),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

/// GET /admin/metadata/usage/hourly — Dense hourly usage buckets.
pub async fn admin_metadata_usage_hourly(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let Some(ref meta) = state.model_meta else {
        return Json(json!({ "hours": [], "total": 0 }));
    };

    let hours = query
        .get("hours")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(24)
        .clamp(1, 24 * 366);

    match meta.get_usage_hourly_summary(hours) {
        Ok(rows) => Json(json!({
            "hours": rows,
            "total": rows.len(),
            "window_hours": hours,
        })),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

/// GET /admin/metadata/usage/lifetime — All-time aggregate usage totals.
pub async fn admin_metadata_usage_lifetime(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    let Some(ref meta) = state.model_meta else {
        return Json(json!({ "lifetime": null }));
    };

    match meta.get_usage_lifetime() {
        Ok(lifetime) => Json(json!({ "lifetime": lifetime })),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

pub async fn admin_metadata_tasks(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let Some(ref meta) = state.model_meta else {
        return Json(json!({ "tasks": [], "total": 0 }));
    };

    let days = query
        .get("days")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(state.config.adaptive_routing.learning_window_days);

    match meta.get_task_stats_summary(days) {
        Ok(rows) => {
            let tasks: Vec<serde_json::Value> = rows
                .iter()
                .map(|row| {
                    let success_rate = if row.request_count > 0 {
                        Some(row.success_count as f64 / row.request_count as f64)
                    } else {
                        None
                    };
                    let avg_latency_ms = if row.request_count > 0 {
                        Some(row.total_latency_ms / row.request_count)
                    } else {
                        None
                    };
                    json!({
                        "provider": row.provider,
                        "model_id": row.model_id,
                        "agent": row.agent,
                        "task_kind": row.task_kind,
                        "request_count": row.request_count,
                        "success_count": row.success_count,
                        "error_count": row.error_count,
                        "success_rate": success_rate,
                        "avg_latency_ms": avg_latency_ms,
                        "prompt_tokens": row.prompt_tokens,
                        "completion_tokens": row.completion_tokens,
                        "last_used_at": row.last_used_at,
                    })
                })
                .collect();
            Json(json!({
                "tasks": tasks,
                "total": tasks.len(),
                "window_days": days,
            }))
        }
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

/// GET /admin/metadata/capabilities — Learned model capability observations.
pub async fn admin_metadata_capabilities(State(state): State<AppState>) -> Json<serde_json::Value> {
    let Some(ref meta) = state.model_meta else {
        return Json(json!({ "capabilities": [], "total": 0 }));
    };

    match meta.get_capability_observation_summary() {
        Ok(rows) => {
            let capabilities: Vec<serde_json::Value> = rows
                .iter()
                .map(|row| {
                    json!({
                        "provider": row.provider,
                        "model_id": row.model_id,
                        "capability": row.capability,
                        "outcome": row.outcome,
                        "count": row.count,
                        "last_observed_at": row.last_observed_at,
                    })
                })
                .collect();
            Json(json!({ "capabilities": capabilities, "total": capabilities.len() }))
        }
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

/// GET /admin/routing/adaptive — Explain adaptive routing for a synthetic request.
pub async fn admin_adaptive_routing_diagnostics(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let model = query
        .get("model")
        .cloned()
        .unwrap_or_else(|| "auto".to_string());
    let content = query
        .get("q")
        .cloned()
        .unwrap_or_else(|| "diagnose adaptive route".to_string());
    let agent = query.get("agent").cloned();
    let scope = if let Some(provider) = query.get("provider") {
        crate::adaptive::AdaptiveScope::Provider(provider.clone())
    } else if let Some(group) = query.get("group") {
        crate::adaptive::AdaptiveScope::ProviderGroup(group.clone())
    } else if let Some(agent) = agent.clone() {
        crate::adaptive::AdaptiveScope::Agent(agent)
    } else {
        crate::adaptive::AdaptiveScope::Auto
    };

    let request = crate::models::ChatCompletionRequest {
        model,
        messages: vec![crate::models::ChatMessage {
            role: "user".into(),
            content: serde_json::Value::String(content),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            extra: serde_json::Map::new(),
        }],
        temperature: None,
        top_p: None,
        n: None,
        stream: Some(false),
        stop: None,
        max_tokens: None,
        presence_penalty: None,
        frequency_penalty: None,
        user: None,
        request_id: None,
        agent_name: agent,
        extra: serde_json::Map::new(),
    };

    match crate::adaptive::routing_diagnostics(&state, &scope, &request) {
        Ok(diagnostics) => Json(json!({ "success": true, "diagnostics": diagnostics })),
        Err(error) => Json(json!({ "success": false, "error": error.to_string() })),
    }
}

/// GET /admin/routing/groups — List adaptive provider groups visible to users.
pub async fn admin_adaptive_routing_groups(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    let groups = crate::adaptive::routing_groups_summary(&state);
    Json(json!({
        "enabled": state.config.adaptive_routing.enabled,
        "groups": groups,
        "total": groups.len(),
    }))
}

/// GET /admin/routing/routes — List adaptive OpenAI-compatible route prefixes.
pub async fn admin_adaptive_routing_routes(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    let routes = crate::adaptive::routing_routes_summary(&state);
    Json(json!({
        "enabled": state.config.adaptive_routing.enabled,
        "routes": routes,
        "total": routes.len(),
    }))
}

/// GET /admin/metadata/errors — Error summary across all models (last 30 days).
pub async fn admin_metadata_errors(State(state): State<AppState>) -> Json<serde_json::Value> {
    let Some(ref meta) = state.model_meta else {
        return Json(json!({ "errors": [], "total": 0 }));
    };

    match meta.get_error_summary(30) {
        Ok(rows) => {
            let errors: Vec<serde_json::Value> = rows
                .iter()
                .map(|e| {
                    json!({
                        "provider": e.provider,
                        "model_id": e.model_id,
                        "category": e.category,
                        "total": e.total,
                    })
                })
                .collect();
            Json(json!({ "errors": errors, "total": errors.len() }))
        }
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

/// GET /admin/metadata/sync — Sync status from public sources.
pub async fn admin_metadata_sync_status(State(state): State<AppState>) -> Json<serde_json::Value> {
    let Some(ref meta) = state.model_meta else {
        return Json(json!({ "sources": [], "total": 0 }));
    };

    match meta.get_sync_status() {
        Ok(rows) => {
            let sources: Vec<serde_json::Value> = rows
                .iter()
                .map(|s| {
                    json!({
                        "source_name": s.source_name,
                        "last_sync_at": s.last_sync_at,
                        "items_found": s.items_found,
                        "items_updated": s.items_updated,
                        "error_message": s.error_message,
                    })
                })
                .collect();
            Json(json!({ "sources": sources, "total": sources.len() }))
        }
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_attempt_analysis_prompt, dedupe_enabled_models, is_validation_model_mismatch,
        key_rate_usage_json, record_admin_validation_attempt, summarize_attempts_for_routing,
        validation_error_should_update_key_state, validation_failure_status,
    };
    use crate::config::KeyTier;
    use crate::error::GatewayError;
    use crate::metadata::RequestAttemptRow;
    use crate::models::{KeyState, KeyStatus};
    use std::collections::HashSet;

    #[test]
    fn rate_limited_key_usage_display_is_exhausted_even_when_counters_are_low() {
        let now = chrono::Utc::now().timestamp() as u64;
        let mut key = KeyState::with_tier("limited-key".to_string(), KeyTier::Free);
        key.status = KeyStatus::RateLimited;
        key.rpd_limit = Some(100);
        key.rpd_count = 8;
        key.rpd_window_start = now / 86400;

        let usage = key_rate_usage_json(&key);

        assert_eq!(usage["blocked_by_status"], true);
        assert_eq!(usage["exhausted"], true);
        assert_eq!(usage["counter_exhausted"], false);
        assert_eq!(usage["display_percent"].as_f64(), Some(100.0));
        assert_eq!(usage["max_percent"].as_f64(), Some(8.0));
        assert_eq!(usage["headroom_percent"].as_f64(), Some(0.0));
    }

    #[test]
    fn region_unavailable_403_is_model_candidate_mismatch() {
        let error =
            GatewayError::http_error(403, "This model is not available in your region.", None);

        assert!(is_validation_model_mismatch(&error));
    }

    #[test]
    fn validation_candidates_are_deduped_and_skip_disabled_models() {
        let disabled = HashSet::from(["bad-model".to_string()]);
        let candidates = dedupe_enabled_models(
            vec![
                "bad-model".into(),
                "working-model".into(),
                "working-model".into(),
                "fallback-model".into(),
            ],
            &disabled,
        );

        assert_eq!(
            candidates,
            vec!["working-model".to_string(), "fallback-model".to_string()]
        );
    }

    #[test]
    fn validation_transient_errors_are_inconclusive_not_key_state_updates() {
        let upstream = GatewayError::UpstreamError("error decoding response body".into());
        let timeout = GatewayError::Timeout("validation timed out".into());
        let http_500 = GatewayError::http_error(500, "provider failed", None);

        assert!(!validation_error_should_update_key_state(&upstream));
        assert!(!validation_error_should_update_key_state(&timeout));
        assert!(!validation_error_should_update_key_state(&http_500));
        assert_eq!(validation_failure_status(&upstream), "inconclusive");
    }

    #[test]
    fn validation_confirmed_auth_or_rate_limit_updates_key_state() {
        let auth = GatewayError::http_error(401, "invalid api key", None);
        let forbidden = GatewayError::http_error(403, "invalid api key", None);
        let limited = GatewayError::http_error(429, "rate limited", Some(60));

        assert!(validation_error_should_update_key_state(&auth));
        assert!(validation_error_should_update_key_state(&forbidden));
        assert!(validation_error_should_update_key_state(&limited));
        assert_eq!(
            validation_failure_status(&limited),
            "key_limited_or_invalid"
        );
    }

    #[test]
    fn admin_validation_attempts_update_deployment_state() {
        let path = std::env::temp_dir().join(format!(
            "free-agent-gateway-admin-validation-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = crate::metadata::ModelMetaStore::open(&path).unwrap();
        let meta = Some(store.clone());

        record_admin_validation_attempt(
            &meta,
            "validate-1",
            1,
            "openrouter",
            "google/gemma:free",
            "sk-test-a",
            Ok(200),
            false,
        );
        record_admin_validation_attempt(
            &meta,
            "validate-1",
            2,
            "openrouter",
            "restricted-model",
            "sk-test-a",
            Err(&GatewayError::http_error(
                403,
                "This model is not available in your region.",
                None,
            )),
            true,
        );

        let attempts = store.get_recent_attempts(10).unwrap();
        assert_eq!(attempts.len(), 2);
        assert!(attempts.iter().any(|attempt| attempt.success));
        assert!(attempts.iter().any(|attempt| {
            attempt.error_category.as_deref() == Some("region_forbidden")
                && attempt.model_id == "restricted-model"
        }));

        let states = store.get_deployment_states().unwrap();
        assert!(states.iter().any(|state| {
            state.model_id == "google/gemma:free"
                && state.success_count == 1
                && state.consecutive_failures == 0
        }));
        assert!(states.iter().any(|state| {
            state.model_id == "restricted-model"
                && state.last_error_category.as_deref() == Some("region_forbidden")
                && state.consecutive_failures == 1
        }));

        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn attempt_analysis_identifies_rate_limited_and_flaky_deployments() {
        let attempts = vec![
            attempt_row(
                "req-1",
                1,
                "openrouter",
                "model-a",
                "key-a",
                false,
                "rate_limited",
            ),
            attempt_row(
                "req-2",
                1,
                "openrouter",
                "model-a",
                "key-a",
                false,
                "rate_limited",
            ),
            attempt_row(
                "req-3",
                1,
                "nvidia",
                "model-b",
                "key-b",
                false,
                "upstream_error",
            ),
            attempt_row(
                "req-4",
                1,
                "nvidia",
                "model-b",
                "key-b",
                false,
                "upstream_error",
            ),
            attempt_row("req-5", 1, "opencode", "model-c", "key-c", true, "success"),
        ];

        let analysis = summarize_attempts_for_routing(&attempts);

        assert_eq!(analysis.total_attempts, 5);
        assert_eq!(analysis.failed_attempts, 4);
        assert_eq!(analysis.top_error_categories[0].category, "rate_limited");
        assert!(analysis.recommendations.iter().any(|item| {
            item.contains("openrouter/model-a/key-a") && item.contains("cooldown")
        }));
        assert!(analysis.recommendations.iter().any(|item| {
            item.contains("nvidia/model-b/key-b") && item.contains("deprioritize")
        }));
    }

    #[test]
    fn attempt_analysis_detects_account_pool_saturation_for_provider_model() {
        let attempts = vec![
            attempt_row(
                "req-1",
                1,
                "openrouter",
                "model-a",
                "key-a",
                false,
                "rate_limited",
            ),
            attempt_row(
                "req-2",
                1,
                "openrouter",
                "model-a",
                "key-b",
                false,
                "rate_limited",
            ),
            attempt_row(
                "req-3",
                1,
                "openrouter",
                "model-a",
                "key-c",
                false,
                "rate_limited",
            ),
            attempt_row("req-4", 1, "opencode", "model-a", "key-d", true, "success"),
        ];

        let analysis = summarize_attempts_for_routing(&attempts);

        assert!(analysis.recommendations.iter().any(|item| {
            item.contains("openrouter/model-a")
                && item.contains("3 independent account")
                && item.contains("switch provider")
        }));
    }

    #[test]
    fn attempt_analysis_prompt_is_redacted_and_mentions_real_quota_cost() {
        let attempts = vec![attempt_row(
            "req-1",
            1,
            "openrouter",
            "model-a",
            "key-fingerprint-only",
            false,
            "rate_limited",
        )];
        let prompt = build_attempt_analysis_prompt(&attempts, 1);

        assert!(prompt.contains("This diagnostic model call is a real upstream request"));
        assert!(prompt.contains("key-fingerprint-only"));
        assert!(!prompt.contains("sk-"));
        assert!(prompt.contains("openrouter"));
    }

    fn attempt_row(
        request_id: &str,
        attempt_index: i64,
        provider: &str,
        model_id: &str,
        key_id: &str,
        success: bool,
        category: &str,
    ) -> RequestAttemptRow {
        RequestAttemptRow {
            id: attempt_index,
            request_id: request_id.to_string(),
            attempt_index,
            provider: provider.to_string(),
            model_id: model_id.to_string(),
            key_id: key_id.to_string(),
            success,
            error_category: (!success).then(|| category.to_string()),
            http_status: (!success).then_some(if category == "rate_limited" { 429 } else { 500 }),
            error_message: (!success).then(|| category.to_string()),
            cooldown_seconds: None,
            fallback: false,
            created_at: 1_700_000_000 + attempt_index,
        }
    }
}
