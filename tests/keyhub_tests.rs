/// Integration tests for KeyHub / KeyPool behavior.
///
/// Validates key rotation, cooldown, rate-limit, and disable logic.
use free_agent_gateway::config::{KeyConfig, KeyTier, RoutingConfig, RoutingStrategy};
use free_agent_gateway::keyhub::{
    KeyHub, KeyPool, REAL_MODEL_RECOVERY_COOLDOWN_S, key_fingerprint,
};
use free_agent_gateway::models::{KeyState, KeyStatus};

fn routing_config() -> RoutingConfig {
    RoutingConfig {
        strategy: RoutingStrategy::LeastFailed,
        fail_threshold: 3,
        cooldown_seconds: 60,
        auto_discover: true,
    }
}

fn least_rate_routing_config() -> RoutingConfig {
    RoutingConfig {
        strategy: RoutingStrategy::LeastRate,
        fail_threshold: 3,
        cooldown_seconds: 60,
        auto_discover: true,
    }
}

#[test]
fn test_keypool_acquires_available_key() {
    let pool = KeyPool::new(
        "github",
        vec!["key-a".into(), "key-b".into(), "key-c".into()],
        routing_config(),
    );
    let key = pool.acquire_key().unwrap();
    assert!(key == "key-a" || key == "key-b" || key == "key-c");
    assert_eq!(pool.available_count(), 3);
}

#[test]
fn test_keypool_429_triggers_rate_limit_cooldown() {
    let pool = KeyPool::new("github", vec!["key-a".into()], routing_config());
    pool.report_failure("key-a", 429);

    let snapshot = pool.snapshot();
    assert_eq!(snapshot[0].status, KeyStatus::RateLimited);
    assert!(snapshot[0].cooldown_until.is_some());
    assert!(snapshot[0].last_error_at.is_some());
    assert_eq!(snapshot[0].last_error_status, Some(429));
    assert!(snapshot[0].status_updated_at.is_some());
    assert_eq!(pool.available_count(), 0);
}

#[test]
fn test_keypool_429_uses_retry_after_when_available() {
    let pool = KeyPool::new("github", vec!["key-a".into()], routing_config());
    let before = chrono::Utc::now().timestamp() as u64;
    pool.report_failure_with_retry_after("key-a", 429, Some(7));

    let snapshot = pool.snapshot();
    assert_eq!(snapshot[0].status, KeyStatus::RateLimited);
    let cooldown_until = snapshot[0].cooldown_until.expect("cooldown");
    assert!(cooldown_until >= before + 7);
    assert!(cooldown_until <= before + 9);
}

#[test]
fn test_429_learns_observed_rpd_limit_without_provider_cooldown() {
    let hub = KeyHub::new(routing_config());
    hub.register_provider(
        "shared",
        vec![
            KeyConfig::detailed("exhausted", KeyTier::Free),
            KeyConfig::detailed("still-available", KeyTier::Free),
        ],
    );
    hub.update_models("shared", "exhausted", vec!["model-a".into()]);
    hub.update_models("shared", "still-available", vec!["model-a".into()]);

    for _ in 0..5 {
        assert!(hub.reserve_key("shared", "exhausted"));
        hub.report_reserved_success("shared", "exhausted", None, None);
    }
    hub.report_failure_with_retry_after("shared", "exhausted", 429, None);

    let mut providers = hub.snapshot();
    let states = providers.remove(0).1;
    let exhausted = states
        .iter()
        .find(|state| state.key_id == key_fingerprint("exhausted"))
        .expect("exhausted key");
    assert_eq!(exhausted.status, KeyStatus::RateLimited);
    assert_eq!(exhausted.rpd_limit, Some(5));

    assert_eq!(
        hub.free_candidates("shared", "model-a", None),
        vec!["still-available".to_string()]
    );
}

#[test]
fn test_429_with_short_retry_after_learns_observed_rpm_limit() {
    let hub = KeyHub::new(routing_config());
    hub.register_provider("shared", vec![KeyConfig::detailed("busy", KeyTier::Free)]);

    assert!(hub.reserve_key("shared", "busy"));
    hub.report_failure_with_retry_after("shared", "busy", 429, Some(30));

    let state = hub.snapshot().remove(0).1.remove(0);
    assert_eq!(state.status, KeyStatus::RateLimited);
    assert_eq!(state.rpm_limit, Some(1));
    assert_eq!(state.rpd_limit, None);
}

#[test]
fn test_real_model_unavailable_keywide_cooldown_skips_only_that_key() {
    let hub = KeyHub::new(routing_config());
    hub.register_provider(
        "shared",
        vec![
            KeyConfig::detailed("bad-key", KeyTier::Free),
            KeyConfig::detailed("good-key", KeyTier::Free),
        ],
    );
    hub.update_models("shared", "bad-key", vec!["real-model".into()]);
    hub.update_models("shared", "good-key", vec!["real-model".into()]);

    let before = chrono::Utc::now().timestamp() as u64;
    hub.report_real_model_failure(
        "shared",
        "bad-key",
        429,
        "real-model",
        "real_model_unavailable",
        None,
    );

    let states = hub.snapshot().remove(0).1;
    let bad = states
        .iter()
        .find(|state| state.key_id == key_fingerprint("bad-key"))
        .expect("bad key state");
    assert_eq!(bad.status, KeyStatus::RateLimited);
    assert_eq!(
        bad.availability_reason.as_deref(),
        Some("real_model_unavailable")
    );
    assert_eq!(bad.availability_model.as_deref(), Some("real-model"));
    assert!(bad.cooldown_until.unwrap() >= before + REAL_MODEL_RECOVERY_COOLDOWN_S);
    assert_eq!(bad.next_probe_at, bad.cooldown_until);

    assert_eq!(
        hub.free_candidates("shared", "real-model", None),
        vec!["good-key".to_string()]
    );
}

#[test]
fn test_real_model_failure_recovers_through_probe_after_expiry() {
    let hub = KeyHub::new(routing_config());
    hub.register_provider(
        "shared",
        vec![KeyConfig::detailed("recovering", KeyTier::Free)],
    );
    hub.update_models("shared", "recovering", vec!["real-model".into()]);

    let mut persisted = KeyState::new("recovering".into());
    persisted.tier = KeyTier::Free;
    persisted.models = vec!["real-model".into()];
    persisted.status = KeyStatus::RateLimited;
    persisted.cooldown_until = Some(chrono::Utc::now().timestamp() as u64 - 1);
    persisted.next_probe_at = persisted.cooldown_until;
    persisted.availability_reason = Some("real_model_rate_limited".into());
    persisted.availability_model = Some("real-model".into());
    hub.restore_provider_states("shared", &[persisted]);

    let state = hub.snapshot().remove(0).1.remove(0);
    assert_eq!(state.status, KeyStatus::Probing);
    assert_eq!(
        hub.free_candidates("shared", "real-model", None),
        vec!["recovering".to_string()]
    );

    assert!(hub.reserve_key("shared", "recovering"));
    let state = hub.snapshot().remove(0).1.remove(0);
    assert_eq!(state.status, KeyStatus::Cooldown);
    assert!(state.cooldown_until.is_some());
}

#[test]
fn test_keypool_429_falls_back_to_local_escalation_without_retry_after() {
    let pool = KeyPool::new("github", vec!["key-a".into()], routing_config());
    let before = chrono::Utc::now().timestamp() as u64;
    pool.report_failure("key-a", 429);

    let snapshot = pool.snapshot();
    let cooldown_until = snapshot[0].cooldown_until.expect("cooldown");
    assert!(cooldown_until >= before + 30);
    assert!(cooldown_until <= before + 32);
}

#[test]
fn test_keypool_401_disables_key_permanently() {
    let pool = KeyPool::new("github", vec!["key-a".into()], routing_config());
    pool.report_failure("key-a", 401);

    let snapshot = pool.snapshot();
    assert_eq!(snapshot[0].status, KeyStatus::Disabled);
    assert!(snapshot[0].last_error_at.is_some());
    assert_eq!(snapshot[0].last_error_status, Some(401));
    assert!(snapshot[0].status_updated_at.is_some());
    assert_eq!(pool.available_count(), 0);
}

#[test]
fn test_keypool_403_disables_key_permanently() {
    let pool = KeyPool::new("github", vec!["key-a".into()], routing_config());
    pool.report_failure("key-a", 403);

    let snapshot = pool.snapshot();
    assert_eq!(snapshot[0].status, KeyStatus::Disabled);
}

#[test]
fn test_transient_403_does_not_disable_key() {
    let pool = KeyPool::new("groq", vec!["key-a".into()], routing_config());
    pool.report_transient_failure("key-a", 403);

    let snapshot = pool.snapshot();
    assert_eq!(snapshot[0].status, KeyStatus::Available);
    assert_eq!(snapshot[0].last_error_status, Some(403));
    assert_eq!(snapshot[0].total_fail_count, 1);
    assert_eq!(pool.available_count(), 1);
}

#[test]
fn test_keypool_5xx_increments_fail_count() {
    let pool = KeyPool::new("github", vec!["key-a".into()], routing_config());
    pool.report_failure("key-a", 503);

    let snapshot = pool.snapshot();
    assert_eq!(snapshot[0].status, KeyStatus::Available); // below threshold
    assert_eq!(snapshot[0].fail_count, 1);
    assert_eq!(pool.available_count(), 1);
}

#[test]
fn test_keypool_consecutive_5xx_reaches_cooldown() {
    let pool = KeyPool::new("github", vec!["key-a".into()], routing_config());
    pool.report_failure("key-a", 500);
    pool.report_failure("key-a", 500);
    // Still available after 2 (below threshold of 3)
    assert_eq!(pool.available_count(), 1);
    pool.report_failure("key-a", 500); // 3rd failure → cooldown
    assert_eq!(pool.available_count(), 0);
    let snapshot = pool.snapshot();
    assert_eq!(snapshot[0].status, KeyStatus::Cooldown);
}

#[test]
fn test_keypool_success_resets_fail_count() {
    let pool = KeyPool::new("github", vec!["key-a".into()], routing_config());
    pool.report_failure("key-a", 500);
    pool.report_failure("key-a", 500);
    pool.report_success("key-a", None, None); // resets

    let snapshot = pool.snapshot();
    assert_eq!(snapshot[0].fail_count, 0);
    assert_eq!(snapshot[0].success_count, 1);
}

#[test]
fn test_keypool_multiple_keys_failover_on_cooldown() {
    let pool = KeyPool::new(
        "github",
        vec!["key-a".into(), "key-b".into()],
        routing_config(),
    );

    // Disable key-a via rate limit
    pool.report_failure("key-a", 429);
    assert_eq!(pool.available_count(), 1);

    // Should still be able to acquire key-b
    let key = pool.acquire_key().unwrap();
    assert_eq!(key, "key-b");
}

#[test]
fn test_keyhub_register_and_acquire() {
    let hub = KeyHub::new(routing_config());
    hub.register_provider("github", vec!["k1".into(), "k2".into()]);

    assert!(hub.has_available_keys("github"));
    let key = hub.acquire_key("github").unwrap();
    assert!(key == "k1" || key == "k2");
}

#[test]
fn test_keyhub_unknown_provider_errors() {
    let hub = KeyHub::new(routing_config());
    let result = hub.acquire_key("nonexistent");
    assert!(result.is_err());
}

#[test]
fn test_keyhub_report_failure_updates_pool() {
    let hub = KeyHub::new(routing_config());
    hub.register_provider("github", vec!["k1".into()]);

    hub.report_failure("github", "k1", 401);
    assert!(!hub.has_available_keys("github")); // disabled
}

#[test]
fn test_keyhub_snapshot() {
    let hub = KeyHub::new(routing_config());
    hub.register_provider("github", vec!["k1".into(), "k2".into()]);
    hub.register_provider("nvidia", vec!["nv1".into()]);

    let snapshot = hub.snapshot();
    assert_eq!(snapshot.len(), 2);

    let github = snapshot.iter().find(|(n, _)| n == "github").unwrap();
    assert_eq!(github.1.len(), 2);
}

#[test]
fn test_round_robin_strategy() {
    let routing = RoutingConfig {
        strategy: RoutingStrategy::RoundRobin,
        fail_threshold: 3,
        cooldown_seconds: 60,
        auto_discover: true,
    };
    let pool = KeyPool::new("test", vec!["a".into(), "b".into(), "c".into()], routing);

    // Acquire multiple keys; round-robin should cycle through them
    let mut acquired = Vec::new();
    for _ in 0..6 {
        acquired.push(pool.acquire_key().unwrap());
    }
    // All three keys should appear
    assert!(acquired.contains(&"a".to_string()));
    assert!(acquired.contains(&"b".to_string()));
    assert!(acquired.contains(&"c".to_string()));
}

#[test]
fn test_rate_limited_key_recovers_after_expiry() {
    let pool = KeyPool::new("github", vec!["key-a".into()], routing_config());
    let mut persisted = KeyState::new("key-a".into());
    persisted.status = KeyStatus::RateLimited;
    persisted.cooldown_until = Some(chrono::Utc::now().timestamp() as u64 - 1);

    pool.restore_states(&[persisted]);

    assert_eq!(pool.available_count(), 0);
    let snapshot = pool.snapshot();
    assert_eq!(snapshot[0].status, KeyStatus::Probing);
    assert!(snapshot[0].last_recovered_at.is_some());
    assert_eq!(pool.acquire_key().unwrap(), "key-a");
}

#[test]
fn test_cooldown_key_recovers_during_availability_check() {
    let pool = KeyPool::new("github", vec!["key-a".into()], routing_config());
    let mut persisted = KeyState::new("key-a".into());
    persisted.status = KeyStatus::Cooldown;
    persisted.fail_count = 3;
    persisted.cooldown_until = Some(chrono::Utc::now().timestamp() as u64 - 1);

    pool.restore_states(&[persisted]);

    assert_eq!(pool.available_count(), 1);
    let state = pool.snapshot().remove(0);
    assert_eq!(state.status, KeyStatus::Available);
    assert_eq!(state.fail_count, 0);
    assert_eq!(state.cooldown_until, None);
}

#[test]
fn test_disabled_key_does_not_recover() {
    let pool = KeyPool::new("github", vec!["key-a".into()], routing_config());
    let mut persisted = KeyState::new("key-a".into());
    persisted.status = KeyStatus::Disabled;
    persisted.cooldown_until = Some(chrono::Utc::now().timestamp() as u64 - 1);

    pool.restore_states(&[persisted]);

    assert_eq!(pool.available_count(), 0);
    assert!(pool.acquire_key().is_err());
}

#[test]
fn test_keyhub_reports_exact_available_key_count() {
    let hub = KeyHub::new(routing_config());
    hub.register_provider("github", vec!["key-a".into(), "key-b".into()]);
    hub.report_failure("github", "key-a", 401);

    assert_eq!(hub.available_count("github"), 1);
}

#[test]
fn test_keyhub_manually_restores_disabled_key_by_id() {
    let hub = KeyHub::new(routing_config());
    hub.register_provider("github", vec!["key-a".into()]);
    hub.report_failure("github", "key-a", 401);

    let restored = hub
        .restore_key("github", &key_fingerprint("key-a"))
        .expect("restore disabled key");

    assert_eq!(restored.status, KeyStatus::Available);
    assert_eq!(restored.fail_count, 0);
    assert_eq!(restored.cooldown_until, None);
    assert_eq!(restored.last_error_status, Some(401));
    assert!(restored.last_error_at.is_some());
    assert!(restored.last_recovered_at.is_some());
    assert_eq!(hub.available_count("github"), 1);
    assert_eq!(hub.acquire_key("github").unwrap(), "key-a");
}

#[test]
fn test_discovery_keys_skip_unavailable_keys() {
    let hub = KeyHub::new(routing_config());
    hub.register_provider(
        "github",
        vec![
            KeyConfig::detailed("available", KeyTier::Free),
            KeyConfig::detailed("disabled", KeyTier::Free),
            KeyConfig::detailed("limited", KeyTier::Free),
            KeyConfig::detailed("cooldown", KeyTier::Free),
        ],
    );
    hub.report_failure("github", "disabled", 401);
    hub.report_failure("github", "limited", 429);
    hub.report_failure("github", "cooldown", 500);
    hub.report_failure("github", "cooldown", 500);
    hub.report_failure("github", "cooldown", 500);

    assert_eq!(
        hub.discovery_keys("github"),
        vec![("available".to_string(), KeyTier::Free)]
    );
}

#[test]
fn test_reserve_key_enforces_rpm_and_rpd_limits() {
    let hub = KeyHub::new(routing_config());
    hub.register_provider(
        "openrouter",
        vec![KeyConfig::Detailed {
            value: "free-1000".into(),
            tier: KeyTier::Free,
            rpm_limit: Some(1),
            rpd_limit: Some(1),
            tpm_limit: None,
            tpd_limit: None,
        }],
    );

    assert!(hub.reserve_key("openrouter", "free-1000"));
    assert!(!hub.reserve_key("openrouter", "free-1000"));

    let state = hub.snapshot().remove(0).1.remove(0);
    assert_eq!(state.rpm_count, 1);
    assert_eq!(state.rpd_count, 1);
}

#[test]
fn test_reserved_success_does_not_double_count_request_usage() {
    let hub = KeyHub::new(routing_config());
    hub.register_provider("openrouter", vec![limited_free_key("free-1000", 10, 1000)]);

    assert!(hub.reserve_key("openrouter", "free-1000"));
    hub.report_reserved_success("openrouter", "free-1000", Some(11), Some(7));

    let state = hub.snapshot().remove(0).1.remove(0);
    assert_eq!(state.success_count, 1);
    assert_eq!(state.rpm_count, 1);
    assert_eq!(state.rpd_count, 1);
    assert_eq!(state.tpm_prompt_count, 11);
    assert_eq!(state.tpm_completion_count, 7);
    assert_eq!(state.tpd_prompt_count, 11);
    assert_eq!(state.tpd_completion_count, 7);
}

fn tiered_key(value: &str, tier: KeyTier) -> KeyConfig {
    KeyConfig::detailed(value, tier)
}

fn limited_free_key(value: &str, rpm_limit: u32, tpd_limit: u32) -> KeyConfig {
    KeyConfig::Detailed {
        value: value.into(),
        tier: KeyTier::Free,
        rpm_limit: Some(rpm_limit),
        rpd_limit: None,
        tpm_limit: None,
        tpd_limit: Some(tpd_limit),
    }
}

fn limited_request_key(value: &str, rpm_limit: u32, rpd_limit: u32) -> KeyConfig {
    KeyConfig::Detailed {
        value: value.into(),
        tier: KeyTier::Free,
        rpm_limit: Some(rpm_limit),
        rpd_limit: Some(rpd_limit),
        tpm_limit: None,
        tpd_limit: None,
    }
}

#[test]
fn test_keys_under_one_provider_keep_independent_model_inventories() {
    let hub = KeyHub::new(routing_config());
    hub.register_provider(
        "shared",
        vec![
            tiered_key("free-a", KeyTier::Free),
            tiered_key("free-b", KeyTier::Free),
        ],
    );

    hub.update_models("shared", "free-a", vec!["model-a".into()]);
    hub.update_models("shared", "free-b", vec!["model-b".into()]);

    assert_eq!(
        hub.free_candidates("shared", "model-a", None),
        vec!["free-a".to_string()]
    );
    assert_eq!(
        hub.free_candidates("shared", "model-b", None),
        vec!["free-b".to_string()]
    );
}

#[test]
fn test_paid_and_unknown_keys_are_not_free_candidates() {
    let hub = KeyHub::new(routing_config());
    hub.register_provider(
        "shared",
        vec![
            tiered_key("paid", KeyTier::Paid),
            KeyConfig::Legacy("legacy".into()),
        ],
    );
    hub.update_models("shared", "paid", vec!["same-model".into()]);
    hub.update_models("shared", "legacy", vec!["same-model".into()]);

    assert!(hub.free_candidates("shared", "same-model", None).is_empty());
}

#[test]
fn test_paid_candidate_for_model_only_returns_matching_paid_key() {
    let hub = KeyHub::new(routing_config());
    hub.register_provider(
        "shared",
        vec![
            tiered_key("free", KeyTier::Free),
            tiered_key("paid-a", KeyTier::Paid),
            tiered_key("paid-b", KeyTier::Paid),
            KeyConfig::Legacy("legacy".into()),
        ],
    );
    hub.update_models("shared", "free", vec!["model-a".into()]);
    hub.update_models("shared", "paid-a", vec!["model-a".into()]);
    hub.update_models("shared", "paid-b", vec!["model-b".into()]);
    hub.update_models("shared", "legacy", vec!["model-a".into()]);

    assert_eq!(
        hub.paid_candidate_for_model("shared", "model-a"),
        Some("paid-a".to_string())
    );
    assert_eq!(
        hub.paid_candidate_for_model("shared", "model-b"),
        Some("paid-b".to_string())
    );
    assert_eq!(hub.paid_candidate_for_model("shared", "model-c"), None);
}

#[test]
fn test_key_capability_snapshot_preserves_tier_and_models() {
    let hub = KeyHub::new(routing_config());
    hub.register_provider("shared", vec![tiered_key("free-a", KeyTier::Free)]);
    hub.update_models(
        "shared",
        "free-a",
        vec!["model-b".into(), "model-a".into(), "model-a".into()],
    );

    let state = hub.snapshot().remove(0).1.remove(0);

    assert_eq!(state.tier, KeyTier::Free);
    assert_eq!(state.models, vec!["model-a", "model-b"]);
    assert!(state.models_updated_at.is_some());
}

#[test]
fn test_persisted_rate_usage_is_restored_for_matching_key() {
    let source = KeyHub::new(routing_config());
    source.register_provider("shared", vec![limited_free_key("free-a", 10, 1000)]);
    source.update_models("shared", "free-a", vec!["model-a".into()]);
    source.report_success("shared", "free-a", Some(123), Some(45));

    let persisted = source.snapshot().remove(0).1;
    let restored = KeyHub::new(routing_config());
    restored.register_provider("shared", vec![limited_free_key("free-a", 10, 1000)]);
    restored.restore_provider_states("shared", &persisted);

    let state = restored.snapshot().remove(0).1.remove(0);

    assert_eq!(state.success_count, 1);
    assert_eq!(state.rpm_count, 1);
    assert_eq!(state.rpd_count, 1);
    assert_eq!(state.tpm_prompt_count, 123);
    assert_eq!(state.tpm_completion_count, 45);
    assert_eq!(state.tpd_prompt_count, 123);
    assert_eq!(state.tpd_completion_count, 45);
}

#[test]
fn test_least_rate_free_candidates_prefer_key_with_more_headroom() {
    let hub = KeyHub::new(least_rate_routing_config());
    hub.register_provider(
        "shared",
        vec![
            limited_free_key("busy", 10, 1000),
            limited_free_key("quiet", 10, 1000),
        ],
    );
    hub.update_models("shared", "busy", vec!["model-a".into()]);
    hub.update_models("shared", "quiet", vec!["model-a".into()]);

    for _ in 0..8 {
        hub.report_success("shared", "busy", Some(1), Some(1));
    }

    assert_eq!(
        hub.free_candidates("shared", "model-a", None),
        vec!["quiet".to_string(), "busy".to_string()]
    );
}

#[test]
fn test_least_rate_without_known_limits_prefers_less_used_key() {
    let hub = KeyHub::new(least_rate_routing_config());
    hub.register_provider(
        "shared",
        vec![
            tiered_key("observed-busy", KeyTier::Free),
            tiered_key("observed-quiet", KeyTier::Free),
        ],
    );
    hub.update_models("shared", "observed-busy", vec!["model-a".into()]);
    hub.update_models("shared", "observed-quiet", vec!["model-a".into()]);

    for _ in 0..3 {
        hub.report_success("shared", "observed-busy", None, None);
    }

    assert_eq!(
        hub.free_candidates("shared", "model-a", None),
        vec!["observed-quiet".to_string(), "observed-busy".to_string()]
    );
}

#[test]
fn test_least_rate_uses_higher_quota_key_by_usage_percentage() {
    let hub = KeyHub::new(least_rate_routing_config());
    hub.register_provider(
        "shared",
        vec![
            limited_request_key("normal-free", 20, 100),
            limited_request_key("topped-up-free", 60, 1000),
        ],
    );
    hub.update_models("shared", "normal-free", vec!["free-model".into()]);
    hub.update_models("shared", "topped-up-free", vec!["free-model".into()]);

    for _ in 0..5 {
        hub.report_success("shared", "normal-free", None, None);
    }
    for _ in 0..10 {
        hub.report_success("shared", "topped-up-free", None, None);
    }

    assert_eq!(
        hub.free_candidates("shared", "free-model", None),
        vec!["topped-up-free".to_string(), "normal-free".to_string()]
    );
}

#[test]
fn test_least_failed_prefers_higher_quota_key_when_failures_equal() {
    let hub = KeyHub::new(routing_config());
    hub.register_provider(
        "openrouter",
        vec![
            limited_request_key("normal-free", 20, 50),
            limited_request_key("topped-up-free", 20, 1000),
        ],
    );
    hub.update_models(
        "openrouter",
        "normal-free",
        vec!["qwen/qwen3-coder:free".into()],
    );
    hub.update_models(
        "openrouter",
        "topped-up-free",
        vec!["qwen/qwen3-coder:free".into()],
    );

    assert_eq!(
        hub.free_candidates("openrouter", "qwen/qwen3-coder:free", None),
        vec!["topped-up-free".to_string(), "normal-free".to_string()]
    );
}

#[test]
fn test_official_limits_are_not_tightened_by_429_observation() {
    let hub = KeyHub::new(routing_config());
    hub.register_provider("shared", vec![tiered_key("official", KeyTier::Free)]);
    assert!(hub.apply_request_limits("shared", "official", Some(20), Some(1000), "official_api"));

    for _ in 0..7 {
        assert!(hub.reserve_key("shared", "official"));
        hub.report_reserved_success("shared", "official", None, None);
    }
    hub.report_failure_with_retry_after("shared", "official", 429, None);

    let state = hub.snapshot().remove(0).1.remove(0);
    assert_eq!(state.rpd_limit, Some(1000));
    assert_eq!(state.rpd_limit_source.as_deref(), Some("official_api"));
    assert_eq!(state.status, KeyStatus::RateLimited);
}
