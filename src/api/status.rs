/// Status endpoints: /health, /status, /metrics, /providers
use axum::{extract::State, response::Json};
use serde_json::json;

use crate::AppState;
use crate::models::GatewayStatus;

/// GET /health
pub async fn health(State(state): State<AppState>) -> Json<serde_json::Value> {
    let health_states = state.health_registry.snapshot();
    let healthy_count = health_states
        .iter()
        .filter(|h| h.status == "healthy")
        .count();
    let total_count = health_states.len();

    Json(json!({
        "status": if healthy_count == total_count && total_count > 0 { "ok" } else { "degraded" },
        "version": env!("CARGO_PKG_VERSION"),
        "providers": {
            "total": total_count,
            "healthy": healthy_count,
            "unhealthy": total_count - healthy_count,
        },
        "providers_detail": health_states,
    }))
}

/// GET /status
pub async fn status(State(state): State<AppState>) -> Json<GatewayStatus> {
    let uptime = state.start_time.elapsed().as_secs();
    let health_states = state.health_registry.snapshot();

    Json(GatewayStatus {
        version: env!("CARGO_PKG_VERSION").into(),
        uptime_seconds: uptime,
        providers: health_states,
        total_requests: state
            .request_counter
            .load(std::sync::atomic::Ordering::Relaxed),
        total_errors: state
            .error_counter
            .load(std::sync::atomic::Ordering::Relaxed),
    })
}

/// GET /metrics
pub async fn metrics(State(state): State<AppState>) -> Json<serde_json::Value> {
    let health_states = state.health_registry.snapshot();
    let keyhub_snapshot = state.keyhub.snapshot();

    let mut provider_metrics = Vec::new();
    for hs in &health_states {
        let keys = keyhub_snapshot
            .iter()
            .find(|(name, _)| name == &hs.provider)
            .map(|(_, keys)| keys.clone())
            .unwrap_or_default();

        provider_metrics.push(json!({
            "provider": hs.provider,
            "status": hs.status,
            "latency_ms": hs.latency_ms,
            "success_count": hs.success_count,
            "fail_count": hs.fail_count,
            "models_count": hs.models_count,
            "keys": {
                "total": hs.total_keys,
                "available": hs.available_keys,
                "detail": keys,
            },
            "last_error": if hs.last_error.is_empty() { serde_json::Value::Null } else { json!(hs.last_error) },
        }));
    }

    Json(json!({
        "gateway": {
            "version": env!("CARGO_PKG_VERSION"),
            "uptime_seconds": state.start_time.elapsed().as_secs(),
            "routing_strategy": format!("{:?}", state.config.routing.strategy),
            "total_requests": state.request_counter.load(std::sync::atomic::Ordering::Relaxed),
            "total_errors": state.error_counter.load(std::sync::atomic::Ordering::Relaxed),
        },
        "providers": provider_metrics,
        "model_aliases": state.config.models,
        "agents": state.config.agents,
        "fallback_chain": state.config.fallback,
    }))
}

/// GET /providers
pub async fn providers(State(state): State<AppState>) -> Json<serde_json::Value> {
    let health_states = state.health_registry.snapshot();

    let mut provider_list = Vec::new();
    for hs in &health_states {
        let config = state.config.providers.get(&hs.provider);
        provider_list.push(json!({
            "name": hs.provider,
            "status": hs.status,
            "latency_ms": hs.latency_ms,
            "success_count": hs.success_count,
            "fail_count": hs.fail_count,
            "models_count": hs.models_count,
            "available_keys": hs.available_keys,
            "total_keys": hs.total_keys,
            "type": config.map(|c| format!("{:?}", c.provider_type)).unwrap_or_else(|| "unknown".into()),
            "base_url": config.map(|c| &c.base_url).map(|s| s.to_string()),
            "priority": config.map(|c| c.priority).unwrap_or(0),
            "last_error": if hs.last_error.is_empty() { serde_json::Value::Null } else { json!(hs.last_error) },
        }));
    }

    Json(json!({
        "providers": provider_list,
        "fallback_chain": state.config.fallback,
    }))
}
