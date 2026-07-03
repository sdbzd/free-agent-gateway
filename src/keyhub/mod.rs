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

/// Candidate metadata used by the router to compare keys across providers
/// without exposing raw key state or credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreeKeyCandidate {
    pub key: String,
    pub usage_sort_key: (u32, u32, u32, u32, u32),
    pub fail_count: u32,
    pub total_fail_count: u64,
}

/// Create a stable, non-credential identifier for persisted key metadata.
pub fn key_fingerprint(key: &str) -> String {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    format!("key-{:016x}", hasher.finish())
}

fn key_usage_score(key: &KeyState, now_secs: u64) -> u32 {
    let now_min = now_secs / 60;
    let now_day = now_secs / 86400;
    let rpm_pct = key
        .rpm_limit
        .map(|limit| {
            percent_used(
                current_minute_count(key.rpm_window_start, key.rpm_count, now_min),
                limit,
            )
        })
        .unwrap_or(0);
    let rpd_pct = key
        .rpd_limit
        .map(|limit| {
            percent_used(
                current_day_count(key.rpd_window_start, key.rpd_count, now_day),
                limit,
            )
        })
        .unwrap_or(0);
    let tpm_used = current_minute_count(
        key.rpm_window_start,
        key.tpm_prompt_count
            .saturating_add(key.tpm_completion_count),
        now_min,
    );
    let tpd_used = current_day_count(
        key.rpd_window_start,
        key.tpd_prompt_count
            .saturating_add(key.tpd_completion_count),
        now_day,
    );
    let tpm_pct = key
        .tpm_limit
        .map(|limit| percent_used(tpm_used, limit))
        .unwrap_or(0);
    let tpd_pct = key
        .tpd_limit
        .map(|limit| percent_used(tpd_used, limit))
        .unwrap_or(0);

    rpm_pct.max(rpd_pct).max(tpm_pct).max(tpd_pct)
}

fn key_usage_sort_key(key: &KeyState, now_secs: u64) -> (u32, u32, u32, u32, u32) {
    let now_min = now_secs / 60;
    let now_day = now_secs / 86400;
    let minute_count = current_minute_count(key.rpm_window_start, key.rpm_count, now_min);
    let day_count = current_day_count(key.rpd_window_start, key.rpd_count, now_day);
    (
        key_usage_score(key, now_secs),
        day_count,
        minute_count,
        key.fail_count,
        key.total_fail_count.min(u32::MAX as u64) as u32,
    )
}

fn current_minute_count(window_start: u64, count: u32, now_min: u64) -> u32 {
    if window_start == now_min { count } else { 0 }
}

fn current_day_count(window_start: u64, count: u32, now_day: u64) -> u32 {
    if window_start == now_day { count } else { 0 }
}

fn percent_used(used: u32, limit: u32) -> u32 {
    if limit == 0 {
        return 100;
    }
    used.saturating_mul(100) / limit
}

fn is_request_selectable(key: &KeyState, now_secs: u64) -> bool {
    matches!(key.status, KeyStatus::Available | KeyStatus::Probing)
        && !key.is_rate_limited(now_secs)
}

fn key_usage_sort_key_for_selection(key: &KeyState, now_secs: u64) -> (u32, u32, u32, u32, u32) {
    let mut sort_key = key_usage_sort_key(key, now_secs);
    if key.status == KeyStatus::Probing {
        sort_key.0 = sort_key.0.saturating_add(1000);
    }
    sort_key
}

fn tighten_observed_limit(limit: &mut Option<u32>, observed: u32) {
    if observed == 0 {
        return;
    }
    *limit = Some(limit.map_or(observed, |current| current.min(observed)));
}

fn can_observed_limit_update(source: &Option<String>) -> bool {
    !matches!(source.as_deref(), Some("config" | "official_api"))
}

fn set_request_limit(
    limit: &mut Option<u32>,
    source: &mut Option<String>,
    value: u32,
    new_source: &str,
) {
    if value == 0 {
        return;
    }
    if matches!(source.as_deref(), Some("config")) && new_source != "config" {
        return;
    }
    *limit = Some(value);
    *source = Some(new_source.to_string());
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
                state.rpm_limit_source = key.rpm_limit().map(|_| "config".to_string());
                state.rpd_limit_source = key.rpd_limit().map(|_| "config".to_string());
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
            .filter(|(_, k)| is_request_selectable(k, now))
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
                    .min_by_key(|&&i| key_usage_sort_key_for_selection(&keys[i], now))
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
            configured.last_success_at = saved.last_success_at;
            configured.last_error_at = saved.last_error_at;
            configured.last_error_status = saved.last_error_status;
            configured.status_updated_at = saved.status_updated_at;
            configured.last_recovered_at = saved.last_recovered_at;
            configured.success_count = saved.success_count;
            configured.total_fail_count = saved.total_fail_count;
            if configured.rpm_limit.is_none() {
                configured.rpm_limit = saved.rpm_limit;
                configured.rpm_limit_source = saved.rpm_limit_source.clone();
            }
            if configured.rpd_limit.is_none() {
                configured.rpd_limit = saved.rpd_limit;
                configured.rpd_limit_source = saved.rpd_limit_source.clone();
            }
            configured.rpm_count = saved.rpm_count;
            configured.rpd_count = saved.rpd_count;
            configured.tpm_prompt_count = saved.tpm_prompt_count;
            configured.tpm_completion_count = saved.tpm_completion_count;
            configured.tpd_prompt_count = saved.tpd_prompt_count;
            configured.tpd_completion_count = saved.tpd_completion_count;
            configured.rpm_window_start = saved.rpm_window_start;
            configured.rpd_window_start = saved.rpd_window_start;
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
                        key.status_updated_at = Some(now);
                        key.last_recovered_at = Some(now);
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
                        key.status = KeyStatus::Probing;
                        key.cooldown_until = None;
                        key.status_updated_at = Some(now);
                        key.last_recovered_at = Some(now);
                        tracing::info!(
                            provider = %self.provider_name,
                            key = %key.masked_key(),
                            fail_count = key.fail_count,
                            stage = "key_recovery",
                            "Key ready for rate-limit recovery probe"
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
            k.last_success_at = Some(now);
            if k.status == KeyStatus::Probing
                || k.status == KeyStatus::RateLimited
                || k.status == KeyStatus::Disabled
                || (k.status == KeyStatus::Cooldown && k.last_error_status == Some(429))
            {
                k.status = KeyStatus::Available;
                k.cooldown_until = None;
                k.status_updated_at = Some(now);
            }

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

    /// Reserve one request against RPM/RPD before sending it upstream.
    ///
    /// This prevents concurrent callers from all selecting the same key while
    /// seeing stale counters. Token counters are still recorded after success.
    pub fn reserve_key(&self, key: &str) -> bool {
        let mut keys = self.keys.write();
        let now = chrono::Utc::now().timestamp() as u64;
        self.recover_expired(&mut keys);
        let Some(k) = keys.iter_mut().find(|k| k.key == key) else {
            return false;
        };
        k.reset_rate_windows(now);
        if !is_request_selectable(k, now) {
            return false;
        }
        if k.status == KeyStatus::Probing {
            k.status = KeyStatus::Cooldown;
            k.cooldown_until = Some(now + self.routing.cooldown_seconds);
            k.status_updated_at = Some(now);
            tracing::info!(
                provider = %self.provider_name,
                key = %k.masked_key(),
                cooldown_s = self.routing.cooldown_seconds,
                "Key reserved for recovery probe"
            );
        }
        k.rpm_count = k.rpm_count.saturating_add(1);
        k.rpd_count = k.rpd_count.saturating_add(1);
        true
    }

    /// Report success for a request that was already reserved.
    pub fn report_reserved_success(
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
            k.last_success_at = Some(now);
            if k.status == KeyStatus::Probing
                || k.status == KeyStatus::RateLimited
                || k.status == KeyStatus::Disabled
                || (k.status == KeyStatus::Cooldown && k.last_error_status == Some(429))
            {
                k.status = KeyStatus::Available;
                k.cooldown_until = None;
                k.status_updated_at = Some(now);
            }
            k.reset_rate_windows(now);

            if let Some(p) = prompt_tokens {
                k.tpm_prompt_count = k.tpm_prompt_count.saturating_add(p);
                k.tpd_prompt_count = k.tpd_prompt_count.saturating_add(p);
            }
            if let Some(c) = completion_tokens {
                k.tpm_completion_count = k.tpm_completion_count.saturating_add(c);
                k.tpd_completion_count = k.tpd_completion_count.saturating_add(c);
            }
        }

        self.provider_fail_count.store(0, Ordering::Relaxed);
        self.provider_cooldown_until.store(0, Ordering::Relaxed);
    }

    /// Report a failure for a key, handling automatic status transitions.
    pub fn report_failure(&self, key: &str, http_status: u16) {
        self.report_failure_with_retry_after(key, http_status, None);
    }

    /// Report a transient upstream failure without auth/rate-limit special handling.
    pub fn report_transient_failure(&self, key: &str, http_status: u16) {
        let mut keys = self.keys.write();
        let now = chrono::Utc::now().timestamp() as u64;
        if let Some(k) = keys.iter_mut().find(|k| k.key == key) {
            self.record_general_failure(k, http_status, now);
        }
        self.record_provider_failure(keys.len() as u64, now);
    }

    /// Force a transient key cooldown after a request has already reached the
    /// upstream response body. At that point the gateway cannot safely switch
    /// streams in-band, so the key must be removed for the client's next retry.
    pub fn force_transient_cooldown(&self, key: &str, http_status: u16, reason: &str) {
        let mut keys = self.keys.write();
        let now = chrono::Utc::now().timestamp() as u64;
        if let Some(k) = keys.iter_mut().find(|k| k.key == key) {
            k.last_error_at = Some(now);
            k.last_error_status = Some(http_status);
            k.fail_count = k.fail_count.saturating_add(1);
            k.total_fail_count = k.total_fail_count.saturating_add(1);
            k.status = KeyStatus::Cooldown;
            k.cooldown_until = Some(now + self.routing.cooldown_seconds);
            k.status_updated_at = Some(now);
            tracing::warn!(
                provider = %self.provider_name,
                key = %k.masked_key(),
                status = http_status,
                cooldown_s = self.routing.cooldown_seconds,
                reason = %reason,
                "Key entering cooldown due to stream body failure"
            );
        }
        self.record_provider_failure(keys.len() as u64, now);
    }

    /// Report a failure with optional upstream retry guidance.
    pub fn report_failure_with_retry_after(
        &self,
        key: &str,
        http_status: u16,
        retry_after_seconds: Option<u64>,
    ) {
        let mut keys = self.keys.write();
        let now = chrono::Utc::now().timestamp() as u64;
        if let Some(k) = keys.iter_mut().find(|k| k.key == key) {
            k.last_error_at = Some(now);
            k.last_error_status = Some(http_status);
            match http_status {
                401 | 403 => {
                    // Auth failure → permanently disabled
                    k.status = KeyStatus::Disabled;
                    k.status_updated_at = Some(now);
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
                    k.reset_rate_windows(now);
                    let now_min = now / 60;
                    let now_day = now / 86400;
                    let observed_minute =
                        current_minute_count(k.rpm_window_start, k.rpm_count, now_min);
                    let observed_day = current_day_count(k.rpd_window_start, k.rpd_count, now_day);
                    match retry_after_seconds {
                        Some(seconds) if seconds <= 300 => {
                            if can_observed_limit_update(&k.rpm_limit_source) {
                                tighten_observed_limit(&mut k.rpm_limit, observed_minute);
                                if k.rpm_limit.is_some() {
                                    k.rpm_limit_source = Some("runtime_429".to_string());
                                }
                            }
                        }
                        Some(_) | None => {
                            if can_observed_limit_update(&k.rpd_limit_source) {
                                tighten_observed_limit(&mut k.rpd_limit, observed_day);
                                if k.rpd_limit.is_some() {
                                    k.rpd_limit_source = Some("runtime_429".to_string());
                                }
                            }
                        }
                    }
                    let tier = (k.fail_count as usize).min(COOLDOWN_ESCALATION_S.len() - 1);
                    let cooldown_source = if retry_after_seconds.is_some() {
                        "upstream_retry_after"
                    } else {
                        "local_escalation"
                    };
                    let cooldown_s = retry_after_seconds.unwrap_or(COOLDOWN_ESCALATION_S[tier]);
                    k.status = KeyStatus::RateLimited;
                    k.cooldown_until = Some(now + cooldown_s);
                    k.status_updated_at = Some(now);
                    k.fail_count += 1;
                    k.total_fail_count += 1;
                    tracing::warn!(
                        provider = %self.provider_name,
                        key = %k.masked_key(),
                        tier = tier,
                        retry_after_s = retry_after_seconds,
                        cooldown_source,
                        "Key rate limited, escalating cooldown for {}s",
                        cooldown_s
                    );
                }
                _ => {
                    self.record_general_failure(k, http_status, now);
                }
            }

            if matches!(http_status, 401 | 403 | 429) {
                return;
            }

            // Provider-level failure tracking.
            // If aggregate failures across all keys exceed threshold,
            // enter provider cooldown.
            self.record_provider_failure(keys.len() as u64, now);
        }
    }

    fn record_general_failure(&self, key: &mut KeyState, http_status: u16, now: u64) {
        key.last_error_at = Some(now);
        key.last_error_status = Some(http_status);
        key.fail_count += 1;
        key.total_fail_count += 1;

        if key.fail_count >= self.routing.fail_threshold {
            key.status = KeyStatus::Cooldown;
            key.cooldown_until = Some(now + self.routing.cooldown_seconds);
            key.status_updated_at = Some(now);
            tracing::warn!(
                provider = %self.provider_name,
                key = %key.masked_key(),
                fail_count = key.fail_count,
                "Key entering cooldown due to consecutive transient failures"
            );
        }
    }

    fn record_provider_failure(&self, key_count: u64, now: u64) {
        let prev_fails = self.provider_fail_count.fetch_add(1, Ordering::Relaxed);
        let threshold = key_count * self.routing.fail_threshold as u64;
        if prev_fails + 1 >= threshold {
            let cooldown_until = now + self.routing.cooldown_seconds;
            self.provider_cooldown_until
                .store(cooldown_until, Ordering::Relaxed);
            tracing::warn!(
                provider = %self.provider_name,
                fail_count = prev_fails + 1,
                threshold = threshold,
                cooldown_s = self.routing.cooldown_seconds,
                "Provider entering cooldown due to aggregate failures"
            );
        }
    }

    /// Get the number of available keys.
    pub fn available_count(&self) -> usize {
        let mut keys = self.keys.write();
        let now = chrono::Utc::now().timestamp() as u64;
        self.recover_expired(&mut keys);
        for key in keys.iter_mut() {
            key.reset_rate_windows(now);
        }
        keys.iter()
            .filter(|k| k.status == KeyStatus::Available && !k.is_rate_limited(now))
            .count()
    }

    /// Get the total number of keys.
    pub fn total_count(&self) -> usize {
        self.keys.read().len()
    }

    /// Get a snapshot of all key states (keys are masked).
    pub fn snapshot(&self) -> Vec<KeyState> {
        let mut keys = self.keys.write();
        self.recover_expired(&mut keys);
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
                last_success_at: k.last_success_at,
                last_error_at: k.last_error_at,
                last_error_status: k.last_error_status,
                status_updated_at: k.status_updated_at,
                last_recovered_at: k.last_recovered_at,
                success_count: k.success_count,
                total_fail_count: k.total_fail_count,
                rpm_limit: k.rpm_limit,
                rpd_limit: k.rpd_limit,
                rpm_limit_source: k.rpm_limit_source.clone(),
                rpd_limit_source: k.rpd_limit_source.clone(),
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

    /// Manually restore a key by its stable fingerprint.
    ///
    /// This is intended for auth failures (401/403), where automatic recovery is
    /// deliberately disabled. Historical error timestamps are preserved for audit.
    pub fn restore_key_by_id(&self, key_id: &str) -> GatewayResult<KeyState> {
        let mut keys = self.keys.write();
        let now = chrono::Utc::now().timestamp() as u64;
        let Some(key) = keys
            .iter_mut()
            .find(|key| key_fingerprint(&key.key) == key_id)
        else {
            return Err(GatewayError::InvalidRequest(format!(
                "Key id not found: {key_id}"
            )));
        };

        key.status = KeyStatus::Available;
        key.fail_count = 0;
        key.cooldown_until = None;
        key.status_updated_at = Some(now);
        key.last_recovered_at = Some(now);

        tracing::info!(
            provider = %self.provider_name,
            key = %key.masked_key(),
            key_id = %key_id,
            stage = "manual_key_restore",
            "Key manually restored"
        );

        Ok(KeyState {
            key: key.masked_key(),
            key_id: key_id.to_string(),
            tier: key.tier,
            models: key.models.clone(),
            models_updated_at: key.models_updated_at,
            models_last_error: key.models_last_error.clone(),
            status: key.status,
            fail_count: key.fail_count,
            cooldown_until: key.cooldown_until,
            last_success_at: key.last_success_at,
            last_error_at: key.last_error_at,
            last_error_status: key.last_error_status,
            status_updated_at: key.status_updated_at,
            last_recovered_at: key.last_recovered_at,
            success_count: key.success_count,
            total_fail_count: key.total_fail_count,
            rpm_limit: key.rpm_limit,
            rpd_limit: key.rpd_limit,
            rpm_limit_source: key.rpm_limit_source.clone(),
            rpd_limit_source: key.rpd_limit_source.clone(),
            tpm_limit: key.tpm_limit,
            tpd_limit: key.tpd_limit,
            rpm_count: key.rpm_count,
            rpd_count: key.rpd_count,
            tpm_prompt_count: key.tpm_prompt_count,
            tpm_completion_count: key.tpm_completion_count,
            tpd_prompt_count: key.tpd_prompt_count,
            tpd_completion_count: key.tpd_completion_count,
            rpm_window_start: key.rpm_window_start,
            rpd_window_start: key.rpd_window_start,
        })
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

    /// Reserve one request before sending it upstream.
    pub fn reserve_key(&self, provider_name: &str, key: &str) -> bool {
        self.pools
            .get(provider_name)
            .map(|pool| pool.reserve_key(key))
            .unwrap_or(false)
    }

    /// Report success for a provider key whose request was already reserved.
    pub fn report_reserved_success(
        &self,
        provider_name: &str,
        key: &str,
        prompt_tokens: Option<u32>,
        completion_tokens: Option<u32>,
    ) {
        if let Some(pool) = self.pools.get(provider_name) {
            pool.report_reserved_success(key, prompt_tokens, completion_tokens);
        }
    }

    /// Report failure for a provider's key.
    pub fn report_failure(&self, provider_name: &str, key: &str, http_status: u16) {
        self.report_failure_with_retry_after(provider_name, key, http_status, None);
    }

    /// Report a transient provider/key failure that should not disable auth.
    pub fn report_transient_failure(&self, provider_name: &str, key: &str, http_status: u16) {
        if let Some(pool) = self.pools.get(provider_name) {
            pool.report_transient_failure(key, http_status);
        }
    }

    /// Force a transient key cooldown when the response body fails after a stream was accepted.
    pub fn force_transient_cooldown(
        &self,
        provider_name: &str,
        key: &str,
        http_status: u16,
        reason: &str,
    ) {
        if let Some(pool) = self.pools.get(provider_name) {
            pool.force_transient_cooldown(key, http_status, reason);
        }
    }

    /// Report failure with optional upstream retry guidance.
    pub fn report_failure_with_retry_after(
        &self,
        provider_name: &str,
        key: &str,
        http_status: u16,
        retry_after_seconds: Option<u64>,
    ) {
        if let Some(pool) = self.pools.get(provider_name) {
            pool.report_failure_with_retry_after(key, http_status, retry_after_seconds);
        }
    }

    /// Report a structured upstream error, preserving auth vs WAF/Cloudflare semantics.
    pub fn report_gateway_error(&self, provider_name: &str, key: &str, error: &GatewayError) {
        if !error.is_key_attributable_failure() {
            tracing::debug!(
                provider = %provider_name,
                key_id = %key_fingerprint(key),
                http_status = error.http_status(),
                error_category = error.category(),
                "Skipping key state penalty for non-key-attributable upstream error"
            );
            return;
        }
        let status = error.http_status();
        if status == 429 || error.is_auth_failure() {
            self.report_failure_with_retry_after(
                provider_name,
                key,
                status,
                error.retry_after_seconds(),
            );
        } else {
            self.report_transient_failure(provider_name, key, status);
        }
    }

    /// Restore persisted key metadata for a registered provider.
    pub fn restore_provider_states(&self, provider_name: &str, states: &[KeyState]) {
        if let Some(pool) = self.pools.get(provider_name) {
            pool.restore_states(states);
        }
    }

    /// Manually restore a key by provider and key fingerprint.
    pub fn restore_key(&self, provider_name: &str, key_id: &str) -> GatewayResult<KeyState> {
        self.pools
            .get(provider_name)
            .ok_or_else(|| GatewayError::ProviderNotFound(provider_name.to_string()))?
            .restore_key_by_id(key_id)
    }

    /// Return raw key material for a provider for internal rule sync.
    pub fn provider_keys(&self, provider_name: &str) -> Vec<String> {
        self.pools
            .get(provider_name)
            .map(|pool| pool.keys.read().iter().map(|key| key.key.clone()).collect())
            .unwrap_or_default()
    }

    /// Return raw key material by stable fingerprint for explicit admin validation.
    pub fn key_by_id(&self, provider_name: &str, key_id: &str) -> GatewayResult<String> {
        self.pools
            .get(provider_name)
            .ok_or_else(|| GatewayError::ProviderNotFound(provider_name.to_string()))?
            .keys
            .read()
            .iter()
            .find(|key| key_fingerprint(&key.key) == key_id)
            .map(|key| key.key.clone())
            .ok_or_else(|| GatewayError::InvalidRequest(format!("Key id not found: {key_id}")))
    }

    /// Return the discovered model inventory for one raw key.
    pub fn models_for_key(&self, provider_name: &str, key: &str) -> Vec<String> {
        self.pools
            .get(provider_name)
            .and_then(|pool| {
                pool.keys
                    .read()
                    .iter()
                    .find(|state| state.key == key)
                    .map(|state| state.models.clone())
            })
            .unwrap_or_default()
    }

    /// Return the discovered model inventory for one key fingerprint.
    pub fn models_for_key_id(&self, provider_name: &str, key_id: &str) -> Vec<String> {
        self.pools
            .get(provider_name)
            .and_then(|pool| {
                pool.keys
                    .read()
                    .iter()
                    .find(|state| key_fingerprint(&state.key) == key_id)
                    .map(|state| state.models.clone())
            })
            .unwrap_or_default()
    }

    /// Apply discovered request limits for a key without touching config files.
    pub fn apply_request_limits(
        &self,
        provider_name: &str,
        key: &str,
        rpm_limit: Option<u32>,
        rpd_limit: Option<u32>,
        source: &str,
    ) -> bool {
        let Some(pool) = self.pools.get(provider_name) else {
            return false;
        };
        let mut keys = pool.keys.write();
        let Some(state) = keys.iter_mut().find(|state| state.key == key) else {
            return false;
        };
        if let Some(limit) = rpm_limit {
            set_request_limit(
                &mut state.rpm_limit,
                &mut state.rpm_limit_source,
                limit,
                source,
            );
        }
        if let Some(limit) = rpd_limit {
            set_request_limit(
                &mut state.rpd_limit,
                &mut state.rpd_limit_source,
                limit,
                source,
            );
        }
        true
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

    /// Summarize key status for diagnostics without exposing key material.
    pub fn provider_key_status_summary(&self, provider_name: &str) -> String {
        self.pools
            .get(provider_name)
            .map(|pool| {
                let now = chrono::Utc::now().timestamp() as u64;
                let mut keys = pool.keys.write();
                pool.recover_expired(&mut keys);
                for key in keys.iter_mut() {
                    key.reset_rate_windows(now);
                }

                let provider_cooldown_until =
                    pool.provider_cooldown_until.load(Ordering::Relaxed);
                let provider_cooldown_remaining = provider_cooldown_until.saturating_sub(now);
                let mut available = 0usize;
                let mut free_available = 0usize;
                let mut free_probing = 0usize;
                let mut free_cooldown = 0usize;
                let mut free_rate_limited = 0usize;
                let mut free_disabled = 0usize;
                let mut paid_or_unknown = 0usize;

                for key in keys.iter() {
                    if key.status == KeyStatus::Available && !key.is_rate_limited(now) {
                        available += 1;
                    }
                    if key.tier != KeyTier::Free {
                        paid_or_unknown += 1;
                        continue;
                    }
                    match key.status {
                        KeyStatus::Available if !key.is_rate_limited(now) => free_available += 1,
                        KeyStatus::Available => free_rate_limited += 1,
                        KeyStatus::Probing => free_probing += 1,
                        KeyStatus::Cooldown => free_cooldown += 1,
                        KeyStatus::RateLimited => free_rate_limited += 1,
                        KeyStatus::Disabled => free_disabled += 1,
                    }
                }

                format!(
                    "available={}, free_available={}, free_probing={}, free_cooldown={}, free_rate_limited={}, free_disabled={}, paid_or_unknown={}, provider_cooldown_remaining_s={}",
                    available,
                    free_available,
                    free_probing,
                    free_cooldown,
                    free_rate_limited,
                    free_disabled,
                    paid_or_unknown,
                    provider_cooldown_remaining
                )
            })
            .unwrap_or_else(|| "provider_not_registered".to_string())
    }

    pub fn discovery_keys(&self, provider_name: &str) -> Vec<(String, KeyTier)> {
        self.pools
            .get(provider_name)
            .map(|pool| {
                let now = chrono::Utc::now().timestamp() as u64;
                let mut keys = pool.keys.write();
                pool.recover_expired(&mut keys);
                for key in keys.iter_mut() {
                    key.reset_rate_windows(now);
                }
                keys.iter()
                    .filter(|key| key.status == KeyStatus::Available && !key.is_rate_limited(now))
                    .map(|key| (key.key.clone(), key.tier))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn model_probe_keys(&self, provider_name: &str) -> Vec<(String, KeyTier)> {
        self.pools
            .get(provider_name)
            .map(|pool| {
                let now = chrono::Utc::now().timestamp() as u64;
                let mut keys = pool.keys.write();
                pool.recover_expired(&mut keys);
                for key in keys.iter_mut() {
                    key.reset_rate_windows(now);
                }
                keys.iter()
                    .filter(|key| {
                        (key.status == KeyStatus::Available && !key.is_rate_limited(now))
                            || key.status == KeyStatus::Disabled
                    })
                    .map(|key| (key.key.clone(), key.tier))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn key_status(&self, provider_name: &str, key: &str) -> Option<KeyStatus> {
        self.pools.get(provider_name).and_then(|pool| {
            pool.keys
                .read()
                .iter()
                .find(|state| state.key == key)
                .map(|state| state.status)
        })
    }

    pub fn update_models(&self, provider_name: &str, key: &str, mut models: Vec<String>) {
        models.sort();
        models.dedup();
        if let Some(pool) = self.pools.get(provider_name) {
            let mut keys = pool.keys.write();
            if let Some(state) = keys.iter_mut().find(|state| state.key == key) {
                let should_recover_disabled =
                    state.status == KeyStatus::Disabled && !models.is_empty();
                state.models = models;
                let now = chrono::Utc::now().timestamp();
                state.models_updated_at = Some(now);
                state.models_last_error.clear();
                if should_recover_disabled {
                    state.status = KeyStatus::Available;
                    state.fail_count = 0;
                    state.cooldown_until = None;
                    state.status_updated_at = Some(now as u64);
                    state.last_recovered_at = Some(now as u64);
                    tracing::info!(
                        provider = %provider_name,
                        key = %state.masked_key(),
                        stage = "model_discovery_recovery",
                        "Key restored after successful model discovery"
                    );
                }
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

    pub fn free_candidates(
        &self,
        provider_name: &str,
        model: &str,
        agent_name: Option<&str>,
    ) -> Vec<String> {
        let mut candidates = self.free_candidate_infos(provider_name, model);
        match self.routing.strategy {
            RoutingStrategy::LeastRate => {
                candidates.sort_by_key(|candidate| candidate.usage_sort_key);
            }
            RoutingStrategy::LeastFailed => {
                candidates.sort_by_key(|candidate| {
                    (
                        candidate.fail_count,
                        candidate.total_fail_count.min(u32::MAX as u64) as u32,
                    )
                });
            }
            RoutingStrategy::Priority => {}
            RoutingStrategy::RoundRobin => {
                if candidates.len() > 1 {
                    let counter = ROUND_ROBIN_COUNTER.fetch_add(1, Ordering::Relaxed);
                    let shift = (counter as usize) % candidates.len();
                    candidates.rotate_left(shift);
                }
            }
            RoutingStrategy::Random => {
                if candidates.len() > 1 {
                    let shift = rand::random::<usize>() % candidates.len();
                    candidates.rotate_left(shift);
                }
            }
        }
        let mut result: Vec<String> = candidates
            .into_iter()
            .map(|candidate| candidate.key)
            .collect();

        // Agent-aware key distribution: rotate the candidate list so
        // different agents start at different keys. This prevents
        // Agent A from exhausting a single key and affecting others.
        if result.len() > 1
            && let Some(agent) = agent_name.filter(|a| !a.is_empty())
        {
            let hash: u32 = agent.bytes().fold(0u32, |a, b| a.wrapping_add(b as u32));
            let shift = (hash as usize) % result.len();
            result.rotate_left(shift);
        }

        result
    }

    pub fn free_candidate_infos(&self, provider_name: &str, model: &str) -> Vec<FreeKeyCandidate> {
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
                keys.iter()
                    .filter(|key| {
                        key.tier == KeyTier::Free
                            && is_request_selectable(key, now)
                            && key.models.iter().any(|candidate| candidate == model)
                    })
                    .map(|key| FreeKeyCandidate {
                        key: key.key.clone(),
                        usage_sort_key: key_usage_sort_key_for_selection(key, now),
                        fail_count: key.fail_count,
                        total_fail_count: key.total_fail_count,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn free_provider_candidate_infos(&self, provider_name: &str) -> Vec<FreeKeyCandidate> {
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
                            "Provider in cooldown, skipping provider-level candidate selection"
                        );
                        return vec![];
                    }
                    pool.provider_cooldown_until.store(0, Ordering::Relaxed);
                    pool.provider_fail_count.store(0, Ordering::Relaxed);
                }

                let mut keys = pool.keys.write();
                pool.recover_expired(&mut keys);
                for key in keys.iter_mut() {
                    key.reset_rate_windows(now);
                }
                keys.iter()
                    .filter(|key| key.tier == KeyTier::Free && is_request_selectable(key, now))
                    .map(|key| FreeKeyCandidate {
                        key: key.key.clone(),
                        usage_sort_key: key_usage_sort_key_for_selection(key, now),
                        fail_count: key.fail_count,
                        total_fail_count: key.total_fail_count,
                    })
                    .collect()
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

    /// Get an available paid key that can serve the requested model.
    /// Used only for last-resort paid escalation after free keys are exhausted.
    pub fn paid_candidate_for_model(&self, provider_name: &str, model: &str) -> Option<String> {
        self.pools.get(provider_name).and_then(|pool| {
            // Provider-level cooldown check
            let now = chrono::Utc::now().timestamp() as u64;
            let cooldown_until = pool.provider_cooldown_until.load(Ordering::Relaxed);
            if cooldown_until > 0 {
                if now < cooldown_until {
                    tracing::debug!(
                        provider = %provider_name,
                        "Provider in cooldown, paid_candidate_for_model returns None"
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
            keys.iter()
                .find(|k| {
                    k.tier == KeyTier::Paid
                        && k.status == KeyStatus::Available
                        && !k.is_rate_limited(now)
                        && k.models.iter().any(|candidate| candidate == model)
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
            for key in keys.iter().filter(|key| {
                key.tier == KeyTier::Free
                    && key.status == KeyStatus::Available
                    && !key.is_rate_limited(now)
            }) {
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
