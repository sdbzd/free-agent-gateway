use free_agent_gateway::metadata::ModelMetaStore;

#[test]
fn rate_limit_failure_learns_quota_and_cooldown_hints() {
    let path =
        std::env::temp_dir().join(format!("free-agent-gateway-meta-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let store = ModelMetaStore::open(&path).unwrap();

    store
        .upsert_model(
            "provider-a",
            "model-a",
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            "test",
        )
        .unwrap();

    store.learn_from_failure(
        "provider-a",
        "model-a",
        "Rate limit exceeded. Limit 30 RPM, requested 31. Try again in 2 minutes.",
        429,
    );

    let model = store.get_model("provider-a", "model-a").unwrap().unwrap();
    let errors = store.get_error_summary(1).unwrap();
    let usage = store.get_usage_summary(1).unwrap();

    assert_eq!(model.rpm_limit, Some(30));
    assert!(store.learned_rate_limit_count().unwrap() >= 2);
    assert_eq!(errors[0].category, "rate_limit");
    assert_eq!(usage[0].total_errors, 1);

    drop(store);
    let _ = std::fs::remove_file(&path);
}
