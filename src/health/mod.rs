/// Health: Tracks provider health state and provides metrics.
///
/// The health module aggregates data from the watcher and keyhub
/// to produce a unified view of system health.
use dashmap::DashMap;
use parking_lot::RwLock;

use crate::config::ProviderConfig;
use crate::models::HealthState;

/// Global health registry.
pub struct HealthRegistry {
    states: DashMap<String, RwLock<HealthState>>,
}

impl HealthRegistry {
    /// Create a new health registry.
    pub fn new() -> Self {
        Self {
            states: DashMap::new(),
        }
    }

    /// Register a provider's health state.
    pub fn register(&self, provider_name: &str, config: &ProviderConfig) {
        let state = HealthState {
            provider: provider_name.to_string(),
            status: "unknown".into(),
            latency_ms: 0,
            success_count: 0,
            fail_count: 0,
            last_error: String::new(),
            cooldown_until: None,
            models_count: 0,
            available_keys: config.keys.len(),
            total_keys: config.keys.len(),
        };
        self.states
            .insert(provider_name.to_string(), RwLock::new(state));
    }

    /// Update a provider's health state after a check.
    pub fn update(
        &self,
        provider_name: &str,
        status: &str,
        latency_ms: u64,
        models_count: usize,
        available_keys: usize,
        total_keys: usize,
    ) {
        if let Some(entry) = self.states.get(provider_name) {
            let mut state = entry.write();
            state.status = status.to_string();
            state.latency_ms = latency_ms;
            state.models_count = models_count;
            state.available_keys = available_keys;
            state.total_keys = total_keys;

            if status == "healthy" {
                state.success_count += 1;
                state.last_error.clear();
            } else {
                state.fail_count += 1;
                state.cooldown_until = None;
            }
        }
    }

    /// Record an error for a provider.
    pub fn record_error(&self, provider_name: &str, error: &str) {
        self.record_error_inner(provider_name, error, None);
    }

    /// Record an error and refresh key counts from the keyhub snapshot.
    pub fn record_error_with_counts(
        &self,
        provider_name: &str,
        error: &str,
        available_keys: usize,
        total_keys: usize,
    ) {
        self.record_error_inner(provider_name, error, Some((available_keys, total_keys)));
    }

    fn record_error_inner(
        &self,
        provider_name: &str,
        error: &str,
        key_counts: Option<(usize, usize)>,
    ) {
        if let Some(entry) = self.states.get(provider_name) {
            let mut state = entry.write();
            state.status = "unhealthy".into();
            state.last_error = error.to_string();
            state.fail_count += 1;
            if let Some((available_keys, total_keys)) = key_counts {
                state.available_keys = available_keys;
                state.total_keys = total_keys;
            }
        }
    }

    /// Get a snapshot of all health states.
    pub fn snapshot(&self) -> Vec<HealthState> {
        self.states
            .iter()
            .map(|entry| {
                let state = entry.value().read();
                state.clone()
            })
            .collect()
    }

    /// Check if all providers are down (used for Ollama fallback decision).
    pub fn all_remote_down(&self) -> bool {
        let states = self.snapshot();
        states
            .iter()
            .filter(|s| s.provider != "ollama")
            .all(|s| s.status != "healthy")
    }
}

impl Default for HealthRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{KeyConfig, KeyTier, ProviderConfig, ProviderType};

    fn provider_config() -> ProviderConfig {
        ProviderConfig {
            provider_type: ProviderType::OpenaiCompatible,
            enabled: true,
            base_url: "http://provider".into(),
            proxy_url: None,
            keys: vec![
                KeyConfig::detailed("key-1", KeyTier::Free),
                KeyConfig::detailed("key-2", KeyTier::Free),
            ],
            health_check_model: "model".into(),
            timeout_seconds: 5,
            priority: 0,
        }
    }

    #[test]
    fn record_error_with_counts_updates_key_counts() {
        let health = HealthRegistry::new();
        health.register("groq", &provider_config());

        health.record_error_with_counts("groq", "No configured keys", 0, 2);

        let state = health
            .snapshot()
            .into_iter()
            .find(|state| state.provider == "groq")
            .unwrap();
        assert_eq!(state.status, "unhealthy");
        assert_eq!(state.available_keys, 0);
        assert_eq!(state.total_keys, 2);
        assert_eq!(state.last_error, "No configured keys");
    }
}
