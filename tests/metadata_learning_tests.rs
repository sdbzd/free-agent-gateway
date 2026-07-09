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

#[test]
fn invalid_model_errors_are_tracked_as_not_found() {
    assert_eq!(
        ModelMetaStore::classify_error("deepseek-v4-flash:free is not a valid model ID", 400),
        "not_found"
    );
    assert_eq!(
        ModelMetaStore::classify_error("AiError: No such model deepseek-v4-flash", 400),
        "not_found"
    );
}

#[test]
fn successful_requests_accumulate_model_token_usage() {
    let path = std::env::temp_dir().join(format!(
        "free-agent-gateway-meta-success-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let store = ModelMetaStore::open(&path).unwrap();

    store.learn_from_request("provider-a", "model-a", true, Some(11), Some(7));
    store.learn_from_request("provider-a", "model-a", true, Some(13), Some(5));

    let usage = store.get_usage_summary(1).unwrap();
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].provider, "provider-a");
    assert_eq!(usage[0].model_id, "model-a");
    assert_eq!(usage[0].total_requests, 2);
    assert_eq!(usage[0].total_prompt_tokens, 24);
    assert_eq!(usage[0].total_completion_tokens, 12);
    assert_eq!(usage[0].total_success, 2);
    assert_eq!(usage[0].total_errors, 0);

    drop(store);
    let _ = std::fs::remove_file(&path);
}
