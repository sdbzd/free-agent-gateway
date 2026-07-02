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
fn request_attempts_update_deployment_state() {
    let path = std::env::temp_dir().join(format!(
        "free-agent-gateway-attempts-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let store = ModelMetaStore::open(&path).unwrap();

    store
        .record_request_attempt(
            "req-1",
            1,
            "openrouter",
            "google/gemma:free",
            "key-a",
            false,
            "rate_limited",
            Some(429),
            Some("rate limit"),
            Some(600),
            true,
        )
        .unwrap();
    store
        .record_request_attempt(
            "req-1",
            2,
            "opencode",
            "google/gemma:free",
            "key-b",
            true,
            "success",
            Some(200),
            None,
            None,
            false,
        )
        .unwrap();

    let attempts = store.get_recent_attempts(10).unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].request_id, "req-1");
    assert_eq!(attempts[0].attempt_index, 2);
    assert_eq!(attempts[0].provider, "opencode");
    assert!(attempts[0].success);
    assert!(!attempts[0].fallback);
    assert_eq!(attempts[1].provider, "openrouter");
    assert_eq!(attempts[1].error_category.as_deref(), Some("rate_limited"));
    assert!(attempts[1].fallback);

    let states = store.get_deployment_states().unwrap();
    let limited = states
        .iter()
        .find(|state| state.provider == "openrouter")
        .unwrap();
    assert_eq!(limited.key_id, "key-a");
    assert_eq!(limited.error_count, 1);
    assert_eq!(limited.success_count, 0);
    assert_eq!(limited.consecutive_failures, 1);
    assert_eq!(limited.last_error_category.as_deref(), Some("rate_limited"));
    assert!(limited.cooldown_until.is_some());

    let healthy = states
        .iter()
        .find(|state| state.provider == "opencode")
        .unwrap();
    assert_eq!(healthy.success_count, 1);
    assert_eq!(healthy.error_count, 0);
    assert_eq!(healthy.consecutive_failures, 0);
    assert_eq!(healthy.cooldown_until, None);

    drop(store);
    let _ = std::fs::remove_file(&path);
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
    assert_eq!(usage[0].token_reported_requests, 2);
    assert_eq!(usage[0].total_success, 2);
    assert_eq!(usage[0].total_errors, 0);

    let daily = store.get_usage_daily_summary(7).unwrap();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let today_row = daily.iter().find(|row| row.date == today).unwrap();
    assert_eq!(daily.len(), 7);
    assert_eq!(today_row.total_requests, 2);
    assert_eq!(today_row.total_prompt_tokens, 24);
    assert_eq!(today_row.total_completion_tokens, 12);
    assert_eq!(today_row.token_reported_requests, 2);

    let hourly = store.get_usage_hourly_summary(24).unwrap();
    assert_eq!(hourly.len(), 24);
    let active_hour = hourly
        .iter()
        .find(|row| row.total_requests == 2)
        .expect("active hourly bucket");
    assert_eq!(active_hour.total_prompt_tokens, 24);
    assert_eq!(active_hour.total_completion_tokens, 12);

    let lifetime = store.get_usage_lifetime().unwrap();
    assert_eq!(lifetime.total_requests, 2);
    assert_eq!(lifetime.total_prompt_tokens, 24);
    assert_eq!(lifetime.total_completion_tokens, 12);
    assert_eq!(lifetime.token_reported_requests, 2);

    drop(store);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn daily_usage_marks_requests_without_reported_tokens() {
    let path = std::env::temp_dir().join(format!(
        "free-agent-gateway-meta-daily-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let store = ModelMetaStore::open(&path).unwrap();

    store.learn_from_request("provider-a", "model-a", true, None, None);
    store.learn_from_request("provider-a", "model-a", true, Some(9), Some(3));

    let usage = store.get_usage_summary(7).unwrap();
    assert_eq!(usage[0].total_requests, 2);
    assert_eq!(usage[0].token_reported_requests, 1);

    let daily = store.get_usage_daily_summary(7).unwrap();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let today_row = daily.iter().find(|row| row.date == today).unwrap();
    assert_eq!(today_row.total_requests, 2);
    assert_eq!(today_row.token_reported_requests, 1);
    assert_eq!(today_row.token_reporting_coverage, Some(0.5));

    drop(store);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn usage_splits_reported_and_estimated_token_totals() {
    let path = std::env::temp_dir().join(format!(
        "free-agent-gateway-meta-token-source-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let store = ModelMetaStore::open(&path).unwrap();

    store.learn_from_request_with_token_source(
        "provider-a",
        "model-a",
        true,
        Some(10),
        Some(5),
        true,
    );
    store.learn_from_request_with_token_source(
        "provider-a",
        "model-a",
        true,
        Some(20),
        Some(7),
        false,
    );

    let usage = store.get_usage_summary(7).unwrap();
    assert_eq!(usage[0].total_prompt_tokens, 30);
    assert_eq!(usage[0].total_completion_tokens, 12);
    assert_eq!(usage[0].reported_prompt_tokens, 10);
    assert_eq!(usage[0].reported_completion_tokens, 5);
    assert_eq!(usage[0].estimated_prompt_tokens, 20);
    assert_eq!(usage[0].estimated_completion_tokens, 7);

    let daily = store.get_usage_daily_summary(7).unwrap();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let today_row = daily.iter().find(|row| row.date == today).unwrap();
    assert_eq!(today_row.reported_prompt_tokens, 10);
    assert_eq!(today_row.reported_completion_tokens, 5);
    assert_eq!(today_row.estimated_prompt_tokens, 20);
    assert_eq!(today_row.estimated_completion_tokens, 7);

    let hourly = store.get_usage_hourly_summary(24).unwrap();
    let active_hour = hourly
        .iter()
        .find(|row| row.total_requests == 2)
        .expect("active hourly bucket");
    assert_eq!(active_hour.reported_prompt_tokens, 10);
    assert_eq!(active_hour.reported_completion_tokens, 5);
    assert_eq!(active_hour.estimated_prompt_tokens, 20);
    assert_eq!(active_hour.estimated_completion_tokens, 7);

    let lifetime = store.get_usage_lifetime().unwrap();
    assert_eq!(lifetime.reported_prompt_tokens, 10);
    assert_eq!(lifetime.reported_completion_tokens, 5);
    assert_eq!(lifetime.estimated_prompt_tokens, 20);
    assert_eq!(lifetime.estimated_completion_tokens, 7);

    drop(store);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn task_stats_accumulate_by_agent_and_task() {
    let path = std::env::temp_dir().join(format!(
        "free-agent-gateway-task-stats-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let store = ModelMetaStore::open(&path).unwrap();

    store
        .record_task_usage(
            "openrouter",
            "qwen/qwen3-coder:free",
            Some("coding_agent"),
            "coding",
            true,
            1200,
            Some(100),
            Some(20),
        )
        .unwrap();
    store
        .record_task_usage(
            "openrouter",
            "qwen/qwen3-coder:free",
            Some("coding_agent"),
            "coding",
            false,
            900,
            None,
            None,
        )
        .unwrap();

    let stats = store
        .get_task_stats(
            "openrouter",
            "qwen/qwen3-coder:free",
            Some("coding_agent"),
            "coding",
            7,
        )
        .unwrap()
        .unwrap();
    assert_eq!(stats.request_count, 2);
    assert_eq!(stats.success_count, 1);
    assert_eq!(stats.error_count, 1);
    assert_eq!(stats.total_latency_ms, 2100);
    assert_eq!(stats.prompt_tokens, 100);
    assert_eq!(stats.completion_tokens, 20);

    drop(store);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn capability_observations_accumulate() {
    let path = std::env::temp_dir().join(format!(
        "free-agent-gateway-capability-observations-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let store = ModelMetaStore::open(&path).unwrap();

    store
        .record_capability_observation("nvidia", "model-a", "tools", "failure")
        .unwrap();
    store
        .record_capability_observation("nvidia", "model-a", "tools", "failure")
        .unwrap();

    let count = store
        .get_capability_observation_count("nvidia", "model-a", "tools", "failure")
        .unwrap();
    assert_eq!(count, 2);

    drop(store);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn task_stats_summary_groups_recent_model_agent_task_rows() {
    let path = std::env::temp_dir().join(format!(
        "free-agent-gateway-task-summary-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let store = ModelMetaStore::open(&path).unwrap();

    store
        .record_task_usage(
            "openrouter",
            "model-a",
            Some("coding_agent"),
            "coding",
            true,
            100,
            Some(10),
            Some(5),
        )
        .unwrap();
    store
        .record_task_usage(
            "openrouter",
            "model-a",
            Some("coding_agent"),
            "coding",
            false,
            200,
            None,
            None,
        )
        .unwrap();

    let summary = store.get_task_stats_summary(7).unwrap();

    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].provider, "openrouter");
    assert_eq!(summary[0].agent.as_deref(), Some("coding_agent"));
    assert_eq!(summary[0].request_count, 2);
    assert_eq!(summary[0].success_count, 1);
    assert_eq!(summary[0].error_count, 1);
    assert_eq!(summary[0].total_latency_ms, 300);

    drop(store);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn capability_observation_summary_groups_outcomes() {
    let path = std::env::temp_dir().join(format!(
        "free-agent-gateway-capability-summary-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let store = ModelMetaStore::open(&path).unwrap();

    store
        .record_capability_observation("nvidia", "model-a", "tools", "failure")
        .unwrap();
    store
        .record_capability_observation("nvidia", "model-a", "tools", "success")
        .unwrap();

    let summary = store.get_capability_observation_summary().unwrap();

    assert_eq!(summary.len(), 2);
    assert!(summary.iter().any(|row| row.outcome == "failure"));
    assert!(summary.iter().any(|row| row.outcome == "success"));

    drop(store);
    let _ = std::fs::remove_file(&path);
}
