/// KeyHub: Manages API keys per provider with automatic rotation and status tracking.
///
/// Each provider can have multiple keys. KeyHub handles:
/// - Key rotation (round-robin, random, least-failed)
/// - Automatic status transitions (cooldown, rate-limited, disabled)
/// - Key recovery after cooldown expiry
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use dashmap::DashMap;
use parking_lot::RwLock;

use crate::config::{KeyConfig, KeyTier, RoutingConfig, RoutingStrategy};
use crate::error::{GatewayError, GatewayResult};
use crate::models::{KeyState, KeyStatus};

/// Escalating cooldown durations for 429 rate-limit hits (like freellmapi).
/// First hit → 2 min, second → 10 min, third → 1 hour, 4+ → 24 hours.
const COOLDOWN_ESCALATION_S: &[u64] = &[120, 600, 3600, 86400];

/// Global counter for round-robin key selection.
static ROUND_ROBIN_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Create a stable, non-credential identifier for persisted key metadata.
pub fn key_fingerprint(key: &str) -> String {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    format!("key-{:016x}", hasher.finish())
}

/// Key pool for a single provider.
pub struct KeyPool {
    provider_name: String,
    keys: RwLock<Vec<KeyState>>,
    routing: RoutingConfig,
    /// Aggregate consecutive failure count across all keys.
    /// Reset on any key success. Used for provider-level cooldown.
    provider_fail_count: AtomicU64,
    /// Epoch seconds when provider-level cooldown expires. 0 = not in cooldown.
    provider_cooldown_until: AtomicU64,
}

impl KeyPool {
    /// Create a new key pool from raw key strings.
    pub fn new(provider_name: &str, keys: Vec<KeyConfig>, routing: RoutingConfig) -> Self {
        let key_states: Vec<KeyState> = keys
            .into_iter()
            .map(|key| {
                let mut state = KeyState::with_tier(key.value().to_string(), key.tier());
                state.rpm_limit = key.rpm_limit();
                state.rpd_limit = key.rpd_limit();
                state.tpm_limit = key.tpm_limit();
                state.tpd_limit = key.tpd_limit();
                state
            })
            .collect();
        Self {
            provider_name: provider_name.to_string(),
            keys: RwLock::new(key_states),
            routing,
            provider_fail_count: AtomicU64::new(0),
            provider_cooldown_until: AtomicU64::new(0),
        }
    }

    /// Get an available key using the configured routing strategy.
    /// Skips keys that have exceeded rate limits.
    /// Returns the key string, or error if no keys are available.
    pub fn acquire_key(&self) -> GatewayResult<String> {
        let mut keys = self.keys.write();
        let now = chrono::Utc::now().timestamp() as u64;

        self.recover_expired(&mut keys);

        // Reset rate windows for all keys first
        for k in keys.iter_mut() {
            k.reset_rate_windows(now);
        }

        let available_indices: Vec<usize> = keys
            .iter()
            .enumerate()
            .filter(|(_, k)| k.status == KeyStatus::Available && !k.is_rate_limited(now))
            .map(|(i, _)| i)
            .collect();

        if available_indices.is_empty() {
            return Err(GatewayError::NoAvailableKeys(self.provider_name.clone()));
        }

        let idx = match self.routing.strategy {
            RoutingStrategy::RoundRobin => {
                let counter = ROUND_ROBIN_COUNTER.fetch_add(1, Ordering::Relaxed);
                available_indices[counter as usize % available_indices.len()]
            }
            RoutingStrategy::Random => {
                let rand_idx = rand::random::<usize>() % available_indices.len();
                available_indices[rand_idx]
            }
            RoutingStrategy::LeastFailed => {
                // Pick the key with the fewest failures
                available_indices
                    .iter()
                    .min_by_key(|&&i| keys[i].fail_count)
                    .copied()
                    .unwrap_or(available_indices[0])
            }
            RoutingStrategy::LeastRate => {
                // Pick the key with the lowest rate-limit usage percentage.
                // For each rate axis (RPM/RPD/TPM/TPD) that has a limit set,
                // compute usage = count / limit and take the max across axes.
                // Prefer the key with the smallest max-usage (most headroom).
                available_indices
                    .iter()
                    .min_by_key(|&&i| {
                        let k = &keys[i];
                        let now_secs = chrono::Utc::now().timestamp() as u64;
                        let now_min = now_secs / 60;
                        let now_day = now_secs / 86400;
                        let rpm_pct = k.rpm_limit.map(|lim| {
                            if lim == 0 { return 100u32; }
                            let cnt = if k.rpm_window_start == now_min { k.rpm_count } else { 0 };
                            (cnt * 100) / lim
                        }).unwrap_or(0);
                        let rpd_pct = k.rpd_limit.map(|lim| {
                            if lim == 0 { return 100u32; }
                            let cnt = if k.rpd_window_start == now_day { k.rpd_count } else { 0 };
                            (cnt * 100) / lim
                        }).unwrap_or(0);
                        let tpm_pct = k.tpm_limit.map(|lim| {
                            if lim == 0 { return 100u32; }
                            let used = if k.rpm_window_start == now_min { k.tpm_prompt_count + k.tpm_completion_count } else { 0 };
                            (used * 100) / lim
                        }).unwrap_or(0);
                        let tpd_pct = k.tpd_limit.map(|lim| {
                            if lim == 0 { return 100u32; }
                            let used = if k.rpd_window_start == now_day { k.tpd_prompt_count + k.tpd_completion_count } else { 0 };
                            (used * 100) / lim
                        }).unwrap_or(0);
                        std::cmp::max(rpm_pct, std::cmp::max(rpd_pct, std::cmp::max(tpm_pct, tpd_pct)))
                    })
                    .copied()
                    .unwrap_or(available_indices[0])
            }
            RoutingStrategy::Priority => {
                // Pick the first available key
                available_indices[0]
            }
        };

        Ok(keys[idx].key.clone())
    }

    /// Restore persisted operational metadata onto configured keys.
    pub fn restore_states(&self, persisted: &[KeyState]) {
        let mut keys = self.keys.write();
        for configured in keys.iter_mut() {
            let configured_id = key_fingerprint(&configured.key);
            let Some(saved) = persisted.iter().find(|saved| {
                let saved_id = if saved.key_id.is_empty() && !saved.key.is_empty() {
                    key_fingerprint(&saved.key)
                } else {
                    saved.key_id.clone()
                };
                saved_id == configured_id
            }) else {
                continue;
            };

            configured.status = saved.status;
            configured.models = saved.models.clone();
            configured.models_updated_at = saved.models_updated_at;
            configured.models_last_error = saved.models_last_error.clone();
            configured.fail_count = saved.fail_count;
            configured.cooldown_until = saved.cooldown_until;
            configured.success_count = saved.success_count;
            configured.total_fail_count = saved.total_fail_count;
            configured.key_id = configured_id;
        }
        self.recover_expired(&mut keys);
    }

    fn recover_expired(&self, keys: &mut [KeyState]) {
        let now = chrono::Utc::now().timestamp() as u64;
        for key in keys.iter_mut() {
            if key.cooldown_until.is_some_and(|until| now >= until) {
                match key.status {
                    KeyStatus::Cooldown => {
                        // Non-429 cooldown: reset fail_count for a fresh start
                        key.status = KeyStatus::Available;
                        key.fail_count = 0;
                        key.cooldown_until = None;
                        tracing::info!(
                            provider = %self.provider_name,
                            key = %key.masked_key(),
                            stage = "key_recovery",
                            "Key recovered from cooldown"
                        );
                    }
                    KeyStatus::RateLimited => {
                        // 429 rate limit: preserve fail_count so escalation
                        // (2min → 10min → 1hr → 24hr) persists across recovery cycles
                        key.status = KeyStatus::Available;
                        key.cooldown_until = None;
                        tracing::info!(
                            provider = %self.provider_name,
                            key = %key.masked_key(),
                            fail_count = key.fail_count,
                            stage = "key_recovery",
                            "Key recovered from rate limit"
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    /// Report a successful request for a key.
    /// `prompt_tokens` and `completion_tokens` are optional for rate tracking.
    pub fn report_success(
        &self,
        key: &str,
        prompt_tokens: Option<u32>,
        completion_tokens: Option<u32>,
    ) {
        let mut keys = self.keys.write();
        let now = chrono::Utc::now().timestamp() as u64;
        if let Some(k) = keys.iter_mut().find(|k| k.key == key) {
            k.success_count += 1;
            k.fail_count = 0;

            // Reset rate windows and increment counters
            k.reset_rate_windows(now);
            k.rpm_count = k.rpm_count.saturating_add(1);
            k.rpd_count = k.rpd_count.saturating_add(1);

            if let Some(p) = prompt_tokens {
                k.tpm_prompt_count = k.tpm_prompt_count.saturating_add(p);
                k.tpd_prompt_count = k.tpd_prompt_count.saturating_add(p);
            }
            if let Some(c) = completion_tokens {
                k.tpm_completion_count = k.tpm_completion_count.saturating_add(c);
                k.tpd_completion_count = k.tpd_completion_count.saturating_add(c);
            }
        }

        // Any success resets provider-level failure tracking
        self.provider_fail_count.store(0, Ordering::Relaxed);
        self.provider_cooldown_until.store(0, Ordering::Relaxed);
    }

    /// Report a failure for a key, handling automatic status transitions.
    pub fn report_failure(&self, key: &str, http_status: u16) {
        let mut keys = self.keys.write();
        if let Some(k) = keys.iter_mut().find(|k| k.key == key) {
            match http_status {
                401 | 403 => {
                    // Auth failure → permanently disabled
                    k.status = KeyStatus::Disabled;
                    k.total_fail_count += 1;
                    tracing::warn!(
                        provider = %self.provider_name,
                        key = %k.masked_key(),
                        status = http_status,
                        "Key disabled due to auth failure"
                    );
                }
                429 => {
                    // Rate limited → escalating cooldown
                    // Track consecutive 429 hits per key to pick the right tier.
                    // Stored as a simple counter in the key state's fail_count field
                    // (we reset fail_count on success but preserve it for 429 tracking).
                    let tier = (k.fail_count as usize).min(COOLDOWN_ESCALATION_S.len() - 1);
                    let cooldown_s = COOLDOWN_ESCALATION_S[tier];
                    k.status = KeyStatus::RateLimited;
                    k.cooldown_until =
                        Some(chrono::Utc::now().timestamp() as u64 + cooldown_s);
                    k.fail_count += 1;
                    k.total_fail_count += 1;
                    tracing::warn!(
                        provider = %self.provider_name,
                        key = %k.masked_key(),
                        tier = tier,
                        "Key rate limited, escalating cooldown for {}s",
                        cooldown_s
                    );
                }
                _ => {
                    // General failure (5xx, timeout, etc.)
                    k.fail_count += 1;
                    k.total_fail_count += 1;

                    if k.fail_count >= self.routing.fail_threshold {
                        k.status = KeyStatus::Cooldown;
                        k.cooldown_until = Some(
                            chrono::Utc::now().timestamp() as u64 + self.routing.cooldown_seconds,
                        );
                        tracing::warn!(
                            provider = %self.provider_name,
                            key = %k.masked_key(),
                            fail_count = k.fail_count,
                            "Key entering cooldown due to consecutive failures"
                        );
                }
            }
        }

        // Provider-level failure tracking.
        // If aggregate failures across all keys exceed threshold,
        // enter provider cooldown.
        let prev_fails = self.provider_fail_count.fetch_add(1, Ordering::Relaxed);
        let key_count = keys.len() as u64;
        let threshold = key_count * self.routing.fail_threshold as u64;
        if prev_fails + 1 >= threshold {
            let cooldown_until =
                chrono::Utc::now().timestamp() as u64 + self.routing.cooldown_seconds;
            self.provider_cooldown_until.store(cooldown_until, Ordering::Relaxed);
            tracing::warn!(
                provider = %self.provider_name,
                fail_count = prev_fails + 1,
                threshold = threshold,
                cooldown_s = self.routing.cooldown_seconds,
                "Provider entering cooldown due to aggregate failures"
            );
        }
    }
    }

    /// Get the number of available keys.
    pub fn available_count(&self) -> usize {
        let mut keys = self.keys.write();
        self.recover_expired(&mut keys);
        keys.iter()
            .filter(|k| k.status == KeyStatus::Available)
            .count()
    }

    /// Get the total number of keys.
    pub fn total_count(&self) -> usize {
        self.keys.read().len()
    }

    /// Get a snapshot of all key states (keys are masked).
    pub fn snapshot(&self) -> Vec<KeyState> {
        let keys = self.keys.read();
        keys.iter()
            .map(|k| KeyState {
                key: k.masked_key(),
                key_id: key_fingerprint(&k.key),
                tier: k.tier,
                models: k.models.clone(),
                models_updated_at: k.models_updated_at,
                models_last_error: k.models_last_error.clone(),
                status: k.status,
                fail_count: k.fail_count,
                cooldown_until: k.cooldown_until,
                success_count: k.success_count,
                total_fail_count: k.total_fail_count,
                rpm_limit: k.rpm_limit,
                rpd_limit: k.rpd_limit,
                tpm_limit: k.tpm_limit,
                tpd_limit: k.tpd_limit,
                rpm_count: k.rpm_count,
                rpd_count: k.rpd_count,
                tpm_prompt_count: k.tpm_prompt_count,
                tpm_completion_count: k.tpm_completion_count,
                tpd_prompt_count: k.tpd_prompt_count,
                tpd_completion_count: k.tpd_completion_count,
                rpm_window_start: k.rpm_window_start,
                rpd_window_start: k.rpd_window_start,
            })
            .collect()
    }
}

/// KeyHub: manages key pools for all providers.
pub struct KeyHub {
    pools: DashMap<String, KeyPool>,
    routing: RoutingConfig,
}

impl KeyHub {
    /// Create a new KeyHub.
    pub fn new(routing: RoutingConfig) -> Self {
        Self {
            pools: DashMap::new(),
            routing,
        }
    }

    /// Register a provider's keys.
    pub fn register_provider(&self, provider_name: &str, keys: Vec<KeyConfig>) {
        let key_count = keys.len();
        let pool = KeyPool::new(provider_name, keys, self.routing.clone());
        self.pools.insert(provider_name.to_string(), pool);
        tracing::info!(provider = %provider_name, keys_count = key_count, "Registered provider keys");
    }

    /// Acquire an available key for a provider.
    pub fn acquire_key(&self, provider_name: &str) -> GatewayResult<String> {
        self.pools
            .get(provider_name)
            .ok_or_else(|| GatewayError::ProviderNotFound(provider_name.to_string()))?
            .acquire_key()
    }

    /// Report success for a provider's key.
    pub fn report_success(
        &self,
        provider_name: &str,
        key: &str,
        prompt_tokens: Option<u32>,
        completion_tokens: Option<u32>,
    ) {
        if let Some(pool) = self.pools.get(provider_name) {
            pool.report_success(key, prompt_tokens, completion_tokens);
        }
    }

    /// Report failure for a provider's key.
    pub fn report_failure(&self, provider_name: &str, key: &str, http_status: u16) {
        if let Some(pool) = self.pools.get(provider_name) {
            pool.report_failure(key, http_status);
        }
    }

    /// Restore persisted key metadata for a registered provider.
    pub fn restore_provider_states(&self, provider_name: &str, states: &[KeyState]) {
        if let Some(pool) = self.pools.get(provider_name) {
            pool.restore_states(states);
        }
    }

    /// Get a snapshot of all key pools.
    pub fn snapshot(&self) -> Vec<(String, Vec<KeyState>)> {
        self.pools
            .iter()
            .map(|entry| {
                let (name, pool) = (entry.key().clone(), entry.value().snapshot());
                (name, pool)
            })
            .collect()
    }

    /// Check if a provider has any available keys.
    pub fn has_available_keys(&self, provider_name: &str) -> bool {
        self.available_count(provider_name) > 0
    }

    /// Get the exact number of currently available keys for a provider.
    pub fn available_count(&self, provider_name: &str) -> usize {
        self.pools
            .get(provider_name)
            .map(|pool| pool.available_count())
            .unwrap_or(0)
    }

    pub fn discovery_keys(&self, provider_name: &str) -> Vec<(String, KeyTier)> {
        self.pools
            .get(provider_name)
            .map(|pool| {
                pool.keys
                    .read()
                    .iter()
                    .map(|key| (key.key.clone(), key.tier))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn update_models(&self, provider_name: &str, key: &str, mut models: Vec<String>) {
        models.sort();
        models.dedup();
        if let Some(pool) = self.pools.get(provider_name) {
            let mut keys = pool.keys.write();
            if let Some(state) = keys.iter_mut().find(|state| state.key == key) {
                state.models = models;
                state.models_updated_at = Some(chrono::Utc::now().timestamp());
                state.models_last_error.clear();
            }
        }
    }

    pub fn record_model_error(&self, provider_name: &str, key: &str, error: &str) {
        if let Some(pool) = self.pools.get(provider_name) {
            let mut keys = pool.keys.write();
            if let Some(state) = keys.iter_mut().find(|state| state.key == key) {
                state.models_last_error = error.to_string();
            }
        }
    }

    pub fn free_candidates(&self, provider_name: &str, model: &str, agent_name: Option<&str>) -> Vec<String> {
        self.pools
            .get(provider_name)
            .map(|pool| {
                // Provider-level cooldown check
                let now = chrono::Utc::now().timestamp() as u64;
                let cooldown_until = pool.provider_cooldown_until.load(Ordering::Relaxed);
                if cooldown_until > 0 {
                    if now < cooldown_until {
                        tracing::debug!(
                            provider = %provider_name,
                            "Provider in cooldown, skipping candidate selection"
                        );
                        return vec![];
                    }
                    // Expired cooldown — reset
                    pool.provider_cooldown_until.store(0, Ordering::Relaxed);
                    pool.provider_fail_count.store(0, Ordering::Relaxed);
                }

                let mut keys = pool.keys.write();
                pool.recover_expired(&mut keys);
                // Reset rate windows for all keys
                for k in keys.iter_mut() {
                    k.reset_rate_windows(now);
                }
                let mut candidates: Vec<&KeyState> = keys
                    .iter()
                    .filter(|key| {
                        key.tier == KeyTier::Free
                            && key.status == KeyStatus::Available
                            && !key.is_rate_limited(now)
                            && key.models.iter().any(|candidate| candidate == model)
                    })
                    .collect();
                candidates.sort_by_key(|key| key.fail_count);
                let mut result: Vec<String> = candidates.into_iter().map(|key| key.key.clone()).collect();

                // Agent-aware key distribution: rotate the candidate list so
                // different agents start at different keys. This prevents
                // Agent A from exhausting a single key and affecting others.
                if result.len() > 1 {
                    if let Some(agent) = agent_name.filter(|a| !a.is_empty()) {
                        let hash: u32 = agent.bytes().fold(0u32, |a, b| a.wrapping_add(b as u32));
                        let shift = (hash as usize) % result.len();
                        result.rotate_left(shift);
                    }
                }

                result
            })
            .unwrap_or_default()
    }

    /// Get any available free key from a provider, ignoring model inventory.
    /// Used for emergency cross-provider fallback when model-specific
    /// candidates are exhausted or unreachable.
    pub fn any_free_key(&self, provider_name: &str) -> Option<String> {
        self.pools.get(provider_name).and_then(|pool| {
            // Provider-level cooldown check
            let now = chrono::Utc::now().timestamp() as u64;
            let cooldown_until = pool.provider_cooldown_until.load(Ordering::Relaxed);
            if cooldown_until > 0 {
                if now < cooldown_until {
                    tracing::debug!(
                        provider = %provider_name,
                        "Provider in cooldown, any_free_key returns None"
                    );
                    return None;
                }
                // Expired cooldown — reset
                pool.provider_cooldown_until.store(0, Ordering::Relaxed);
                pool.provider_fail_count.store(0, Ordering::Relaxed);
            }

            let mut keys = pool.keys.write();
            pool.recover_expired(&mut keys);
            for k in keys.iter_mut() {
                k.reset_rate_windows(now);
            }
            keys.iter()
                .find(|k| {
                    k.tier == KeyTier::Free
                        && k.status == KeyStatus::Available
                        && !k.is_rate_limited(now)
                })
                .map(|k| k.key.clone())
        })
    }

    /// Get any available key (Free or Paid tier) from a provider, ignoring model inventory.
    /// Used for paid key escalation when all free keys are rate-limited.
    pub fn any_available_key(&self, provider_name: &str) -> Option<String> {
        self.pools.get(provider_name).and_then(|pool| {
            // Provider-level cooldown check
            let now = chrono::Utc::now().timestamp() as u64;
            let cooldown_until = pool.provider_cooldown_until.load(Ordering::Relaxed);
            if cooldown_until > 0 {
                if now < cooldown_until {
                    tracing::debug!(
                        provider = %provider_name,
                        "Provider in cooldown, any_available_key returns None"
                    );
                    return None;
                }
                pool.provider_cooldown_until.store(0, Ordering::Relaxed);
                pool.provider_fail_count.store(0, Ordering::Relaxed);
            }

            let mut keys = pool.keys.write();
            pool.recover_expired(&mut keys);
            for k in keys.iter_mut() {
                k.reset_rate_windows(now);
            }
            // First try paid keys (explicit Paid tier), then fall back to Free
            keys.iter()
                .find(|k| {
                    k.tier == KeyTier::Paid
                        && k.status == KeyStatus::Available
                        && !k.is_rate_limited(now)
                })
                .or_else(|| {
                    keys.iter().find(|k| {
                        k.tier == KeyTier::Free
                            && k.status == KeyStatus::Available
                            && !k.is_rate_limited(now)
                    })
                })
                .map(|k| k.key.clone())
        })
    }

    /// Lightweight summary of free key model counts per provider.
    /// Uses read-only snapshot — no write locks, no side effects.
    /// Returns e.g. "nvidia=42, github=15"
    pub fn free_model_summary(&self) -> String {
        let mut counts: Vec<(String, usize)> = self
            .pools
            .iter()
            .map(|pool| {
                let provider = pool.key().clone();
                let model_count: usize = pool
                    .value()
                    .snapshot()
                    .iter()
                    .filter(|k| k.tier == KeyTier::Free && k.status == KeyStatus::Available)
                    .map(|k| k.models.len())
                    .sum();
                (provider, model_count)
            })
            .filter(|(_, c)| *c > 0)
            .collect();
        counts.sort_by(|a, b| b.1.cmp(&a.1));
        counts
            .into_iter()
            .map(|(p, c)| format!("{p}={c}"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn available_free_models(&self) -> Vec<(String, String)> {
        let mut models = Vec::new();
        let now = chrono::Utc::now().timestamp() as u64;
        for pool in self.pools.iter() {
            let provider = pool.key().clone();
            let mut keys = pool.value().keys.write();
            pool.value().recover_expired(&mut keys);
            for k in keys.iter_mut() {
                k.reset_rate_windows(now);
            }
            for key in keys
                .iter()
                .filter(|key| {
                    key.tier == KeyTier::Free
                        && key.status == KeyStatus::Available
                        && !key.is_rate_limited(now)
                })
            {
                models.extend(
                    key.models
                        .iter()
                        .cloned()
                        .map(|model| (provider.clone(), model)),
                );
            }
        }
        models.sort();
        models.dedup();
        models
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_routing() -> RoutingConfig {
        RoutingConfig {
            strategy: RoutingStrategy::LeastFailed,
            fail_threshold: 3,
            cooldown_seconds: 60,
            auto_discover: true,
        }
    }

    #[test]
    fn test_key_pool_basic() {
        let pool = KeyPool::new(
            "test",
            vec!["key1".into(), "key2".into(), "key3".into()],
            test_routing(),
        );
        assert_eq!(pool.total_count(), 3);
        assert_eq!(pool.available_count(), 3);
    }

    #[test]
    fn test_acquire_key() {
        let pool = KeyPool::new("test", vec!["key1".into(), "key2".into()], test_routing());
        let key = pool.acquire_key().unwrap();
        assert!(!key.is_empty());
    }

    #[test]
    fn test_report_failure_429() {
        let pool = KeyPool::new("test", vec!["key1".into()], test_routing());
        let key = "key1".to_string();
        pool.report_failure(&key, 429);
        let snapshot = pool.snapshot();
        assert_eq!(snapshot[0].status, KeyStatus::RateLimited);
        assert_eq!(pool.available_count(), 0);
    }

    #[test]
    fn test_report_failure_401_disables_key() {
        let pool = KeyPool::new("test", vec!["key1".into()], test_routing());
        pool.report_failure("key1", 401);
        assert_eq!(pool.available_count(), 0);
        let snapshot = pool.snapshot();
        assert_eq!(snapshot[0].status, KeyStatus::Disabled);
    }

    #[test]
    fn test_consecutive_failures_cooldown() {
        let pool = KeyPool::new("test", vec!["key1".into()], test_routing());
        pool.report_failure("key1", 500);
        pool.report_failure("key1", 500);
        pool.report_failure("key1", 500); // threshold reached
        let snapshot = pool.snapshot();
        assert_eq!(snapshot[0].status, KeyStatus::Cooldown);
        assert_eq!(pool.available_count(), 0);
    }

    #[test]
    fn test_no_available_keys() {
        let pool = KeyPool::new("test", vec![], test_routing());
        let result = pool.acquire_key();
        assert!(result.is_err());
    }
}
