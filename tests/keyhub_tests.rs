/// Integration tests for KeyHub / KeyPool behavior.
///
/// Validates key rotation, cooldown, rate-limit, and disable logic.
use agent_gateway::config::{KeyConfig, KeyTier, RoutingConfig, RoutingStrategy};
use agent_gateway::keyhub::{KeyHub, KeyPool};
use agent_gateway::models::{KeyState, KeyStatus};

fn routing_config() -> RoutingConfig {
    RoutingConfig {
        strategy: RoutingStrategy::LeastFailed,
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
    assert_eq!(pool.available_count(), 0);
}

#[test]
fn test_keypool_401_disables_key_permanently() {
    let pool = KeyPool::new("github", vec!["key-a".into()], routing_config());
    pool.report_failure("key-a", 401);

    let snapshot = pool.snapshot();
    assert_eq!(snapshot[0].status, KeyStatus::Disabled);
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

    assert!(pool.available_count() > 0);
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

fn tiered_key(value: &str, tier: KeyTier) -> KeyConfig {
    KeyConfig::detailed(value, tier)
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
