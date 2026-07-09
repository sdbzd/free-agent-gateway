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

            for (api_key, tier) in self.keyhub.model_probe_keys(&provider_name) {
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
                self.health.record_error_with_counts(
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
        let min_interval =
            std::time::Duration::from_secs(self.config.watcher.min_interval_seconds.max(10));
        let jitter_percent = self.config.watcher.jitter_percent.clamp(0.0, 0.95);

        loop {
            self.check_all().await;
            // Add random jitter to avoid fixed-interval background quota probes.
            let jitter = (rand::random::<f64>() * 2.0 * jitter_percent - jitter_percent)
                * interval.as_secs_f64();
            let sleep_dur = if jitter >= 0.0 {
                interval.saturating_add(std::time::Duration::from_secs_f64(jitter))
            } else {
                interval.saturating_sub(std::time::Duration::from_secs_f64(-jitter))
            };
            tokio::time::sleep(std::cmp::max(sleep_dur, min_interval)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use bytes::Bytes;
    use std::collections::HashMap;

    use crate::config::{
        Config, KeyConfig, KeyTier, ProviderConfig, ProviderType, RoutingConfig, RoutingStrategy,
        ServerConfig, WatcherConfig,
    };
    use crate::error::{GatewayError, GatewayResult};
    use crate::models::ChatCompletionRequest;
    use crate::providers::traits::{ChatResponse, Provider, StreamResponse};

    #[derive(Debug)]
    struct DiscoveryErrorProvider;

    #[derive(Debug)]
    struct DiscoveryOkProvider;

    #[async_trait]
    impl Provider for DiscoveryErrorProvider {
        fn name(&self) -> &str {
            "opencode"
        }

        fn base_url(&self) -> &str {
            "http://opencode"
        }

        async fn list_models(&self, _api_key: &str) -> GatewayResult<Vec<String>> {
            Err(GatewayError::UpstreamError(
                "error decoding response body".into(),
            ))
        }

        async fn chat(
            &self,
            _api_key: &str,
            _request: ChatCompletionRequest,
        ) -> GatewayResult<ChatResponse> {
            unreachable!("watcher model discovery test does not call chat")
        }

        async fn chat_stream(
            &self,
            _api_key: &str,
            _request: ChatCompletionRequest,
        ) -> GatewayResult<StreamResponse> {
            Ok(Box::pin(futures::stream::iter(Vec::<
                Result<Bytes, GatewayError>,
            >::new())))
        }

        async fn health_check(&self, _api_key: &str) -> GatewayResult<u64> {
            Ok(1)
        }

        fn health_check_model(&self) -> &str {
            "working-model"
        }

        fn timeout_seconds(&self) -> u64 {
            5
        }

        fn priority(&self) -> u32 {
            0
        }
    }

    #[async_trait]
    impl Provider for DiscoveryOkProvider {
        fn name(&self) -> &str {
            "opencode"
        }

        fn base_url(&self) -> &str {
            "http://opencode"
        }

        async fn list_models(&self, _api_key: &str) -> GatewayResult<Vec<String>> {
            Ok(vec!["working-model".into()])
        }

        async fn chat(
            &self,
            _api_key: &str,
            _request: ChatCompletionRequest,
        ) -> GatewayResult<ChatResponse> {
            unreachable!("watcher model discovery test does not call chat")
        }

        async fn chat_stream(
            &self,
            _api_key: &str,
            _request: ChatCompletionRequest,
        ) -> GatewayResult<StreamResponse> {
            Ok(Box::pin(futures::stream::iter(Vec::<
                Result<Bytes, GatewayError>,
            >::new())))
        }

        async fn health_check(&self, _api_key: &str) -> GatewayResult<u64> {
            Ok(1)
        }

        fn health_check_model(&self) -> &str {
            "working-model"
        }

        fn timeout_seconds(&self) -> u64 {
            5
        }

        fn priority(&self) -> u32 {
            0
        }
    }

    fn watcher_test_config() -> Config {
        let mut providers = HashMap::new();
        providers.insert(
            "opencode".into(),
            ProviderConfig {
                provider_type: ProviderType::OpenaiCompatible,
                enabled: true,
                base_url: "http://opencode".into(),
                proxy_url: None,
                keys: vec![KeyConfig::detailed("opencode-key", KeyTier::Free)],
                health_check_model: "working-model".into(),
                timeout_seconds: 5,
                priority: 0,
            },
        );

        Config {
            server: ServerConfig {
                host: "127.0.0.1".into(),
                port: 9000,
                log_level: "info".into(),
                request_timeout: 120,
                sse_keepalive: 15,
            },
            routing: RoutingConfig {
                strategy: RoutingStrategy::LeastFailed,
                fail_threshold: 3,
                cooldown_seconds: 60,
                auto_discover: true,
            },
            fallback: vec!["opencode".into()],
            agents: HashMap::new(),
            models: HashMap::new(),
            model_fallbacks: HashMap::new(),
            providers,
            watcher: WatcherConfig {
                enabled: true,
                startup_check: false,
                interval_seconds: 600,
                min_interval_seconds: 10,
                jitter_percent: 0.0,
                check_timeout_seconds: 5,
            },
            state: Default::default(),
            cors: Default::default(),
            adaptive_routing: Default::default(),
            context_compression: Default::default(),
            logging: Default::default(),
        }
    }

    #[tokio::test]
    async fn model_discovery_error_does_not_penalize_key_availability() {
        let config = Arc::new(watcher_test_config());
        let providers = Arc::new(DashMap::new());
        providers.insert(
            "opencode".into(),
            Box::new(DiscoveryErrorProvider) as BoxedProvider,
        );
        let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
        keyhub.register_provider("opencode", config.providers["opencode"].keys.clone());
        let health = Arc::new(HealthRegistry::new());
        health.register("opencode", &config.providers["opencode"]);
        let watcher = Watcher::new(config, providers, keyhub.clone(), health);

        for _ in 0..3 {
            watcher.check_all().await;
        }

        let snapshot = keyhub.snapshot();
        let key = snapshot
            .iter()
            .find(|(provider, _)| provider == "opencode")
            .unwrap()
            .1
            .first()
            .unwrap();
        assert_eq!(key.status, crate::models::KeyStatus::Available);
        assert_eq!(key.fail_count, 0);
        assert_eq!(key.total_fail_count, 0);
        assert!(
            key.models_last_error
                .contains("error decoding response body")
        );
        assert_eq!(keyhub.available_count("opencode"), 1);
    }

    #[tokio::test]
    async fn disabled_key_recovers_after_successful_model_discovery() {
        let config = Arc::new(watcher_test_config());
        let providers = Arc::new(DashMap::new());
        providers.insert(
            "opencode".into(),
            Box::new(DiscoveryOkProvider) as BoxedProvider,
        );
        let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
        keyhub.register_provider("opencode", config.providers["opencode"].keys.clone());
        keyhub.report_failure("opencode", "opencode-key", 403);
        assert_eq!(keyhub.available_count("opencode"), 0);

        let health = Arc::new(HealthRegistry::new());
        health.register("opencode", &config.providers["opencode"]);
        let watcher = Watcher::new(config, providers, keyhub.clone(), health);

        watcher.check_all().await;

        let snapshot = keyhub.snapshot();
        let key = snapshot
            .iter()
            .find(|(provider, _)| provider == "opencode")
            .unwrap()
            .1
            .first()
            .unwrap();
        assert_eq!(key.status, crate::models::KeyStatus::Available);
        assert_eq!(key.fail_count, 0);
        assert_eq!(key.models, vec!["working-model".to_string()]);
        assert_eq!(keyhub.available_count("opencode"), 1);
    }
}
