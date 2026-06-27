/// Watcher: Background task that periodically checks provider health and key availability.
///
/// Runs every N seconds (configurable) and updates the health registry.
use std::sync::Arc;

use dashmap::DashMap;

use crate::config::Config;
use crate::health::HealthRegistry;
use crate::keyhub::KeyHub;
use crate::providers::BoxedProvider;

/// The background watcher.
pub struct Watcher {
    config: Arc<Config>,
    providers: Arc<DashMap<String, BoxedProvider>>,
    keyhub: Arc<KeyHub>,
    health: Arc<HealthRegistry>,
}

impl Watcher {
    pub fn new(
        config: Arc<Config>,
        providers: Arc<DashMap<String, BoxedProvider>>,
        keyhub: Arc<KeyHub>,
        health: Arc<HealthRegistry>,
    ) -> Self {
        Self {
            config,
            providers,
            keyhub,
            health,
        }
    }

    /// Run a single health check cycle across all providers.
    pub async fn check_all(&self) {
        tracing::debug!("Running health check cycle");

        // Collect provider names first to avoid holding the DashMap lock across awaits
        let provider_names: Vec<String> = self
            .providers
            .iter()
            .map(|entry| entry.key().clone())
            .collect();

        for provider_name in provider_names {
            let provider = match self.providers.get(&provider_name) {
                Some(p) => p,
                None => continue,
            };

            // Skip disabled providers
            if let Some(pc) = self.config.providers.get(&provider_name)
                && !pc.enabled
            {
                self.health.update(&provider_name, "disabled", 0, 0, 0, 0);
                continue;
            }

            let check_timeout =
                std::time::Duration::from_secs(self.config.watcher.check_timeout_seconds);
            let mut successful_keys = 0usize;
            let mut total_latency = 0u64;
            let mut provider_models = std::collections::BTreeSet::new();
            let mut last_error = String::new();

            for (api_key, tier) in self.keyhub.discovery_keys(&provider_name) {
                let started = std::time::Instant::now();
                match tokio::time::timeout(check_timeout, provider.list_models(&api_key)).await {
                    Ok(Ok(models)) => {
                        total_latency += started.elapsed().as_millis() as u64;
                        successful_keys += 1;
                        provider_models.extend(models.iter().cloned());
                        self.keyhub.update_models(&provider_name, &api_key, models);
                        tracing::debug!(
                            provider = %provider_name,
                            key = %crate::keyhub::key_fingerprint(&api_key),
                            tier = %tier,
                            stage = "model_discovery",
                            "Key model discovery passed"
                        );
                    }
                    Ok(Err(error)) => {
                        let status = error.http_status();
                        if matches!(status, 401 | 403 | 429) {
                            self.keyhub.report_failure(&provider_name, &api_key, status);
                        }
                        last_error = error.to_string();
                        self.keyhub.record_model_error(
                            &provider_name,
                            &api_key,
                            &crate::error::sanitize_diagnostic(&last_error),
                        );
                    }
                    Err(_) => {
                        last_error = "Model discovery timed out".into();
                        self.keyhub
                            .record_model_error(&provider_name, &api_key, &last_error);
                    }
                }
            }

            let available = self.keyhub.available_count(&provider_name);
            let total = self
                .config
                .providers
                .get(&provider_name)
                .map(|pc| pc.keys.len())
                .unwrap_or(0);

            if successful_keys > 0 {
                self.health.update(
                    &provider_name,
                    "healthy",
                    total_latency / successful_keys as u64,
                    provider_models.len(),
                    available,
                    total,
                );
            } else {
                self.health.record_error(
                    &provider_name,
                    if last_error.is_empty() {
                        "No configured keys"
                    } else {
                        &last_error
                    },
                );
            }
        }

        tracing::info!("Health check cycle complete");
    }

    /// Run the watcher loop. This is meant to be spawned as a background task.
    pub async fn run(self: Arc<Self>) {
        if !self.config.watcher.enabled {
            tracing::info!("Watcher is disabled");
            return;
        }

        let interval = std::time::Duration::from_secs(self.config.watcher.interval_seconds);

        loop {
            self.check_all().await;
            tokio::time::sleep(interval).await;
        }
    }
}
