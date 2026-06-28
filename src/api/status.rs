/// Status endpoints: /health, /status, /metrics, /metrics/prometheus, /providers
use axum::{
    extract::State,
    http::header,
    response::{IntoResponse, Json},
};
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

/// GET /metrics/prometheus
pub async fn metrics_prometheus(State(state): State<AppState>) -> impl IntoResponse {
    let health_states = state.health_registry.snapshot();
    let uptime = state.start_time.elapsed().as_secs();
    let total_requests = state
        .request_counter
        .load(std::sync::atomic::Ordering::Relaxed);
    let total_errors = state
        .error_counter
        .load(std::sync::atomic::Ordering::Relaxed);

    let mut body = String::new();
    push_metric_help(
        &mut body,
        "free_agent_gateway_uptime_seconds",
        "Gateway process uptime in seconds.",
        "gauge",
    );
    push_metric_line(&mut body, "free_agent_gateway_uptime_seconds", &[], uptime);

    push_metric_help(
        &mut body,
        "free_agent_gateway_requests_total",
        "Total chat completion requests accepted by the gateway.",
        "counter",
    );
    push_metric_line(
        &mut body,
        "free_agent_gateway_requests_total",
        &[],
        total_requests,
    );

    push_metric_help(
        &mut body,
        "free_agent_gateway_errors_total",
        "Total chat completion requests that ended in an error.",
        "counter",
    );
    push_metric_line(
        &mut body,
        "free_agent_gateway_errors_total",
        &[],
        total_errors,
    );

    push_metric_help(
        &mut body,
        "free_agent_gateway_provider_up",
        "Provider health status, 1 for healthy and 0 otherwise.",
        "gauge",
    );
    push_metric_help(
        &mut body,
        "free_agent_gateway_provider_latency_ms",
        "Last observed provider health-check latency in milliseconds.",
        "gauge",
    );
    push_metric_help(
        &mut body,
        "free_agent_gateway_provider_success_total",
        "Provider health-check success count.",
        "counter",
    );
    push_metric_help(
        &mut body,
        "free_agent_gateway_provider_failures_total",
        "Provider health-check failure count.",
        "counter",
    );
    push_metric_help(
        &mut body,
        "free_agent_gateway_provider_models",
        "Number of models discovered for a provider.",
        "gauge",
    );
    push_metric_help(
        &mut body,
        "free_agent_gateway_provider_keys",
        "Provider API key count by state.",
        "gauge",
    );

    for provider in health_states {
        let labels = [("provider", provider.provider.as_str())];
        push_metric_line(
            &mut body,
            "free_agent_gateway_provider_up",
            &labels,
            u64::from(provider.status == "healthy"),
        );
        push_metric_line(
            &mut body,
            "free_agent_gateway_provider_latency_ms",
            &labels,
            provider.latency_ms,
        );
        push_metric_line(
            &mut body,
            "free_agent_gateway_provider_success_total",
            &labels,
            provider.success_count,
        );
        push_metric_line(
            &mut body,
            "free_agent_gateway_provider_failures_total",
            &labels,
            provider.fail_count,
        );
        push_metric_line(
            &mut body,
            "free_agent_gateway_provider_models",
            &labels,
            provider.models_count as u64,
        );

        let available = provider.available_keys as u64;
        let total = provider.total_keys as u64;
        push_metric_line(
            &mut body,
            "free_agent_gateway_provider_keys",
            &[
                ("provider", provider.provider.as_str()),
                ("state", "available"),
            ],
            available,
        );
        push_metric_line(
            &mut body,
            "free_agent_gateway_provider_keys",
            &[
                ("provider", provider.provider.as_str()),
                ("state", "unavailable"),
            ],
            total.saturating_sub(available),
        );
        push_metric_line(
            &mut body,
            "free_agent_gateway_provider_keys",
            &[("provider", provider.provider.as_str()), ("state", "total")],
            total,
        );
    }

    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
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

fn push_metric_help(body: &mut String, name: &str, help: &str, metric_type: &str) {
    body.push_str("# HELP ");
    body.push_str(name);
    body.push(' ');
    body.push_str(help);
    body.push('\n');
    body.push_str("# TYPE ");
    body.push_str(name);
    body.push(' ');
    body.push_str(metric_type);
    body.push('\n');
}

fn push_metric_line(body: &mut String, name: &str, labels: &[(&str, &str)], value: u64) {
    body.push_str(name);
    if !labels.is_empty() {
        body.push('{');
        for (index, (key, value)) in labels.iter().enumerate() {
            if index > 0 {
                body.push(',');
            }
            body.push_str(key);
            body.push_str("=\"");
            push_escaped_label_value(body, value);
            body.push('"');
        }
        body.push('}');
    }
    body.push(' ');
    body.push_str(&value.to_string());
    body.push('\n');
}

fn push_escaped_label_value(body: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '\\' => body.push_str("\\\\"),
            '"' => body.push_str("\\\""),
            '\n' => body.push_str("\\n"),
            _ => body.push(ch),
        }
    }
}
