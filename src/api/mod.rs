pub mod adaptive;
/// API handlers: OpenAI-compatible endpoints + admin/management endpoints.
///
/// Endpoints:
/// - GET  /v1/models              — List models (OpenAI-compatible)
/// - POST /v1/chat/completions    — Chat completion (OpenAI-compatible)
/// - GET  /health                 — Health check
/// - GET  /status                 — Gateway status
/// - GET  /metrics                — Detailed JSON metrics
/// - GET  /metrics/prometheus     — Prometheus text metrics
/// - GET  /providers              — Provider status list
/// - GET  /admin                  — Admin dashboard HTML
/// - GET  /admin/config           — Get configuration
/// - PUT  /admin/config           — Update configuration
/// - GET  /admin/status           — Full gateway status
/// - POST /admin/providers/:name/refresh — Refresh provider
/// - POST /admin/providers/:name/test    — Test provider
/// - POST /admin/providers/:name/keys/:key_id/restore — Restore key
/// - GET  /admin/events           — SSE real-time events
pub mod admin;
pub mod admin_html;
pub mod chat;
pub mod compat;
pub mod models;
pub mod status;

pub use adaptive::{
    adaptive_agent_chat_completions, adaptive_agent_models, adaptive_chat_completions,
    adaptive_models, adaptive_provider_chat_completions, adaptive_provider_group_chat_completions,
    adaptive_provider_group_models, adaptive_provider_models,
};
pub use admin::{
    admin_adaptive_routing_diagnostics, admin_adaptive_routing_groups,
    admin_adaptive_routing_routes, admin_config_get, admin_config_put, admin_events, admin_keys,
    admin_metadata_attempts, admin_metadata_attempts_analyze, admin_metadata_capabilities,
    admin_metadata_deployments, admin_metadata_errors, admin_metadata_models, admin_metadata_stats,
    admin_metadata_sync_status, admin_metadata_tasks, admin_metadata_usage,
    admin_metadata_usage_daily, admin_metadata_usage_hourly, admin_metadata_usage_lifetime,
    admin_provider_key_restore, admin_provider_key_validate, admin_provider_models_get,
    admin_provider_models_toggle, admin_provider_refresh, admin_provider_test, admin_save,
    admin_status,
};
pub use admin_html::{admin_index, admin_legacy_index, admin_usage_index};
pub use chat::chat_completions;
pub use compat::{completions, embeddings, responses};
pub use models::{admin_model_families, list_models};
pub use status::{health, metrics, metrics_prometheus, providers, status};

/// Sensitive header names that must never appear in logs.
pub const SECURITY_REDACT_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "set-cookie",
    "api-key",
    "apikey",
    "x-api-key",
    "token",
    "x-token",
];
