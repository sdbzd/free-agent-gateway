use std::sync::Arc;
/// free-agent-gateway — Main entry point.
///
/// Single EXE deployment. No Docker, no Kubernetes, no external databases.
use std::sync::atomic::AtomicU64;
use std::time::Instant;

use axum::{
    Router as AxumRouter,
    routing::{get, post},
};
use dashmap::DashMap;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use std::collections::{HashMap, HashSet};

use parking_lot::RwLock;

use free_agent_gateway::{
    AppState,
    api::{
        self, adaptive_agent_chat_completions, adaptive_agent_models, adaptive_chat_completions,
        adaptive_models, adaptive_provider_chat_completions,
        adaptive_provider_group_chat_completions, adaptive_provider_group_models,
        adaptive_provider_models, admin_adaptive_routing_diagnostics,
        admin_adaptive_routing_groups, admin_adaptive_routing_routes, admin_config_get,
        admin_config_put, admin_events, admin_index, admin_keys, admin_legacy_index,
        admin_metadata_attempts, admin_metadata_attempts_analyze, admin_metadata_capabilities,
        admin_metadata_deployments, admin_metadata_errors, admin_metadata_models,
        admin_metadata_stats, admin_metadata_sync_status, admin_metadata_tasks,
        admin_metadata_usage, admin_metadata_usage_daily, admin_metadata_usage_hourly,
        admin_metadata_usage_lifetime, admin_model_families, admin_provider_key_restore,
        admin_provider_key_validate, admin_provider_models_get, admin_provider_models_toggle,
        admin_provider_refresh, admin_provider_test, admin_save, admin_status, admin_usage_index,
        chat_completions, completions, embeddings, health, list_models, metrics,
        metrics_prometheus, responses, status,
    },
    config::Config,
    health::HealthRegistry,
    keyhub::KeyHub,
    metadata::{ModelMetaStore, sync::SyncScheduler},
    providers::create_provider,
    rate_rules::start_openrouter_key_rule_sync,
    router::Router,
    state::PersistedState,
    watcher::Watcher,
};

/// Gateway version, embedded at compile time.
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ─── Initialize tracing ────────────────────────────────────────
    let config = Config::load("config.yaml")?;
    init_tracing(&config.server.log_level);

    tracing::info!("🦀 free-agent-gateway v{}", VERSION);
    tracing::info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    // ─── Initialize state ──────────────────────────────────────────
    let config = Arc::new(config);
    let providers: Arc<DashMap<String, _>> = Arc::new(DashMap::new());
    let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
    let health_registry = Arc::new(HealthRegistry::new());

    // ─── Load persisted state ──────────────────────────────────────
    let persisted = PersistedState::load(&config.state.state_file).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Could not load persisted state, starting fresh");
        PersistedState::new()
    });
    for (provider_name, provider_keys) in &persisted.providers {
        for key_state in &provider_keys.keys {
            tracing::info!(
                provider = %provider_name,
                key = %key_state.masked_key(),
                status = %key_state.status,
                "Restored key state"
            );
        }
    }
    let persisted_state = Arc::new(parking_lot::RwLock::new(persisted.clone()));

    // ─── Register providers ────────────────────────────────────────
    for (name, provider_config) in &config.providers {
        if !provider_config.enabled {
            tracing::info!(provider = %name, "Provider disabled, skipping");
            continue;
        }

        match create_provider(name, provider_config) {
            Ok(provider) => {
                tracing::info!(
                    provider = %name,
                    r#type = %provider_config.provider_type,
                    base_url = %provider_config.base_url,
                    keys_count = provider_config.keys.len(),
                    "Provider registered"
                );
                providers.insert(name.clone(), provider);
                keyhub.register_provider(name, provider_config.keys.clone());
                if let Some(provider_state) = persisted.providers.get(name) {
                    keyhub.restore_provider_states(name, &provider_state.keys);
                }
                health_registry.register(name, provider_config);
            }
            Err(e) => {
                tracing::error!(provider = %name, error = %e, "Failed to create provider");
            }
        }
    }

    // ─── Load disabled models from persisted state ────────────────────
    let disabled_models: HashMap<String, HashSet<String>> = persisted
        .disabled_models
        .iter()
        .map(|(provider, models)| (provider.clone(), models.iter().cloned().collect()))
        .collect();
    tracing::info!(
        "Loaded disabled models: {:?}",
        disabled_models
            .iter()
            .map(|(p, ms)| format!("{}={}", p, ms.len()))
            .collect::<Vec<_>>()
    );
    let disabled_models = Arc::new(RwLock::new(disabled_models));

    // ─── Initialize metadata database ───────────────────────────────
    let meta_db_path = format!("{}.db", config.state.state_file.trim_end_matches(".json"));
    let model_meta = match ModelMetaStore::open(&meta_db_path) {
        Ok(store) => {
            tracing::info!("📚 Model metadata DB opened: {meta_db_path}");
            Some(store)
        }
        Err(e) => {
            tracing::warn!("Could not open model metadata DB: {e}");
            None
        }
    };

    // ─── Create SSE broadcast channel ───────────────────────────────
    let (sse_tx, _) = tokio::sync::broadcast::channel::<String>(256);

    // ─── Initialize router ─────────────────────────────────────────
    let router = Arc::new(Router::new(
        config.clone(),
        providers.clone(),
        keyhub.clone(),
        disabled_models.clone(),
        model_meta.clone(),
    ));

    // ─── Build app state ──────────────────────────────────────────
    let state = AppState {
        config: config.clone(),
        state: persisted_state,
        http_client: reqwest::Client::new(),
        providers: providers.clone(),
        keyhub: keyhub.clone(),
        router,
        health_registry: health_registry.clone(),
        request_counter: Arc::new(AtomicU64::new(0)),
        error_counter: Arc::new(AtomicU64::new(0)),
        start_time: Instant::now(),
        sse_tx,
        disabled_models,
        model_meta: model_meta.clone(),
        _sync_handle: Arc::new(parking_lot::Mutex::new(None)),
    };

    // ─── Start metadata sync scheduler ───────────────────────────────
    if let Some(ref meta_store) = model_meta {
        let sync_scheduler = Arc::new(SyncScheduler::new(
            meta_store.clone(),
            reqwest::Client::new(),
        ));
        sync_scheduler.start_background_sync();
    }

    let _openrouter_rule_sync =
        start_openrouter_key_rule_sync(keyhub.clone(), reqwest::Client::new());

    // ─── Spawn watcher background task ─────────────────────────────
    let watcher = Arc::new(Watcher::new(
        config.clone(),
        providers.clone(),
        keyhub.clone(),
        health_registry.clone(),
    ));
    let watcher_task = watcher.clone();
    tokio::spawn(async move {
        watcher_task.run().await;
    });

    // ─── Initial model discovery on startup ─────────────────────────
    tracing::info!("Running initial model discovery...");
    watcher.check_all().await;
    tracing::info!("Initial model discovery complete");

    // ─── Spawn in-memory state sync task (NO disk I/O) ───────────────
    // The background sync only keeps the in-memory PersistedState up-to-date.
    // Disk writes happen only on explicit POST /admin/save.
    let save_config = config.clone();
    let save_keyhub = keyhub.clone();
    let save_state = state.state.clone();
    let save_disabled = state.disabled_models.clone();
    tokio::spawn(async move {
        let interval = std::time::Duration::from_secs(save_config.state.save_interval_seconds);
        loop {
            tokio::time::sleep(interval).await;

            let keyhub_snapshot = save_keyhub.snapshot();

            let mut persisted = PersistedState::new();
            for (provider, keys) in keyhub_snapshot {
                persisted.providers.insert(
                    provider,
                    free_agent_gateway::state::ProviderKeyState { keys },
                );
            }

            // Sync disabled models
            {
                let dm = save_disabled.read();
                persisted.disabled_models = dm
                    .iter()
                    .map(|(provider, models)| (provider.clone(), models.iter().cloned().collect()))
                    .collect();
            }

            // Update in-memory cached state only — no disk write
            {
                let mut guard = save_state.write();
                *guard = persisted.clone();
            }
        }
    });

    // ─── Build HTTP routes ─────────────────────────────────────────
    let cors = build_cors_layer(&config);

    let app = AxumRouter::new()
        // OpenAI-compatible routes
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/completions", post(completions))
        .route("/v1/responses", post(responses))
        .route("/v1/embeddings", post(embeddings))
        .route("/v1/models", get(list_models))
        // Adaptive OpenAI-compatible route namespaces
        .route("/auto/v1/chat/completions", post(adaptive_chat_completions))
        .route("/auto/v1/models", get(adaptive_models))
        .route(
            "/agents/{agent}/v1/chat/completions",
            post(adaptive_agent_chat_completions),
        )
        .route("/agents/{agent}/v1/models", get(adaptive_agent_models))
        .route(
            "/provider-groups/{group}/v1/chat/completions",
            post(adaptive_provider_group_chat_completions),
        )
        .route(
            "/provider-groups/{group}/v1/models",
            get(adaptive_provider_group_models),
        )
        .route(
            "/{provider}/v1/chat/completions",
            post(adaptive_provider_chat_completions),
        )
        .route("/{provider}/v1/models", get(adaptive_provider_models))
        // Admin/management routes
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/metrics", get(metrics))
        .route("/metrics/prometheus", get(metrics_prometheus))
        .route("/providers", get(api::providers))
        // Admin dashboard & management routes
        .route("/admin", get(admin_index))
        .route("/admin/usage", get(admin_usage_index))
        .route("/admin/legacy", get(admin_legacy_index))
        .route("/admin/config", get(admin_config_get).put(admin_config_put))
        .route("/admin/status", get(admin_status))
        .route(
            "/admin/providers/{name}/refresh",
            post(admin_provider_refresh),
        )
        .route("/admin/providers/{name}/test", post(admin_provider_test))
        .route(
            "/admin/providers/{name}/keys/{key_id}/restore",
            post(admin_provider_key_restore),
        )
        .route(
            "/admin/providers/{name}/keys/{key_id}/validate",
            post(admin_provider_key_validate),
        )
        .route(
            "/admin/providers/{name}/models",
            get(admin_provider_models_get),
        )
        .route(
            "/admin/providers/{name}/models/{model}/toggle",
            post(admin_provider_models_toggle),
        )
        .route("/admin/events", get(admin_events))
        .route("/admin/keys", get(admin_keys))
        .route("/admin/save", post(admin_save))
        // Model metadata routes
        .route("/admin/models/families", get(admin_model_families))
        .route("/admin/metadata", get(admin_metadata_stats))
        .route("/admin/metadata/models", get(admin_metadata_models))
        .route("/admin/metadata/attempts", get(admin_metadata_attempts))
        .route(
            "/admin/metadata/attempts/analyze",
            get(admin_metadata_attempts_analyze),
        )
        .route(
            "/admin/metadata/deployments",
            get(admin_metadata_deployments),
        )
        .route("/admin/metadata/usage", get(admin_metadata_usage))
        .route(
            "/admin/metadata/usage/daily",
            get(admin_metadata_usage_daily),
        )
        .route(
            "/admin/metadata/usage/hourly",
            get(admin_metadata_usage_hourly),
        )
        .route(
            "/admin/metadata/usage/lifetime",
            get(admin_metadata_usage_lifetime),
        )
        .route("/admin/metadata/tasks", get(admin_metadata_tasks))
        .route(
            "/admin/metadata/capabilities",
            get(admin_metadata_capabilities),
        )
        .route("/admin/metadata/errors", get(admin_metadata_errors))
        .route("/admin/metadata/sync", get(admin_metadata_sync_status))
        .route(
            "/admin/routing/adaptive",
            get(admin_adaptive_routing_diagnostics),
        )
        .route("/admin/routing/groups", get(admin_adaptive_routing_groups))
        .route("/admin/routing/routes", get(admin_adaptive_routing_routes))
        // Middleware
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    // ─── Start server ─────────────────────────────────────────────
    let addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    tracing::info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    tracing::info!("🌐 free-agent-gateway listening on http://{}", addr);
    tracing::info!("📋 OpenAI-compatible API:  http://{}/v1", addr);
    tracing::info!("🔧 Management API:          http://{}/health", addr);
    tracing::info!("📊 Metrics:                 http://{}/metrics", addr);
    tracing::info!("📋 Admin Dashboard:         http://{}/admin", addr);
    tracing::info!("🛑 Press Ctrl+C to stop the gateway gracefully",);
    tracing::info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("Gateway shutdown complete");
    Ok(())
}

/// Initialize the tracing subscriber.
fn init_tracing(level: &str) {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(
                    level
                        .to_lowercase()
                        .parse()
                        .unwrap_or_else(|_| tracing::Level::INFO.into()),
                )
                .from_env_lossy(),
        )
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .init();
}

/// Build CORS middleware layer from config.
fn build_cors_layer(config: &Config) -> CorsLayer {
    use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, Any};

    let all_wildcard = config.cors.allowed_origins.iter().any(|o| o == "*");
    let layer = CorsLayer::new()
        .allow_methods(AllowMethods::list(parse_cors_methods(
            &config.cors.allowed_methods,
        )))
        .allow_headers(AllowHeaders::list(parse_cors_headers(
            &config.cors.allowed_headers,
        )));

    if all_wildcard {
        layer.allow_origin(Any)
    } else {
        let origins = config.cors.allowed_origins.iter().filter_map(|origin| {
            origin
                .parse()
                .map_err(|error| {
                    tracing::warn!(origin, %error, "Ignoring invalid CORS origin");
                })
                .ok()
        });
        layer.allow_origin(AllowOrigin::list(origins))
    }
}

fn parse_cors_methods(values: &[String]) -> Vec<axum::http::Method> {
    values
        .iter()
        .filter_map(|value| {
            axum::http::Method::from_bytes(value.as_bytes())
                .map_err(|error| {
                    tracing::warn!(method = value, %error, "Ignoring invalid CORS method");
                })
                .ok()
        })
        .collect()
}

fn parse_cors_headers(values: &[String]) -> Vec<axum::http::HeaderName> {
    values
        .iter()
        .filter_map(|value| {
            axum::http::HeaderName::from_bytes(value.as_bytes())
                .map_err(|error| {
                    tracing::warn!(header = value, %error, "Ignoring invalid CORS header");
                })
                .ok()
        })
        .collect()
}

/// Graceful shutdown signal handler.
///
/// Listens for:
/// - `SIGTERM` on Unix (systemd `kill`/`service stop`)
/// - `Ctrl+C` on all platforms (console interrupt)
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    let sigterm = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let sigterm = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("Ctrl+C received, shutting down gracefully...");
        }
        _ = sigterm => {
            tracing::info!("SIGTERM received, shutting down gracefully...");
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{Method, header};

    use super::{parse_cors_headers, parse_cors_methods};

    #[test]
    fn cors_methods_follow_configuration_and_ignore_invalid_values() {
        let methods = parse_cors_methods(&["GET".into(), "POST".into(), "not a method".into()]);

        assert_eq!(methods, vec![Method::GET, Method::POST]);
    }

    #[test]
    fn cors_headers_follow_configuration_and_ignore_invalid_values() {
        let headers = parse_cors_headers(&[
            "Authorization".into(),
            "Content-Type".into(),
            "bad header".into(),
        ]);

        assert_eq!(headers, vec![header::AUTHORIZATION, header::CONTENT_TYPE]);
    }
}
