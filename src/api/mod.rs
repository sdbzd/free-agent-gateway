/// API handlers: OpenAI-compatible endpoints + admin/management endpoints.
///
/// Endpoints:
/// - GET  /v1/models              — List models (OpenAI-compatible)
/// - POST /v1/chat/completions    — Chat completion (OpenAI-compatible)
/// - GET  /health                 — Health check
/// - GET  /status                 — Gateway status
/// - GET  /metrics                — Detailed metrics
/// - GET  /providers              — Provider status list
/// - GET  /admin                  — Admin dashboard HTML
/// - GET  /admin/config           — Get configuration
/// - PUT  /admin/config           — Update configuration
/// - GET  /admin/status           — Full gateway status
/// - POST /admin/providers/:name/refresh — Refresh provider
/// - POST /admin/providers/:name/test    — Test provider
/// - GET  /admin/events           — SSE real-time events
pub mod admin;
pub mod admin_html;
pub mod chat;
pub mod models;
pub mod status;

pub use admin::{
    admin_config_get, admin_config_put, admin_events, admin_keys, admin_metadata_errors,
    admin_metadata_models, admin_metadata_stats, admin_metadata_sync_status, admin_metadata_usage,
    admin_provider_models_get, admin_provider_models_toggle, admin_save, admin_status,
    admin_provider_refresh, admin_provider_test,
};
pub use admin_html::admin_index;
pub use chat::chat_completions;
pub use models::list_models;
pub use status::{health, metrics, providers, status};

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
