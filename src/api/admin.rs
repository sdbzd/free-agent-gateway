/// Admin endpoints: configuration management, provider testing, SSE events.
use std::time::Instant;

use axum::{
    extract::{Path, State},
    response::{
        sse::{Event, Sse},
        Json,
    },
};
use futures::stream::Stream;
use serde_json::json;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::AppState;

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
                if new_config.enabled != pc.enabled || new_config.base_url != pc.base_url {
                    if let Ok(provider) =
                        crate::providers::create_provider(name, &new_config)
                    {
                        state.providers.insert(name.clone(), provider);
                        state
                            .keyhub
                            .register_provider(name, new_config.keys.clone());
                    }
                }

                // Update in-memory config (use unsafe to mutate Arc'd config)
                // Instead, we recreate config with updated providers
                let mut providers_map = state.config.providers.clone();
                providers_map.insert(name.clone(), new_config.clone());
                // Note: We don't persist this to disk yet. Config reload requires restart.
                // We update the provider in the providers map for runtime behavior.

                // Broadcast config update event
                let _ = state.sse_tx.send(json!({
                    "type": "config_update",
                    "data": { "provider": name, "enabled": new_config.enabled },
                    "timestamp": chrono::Utc::now().timestamp(),
                }).to_string());
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
    let total_requests = state.request_counter.load(std::sync::atomic::Ordering::Relaxed);
    let total_errors = state.error_counter.load(std::sync::atomic::Ordering::Relaxed);

    // Build a lookup: provider_name -> key snapshots (real-time from keyhub)
    let key_snapshots: std::collections::HashMap<String, Vec<crate::models::KeyState>> = state
        .keyhub
        .snapshot()
        .into_iter()
        .map(|(name, keys)| (name.clone(), keys))
        .collect();

    // Build real-time available key count from keyhub snapshot
    use crate::models::KeyStatus;
    let real_available: std::collections::HashMap<String, usize> = key_snapshots
        .iter()
        .map(|(name, keys)| {
            let avail = keys.iter().filter(|k| k.status == KeyStatus::Available).count();
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
            "unhealthy" => { unhealthy += 1; "unhealthy" }
            "disabled"  => { unhealthy += 1; "disabled" }
            _ if real_available_keys == 0 && total_keys > 0 => { exhausted += 1; "exhausted" }
            _ if real_available_keys > 0 && real_available_keys < total_keys => { degraded += 1; "degraded" }
            _ if real_available_keys > 0 => { healthy += 1; "healthy" }
            _ => { healthy += 1; &hs.status }
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
                "rpm_limit": k.rpm_limit,
                "rpd_limit": k.rpd_limit,
                "tpm_limit": k.tpm_limit,
                "tpd_limit": k.tpd_limit,
                "rpm_count": k.rpm_count,
                "rpd_count": k.rpd_count,
                "tpm_total": k.tpm_prompt_count + k.tpm_completion_count,
                "tpd_total": k.tpd_prompt_count + k.tpd_completion_count,
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
    let check_timeout = std::time::Duration::from_secs(
        state.config.watcher.check_timeout_seconds,
    );
    let mut successful_keys = 0usize;
    let mut total_latency = 0u64;
    let mut provider_models = std::collections::BTreeSet::new();
    let mut last_error = String::new();

    for (api_key, _tier) in state.keyhub.discovery_keys(&provider_name) {
        let started = Instant::now();
        match tokio::time::timeout(check_timeout, provider.list_models(&api_key)).await {
            Ok(Ok(models)) => {
                total_latency += started.elapsed().as_millis() as u64;
                successful_keys += 1;
                provider_models.extend(models.iter().cloned());
                state.keyhub.update_models(&provider_name, &api_key, models);
            }
            Ok(Err(error)) => {
                let status = error.http_status();
                if matches!(status, 401 | 403 | 429) {
                    state.keyhub.report_failure(&provider_name, &api_key, status);
                }
                last_error = error.to_string();
                state.keyhub.record_model_error(
                    &provider_name,
                    &api_key,
                    &crate::error::sanitize_diagnostic(&last_error),
                );
            }
            Err(_) => {
                last_error = "Model discovery timed out".into();
                state.keyhub.record_model_error(&provider_name, &api_key, &last_error);
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
        state.health_registry.record_error(
            &provider_name,
            if last_error.is_empty() {
                "No configured keys"
            } else {
                &last_error
            },
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

    // Get the health check model
    let test_model = state
        .config
        .providers
        .get(&provider_name)
        .map(|pc| pc.health_check_model.clone())
        .unwrap_or_else(|| "gpt-4o-mini".to_string());

    // Create a minimal test request
    let test_request = crate::models::ChatCompletionRequest {
        model: test_model.clone(),
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
    };

    let started = Instant::now();
    let timeout_dur = std::time::Duration::from_secs(
        state
            .config
            .providers
            .get(&provider_name)
            .map(|pc| pc.timeout_seconds)
            .unwrap_or(30),
    );

    let result = tokio::time::timeout(timeout_dur, provider.chat(&api_key, test_request)).await;

    match result {
        Ok(Ok(response)) => {
            let latency = started.elapsed().as_millis() as u64;
            state
                .keyhub
                .report_success(&provider_name, &api_key, None, None);

            // Broadcast event
            let _ = state.sse_tx.send(json!({
                "type": "provider_test",
                "data": { "provider": &provider_name, "success": true, "latency_ms": latency },
                "timestamp": chrono::Utc::now().timestamp(),
            }).to_string());

            let body = &response.body;
            let model = body.get("model").and_then(|v| v.as_str()).unwrap_or(&test_model).to_string();
            let content_preview = body
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|arr| arr.first())
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|v| v.as_str())
                .map(|s| {
                    if s.len() > 100 { format!("{}...", &s[..100]) } else { s.to_string() }
                });

            Json(json!({
                "success": true,
                "provider": provider_name,
                "model": model,
                "latency_ms": latency,
                "status": response.status,
                "response_preview": content_preview,
            }))
        }
        Ok(Err(e)) => {
            let latency = started.elapsed().as_millis() as u64;
            let status = e.http_status();
            state.keyhub.report_failure(&provider_name, &api_key, status);
            Json(json!({
                "success": false,
                "provider": provider_name,
                "latency_ms": latency,
                "error": e.to_string(),
                "http_status": status,
            }))
        }
        Err(_) => {
            Json(json!({
                "success": false,
                "provider": provider_name,
                "error": "Request timed out",
                "latency_ms": started.elapsed().as_millis() as u64,
            }))
        }
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
                        "keys_healthy": 0,
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
                if key.status == crate::models::KeyStatus::Available {
                    entry["keys_healthy"] = json!(entry["keys_healthy"].as_u64().unwrap_or(0) + 1);
                }
            }
        }
    }

    let models: Vec<serde_json::Value> = model_map.into_values().collect();
    let enabled_count = models.iter().filter(|m| m["enabled"].as_bool().unwrap_or(false)).count();
    let disabled_count = models.len() - enabled_count;

    Json(json!({
        "provider": provider_name,
        "models": models,
        "total": models.len(),
        "enabled_count": enabled_count,
        "disabled_count": disabled_count,
    }))
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
    let _ = state.sse_tx.send(json!({
        "type": "model_toggle",
        "data": {
            "provider": &provider_name,
            "model": &model_id,
            "enabled": now_enabled,
        },
        "timestamp": chrono::Utc::now().timestamp(),
    }).to_string());

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
                let tpm_used = if rpm_window_active { k.tpm_prompt_count + k.tpm_completion_count } else { 0 };
                let tpd_used = if rpd_window_active { k.tpd_prompt_count + k.tpd_completion_count } else { 0 };

                let rpm_pct = k.rpm_limit.map(|lim| if lim > 0 { (rpm_used as f64 / lim as f64) * 100.0 } else { 100.0 });
                let rpd_pct = k.rpd_limit.map(|lim| if lim > 0 { (rpd_used as f64 / lim as f64) * 100.0 } else { 100.0 });
                let tpm_pct = k.tpm_limit.map(|lim| if lim > 0 { (tpm_used as f64 / lim as f64) * 100.0 } else { 100.0 });
                let tpd_pct = k.tpd_limit.map(|lim| if lim > 0 { (tpd_used as f64 / lim as f64) * 100.0 } else { 100.0 });

                json!({
                    "key_id": k.key_id,
                    "key": k.masked_key(),
                    "tier": k.tier,
                    "status": k.status,
                    "success_count": k.success_count,
                    "fail_count": k.fail_count,
                    "total_fail_count": k.total_fail_count,
                    "cooldown_until": k.cooldown_until,
                    "models": k.models,
                    "rate_limits": {
                        "rpm": { "limit": k.rpm_limit, "used": rpm_used, "percent": rpm_pct },
                        "rpd": { "limit": k.rpd_limit, "used": rpd_used, "percent": rpd_pct },
                        "tpm": { "limit": k.tpm_limit, "used": tpm_used, "percent": tpm_pct },
                        "tpd": { "limit": k.tpd_limit, "used": tpd_used, "percent": tpd_pct },
                    },
                })
            })
            .collect();
        providers.insert(provider_name, json!(key_list));
    }

    Json(json!({
        "providers": providers,
    }))
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
    let mut error_categories: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
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
        "error_total": error_total,
        "error_categories": error_categories,
        "top_failing_models": top_failing_models,
    }))
}

/// GET /admin/metadata/models — List all learned model metadata.
pub async fn admin_metadata_models(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    let Some(ref meta) = state.model_meta else {
        return Json(json!({ "models": [], "total": 0 }));
    };

    match meta.list_models(None) {
        Ok(rows) => {
            let models: Vec<serde_json::Value> = rows.iter().map(|m| json!({
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
            })).collect();
            Json(json!({ "models": models, "total": models.len() }))
        }
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

/// GET /admin/metadata/usage — Usage summary (last N days).
pub async fn admin_metadata_usage(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    let Some(ref meta) = state.model_meta else {
        return Json(json!({ "usage": [], "total": 0 }));
    };

    match meta.get_usage_summary(30) {
        Ok(rows) => {
            let usage: Vec<serde_json::Value> = rows.iter().map(|u| json!({
                "provider": u.provider,
                "model_id": u.model_id,
                "total_requests": u.total_requests,
                "total_prompt_tokens": u.total_prompt_tokens,
                "total_completion_tokens": u.total_completion_tokens,
                "total_success": u.total_success,
                "total_errors": u.total_errors,
                "last_used_at": u.last_used_at,
            })).collect();
            Json(json!({ "usage": usage, "total": usage.len() }))
        }
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

/// GET /admin/metadata/errors — Error summary across all models (last 30 days).
pub async fn admin_metadata_errors(State(state): State<AppState>) -> Json<serde_json::Value> {
    let Some(ref meta) = state.model_meta else {
        return Json(json!({ "errors": [], "total": 0 }));
    };

    match meta.get_error_summary(30) {
        Ok(rows) => {
            let errors: Vec<serde_json::Value> = rows.iter().map(|e| json!({
                "provider": e.provider,
                "model_id": e.model_id,
                "category": e.category,
                "total": e.total,
            })).collect();
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
            let sources: Vec<serde_json::Value> = rows.iter().map(|s| json!({
                "source_name": s.source_name,
                "last_sync_at": s.last_sync_at,
                "items_found": s.items_found,
                "items_updated": s.items_updated,
                "error_message": s.error_message,
            })).collect();
            Json(json!({ "sources": sources, "total": sources.len() }))
        }
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}
