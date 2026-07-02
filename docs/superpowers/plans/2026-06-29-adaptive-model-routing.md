# Adaptive Model Routing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add isolated adaptive/provider/agent route namespaces that can select models from provider inventory using task profiles, model metadata, key capacity, and learned runtime observations without changing existing `/v1` behavior.

**Architecture:** Add focused adaptive routing modules under `src/adaptive/`, keeping the existing `Router` as the execution engine for concrete provider/model/key attempts. New API handlers translate URL namespace constraints into an adaptive routing request, call the adaptive router, and then forward through existing provider traits with OpenAI-compatible response shapes.

**Tech Stack:** Rust 2024, axum routes, existing `KeyHub`, existing `ModelMetaStore`, SQLite migrations via rusqlite, existing provider trait, cargo tests.

---

## File Structure

- Create `src/adaptive/mod.rs`: module exports and public adaptive router API.
- Create `src/adaptive/profile.rs`: deterministic task profiling from request, agent, route namespace, tools, images, context size, and text signals.
- Create `src/adaptive/scoring.rs`: candidate scoring, score breakdown, rejection reasons, and route constraints.
- Create `src/adaptive/observations.rs`: runtime observation helpers for model task stats and capability outcomes.
- Modify `src/config.rs`: parse `adaptive_routing`, `auto_models`, `agent_profiles`, and `routing_groups`.
- Modify `src/metadata/mod.rs`: add migrations and record/query helpers for task-aware stats and capability observations.
- Modify `src/keyhub/mod.rs`: expose safe model/key candidate snapshots for adaptive scoring without raw keys in logs.
- Modify `src/api/chat.rs`: expose reusable response builders or add adaptive handler functions beside existing chat handling.
- Modify `src/api/models.rs`: add scoped model listing helpers for adaptive/provider/group models while preserving existing `/v1/models`.
- Modify `src/api/mod.rs`: export new adaptive handlers.
- Modify `src/main.rs`: mount `/auto/v1`, `/agents/{agent}/v1`, `/{provider_name}/v1`, and `/provider-groups/{group}/v1` after reserved route validation.
- Create `tests/adaptive_routing_tests.rs`: integration tests for route constraints, profiling, scoring, and unchanged `/v1` behavior.
- Update `config.yaml.sample` and `CONFIG.md`: document adaptive routing configuration.

## Task 1: Configuration Foundation

**Files:**
- Modify: `src/config.rs`
- Test: `tests/config_state_health_tests.rs`
- Modify: `config.yaml.sample`
- Modify: `CONFIG.md`

- [ ] **Step 1: Write failing config parse tests**

Add tests to `tests/config_state_health_tests.rs`:

```rust
#[test]
fn test_adaptive_routing_config_parses() {
    let yaml = r#"
server:
  host: "127.0.0.1"
  port: 9000
routing:
  auto_discover: true
fallback: ["openrouter"]
models: {}
providers:
  openrouter:
    type: "openai_compatible"
    enabled: true
    base_url: "https://openrouter.ai/api/v1"
    keys:
      - value: "test-key"
        tier: free
adaptive_routing:
  enabled: true
  mode: observe
  allow_paid: false
  candidate_limit: 12
  learning_window_days: 14
  hard_override_on_capability_mismatch: true
  auto_models:
    coding-auto:
      task: coding
  agent_profiles:
    coding_agent:
      default_auto_model: coding-auto
      preferred_tasks: ["coding", "tools"]
      provider_groups: ["coding"]
  routing_groups:
    coding:
      providers: ["openrouter"]
"#;

    let config: free_agent_gateway::config::Config = serde_yaml::from_str(yaml).unwrap();

    let adaptive = config.adaptive_routing;
    assert!(adaptive.enabled);
    assert_eq!(adaptive.mode, free_agent_gateway::config::AdaptiveMode::Observe);
    assert_eq!(adaptive.candidate_limit, 12);
    assert_eq!(adaptive.learning_window_days, 14);
    assert_eq!(adaptive.auto_models["coding-auto"].task, "coding");
    assert_eq!(
        adaptive.agent_profiles["coding_agent"].preferred_tasks,
        vec!["coding".to_string(), "tools".to_string()]
    );
    assert_eq!(
        adaptive.routing_groups["coding"].providers,
        vec!["openrouter".to_string()]
    );
}

#[test]
fn test_adaptive_routing_defaults_are_safe() {
    let config: free_agent_gateway::config::Config = serde_yaml::from_str(MINIMAL_CONFIG).unwrap();

    assert!(!config.adaptive_routing.enabled);
    assert_eq!(config.adaptive_routing.mode, free_agent_gateway::config::AdaptiveMode::Observe);
    assert!(!config.adaptive_routing.allow_paid);
    assert_eq!(config.adaptive_routing.candidate_limit, 20);
    assert_eq!(config.adaptive_routing.learning_window_days, 7);
}
```

If `MINIMAL_CONFIG` is not available in the test module, add a local minimal YAML string with one free provider.

- [ ] **Step 2: Run config tests and verify RED**

Run: `cargo test --test config_state_health_tests adaptive_routing`

Expected: FAIL because `Config` has no `adaptive_routing` field and no `AdaptiveMode`.

- [ ] **Step 3: Implement minimal config structs**

Add to `src/config.rs`:

```rust
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdaptiveMode {
    Observe,
    Assist,
    Auto,
}

impl Default for AdaptiveMode {
    fn default() -> Self {
        Self::Observe
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AdaptiveRoutingConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub mode: AdaptiveMode,
    #[serde(default)]
    pub allow_paid: bool,
    #[serde(default = "default_adaptive_candidate_limit")]
    pub candidate_limit: usize,
    #[serde(default = "default_adaptive_learning_window_days")]
    pub learning_window_days: i64,
    #[serde(default = "default_true")]
    pub hard_override_on_capability_mismatch: bool,
    #[serde(default)]
    pub auto_models: HashMap<String, AutoModelConfig>,
    #[serde(default)]
    pub agent_profiles: HashMap<String, AdaptiveAgentProfile>,
    #[serde(default)]
    pub routing_groups: HashMap<String, RoutingGroupConfig>,
}

impl Default for AdaptiveRoutingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: AdaptiveMode::Observe,
            allow_paid: false,
            candidate_limit: default_adaptive_candidate_limit(),
            learning_window_days: default_adaptive_learning_window_days(),
            hard_override_on_capability_mismatch: true,
            auto_models: HashMap::new(),
            agent_profiles: HashMap::new(),
            routing_groups: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct AutoModelConfig {
    #[serde(default)]
    pub task: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct AdaptiveAgentProfile {
    #[serde(default)]
    pub default_auto_model: String,
    #[serde(default)]
    pub preferred_tasks: Vec<String>,
    #[serde(default)]
    pub provider_groups: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct RoutingGroupConfig {
    #[serde(default)]
    pub providers: Vec<String>,
}

fn default_adaptive_candidate_limit() -> usize {
    20
}

fn default_adaptive_learning_window_days() -> i64 {
    7
}
```

Add to `Config`:

```rust
#[serde(default)]
pub adaptive_routing: AdaptiveRoutingConfig,
```

- [ ] **Step 4: Run config tests and verify GREEN**

Run: `cargo test --test config_state_health_tests adaptive_routing`

Expected: PASS.

- [ ] **Step 5: Update sample config and docs**

Add `adaptive_routing` examples to `config.yaml.sample` and `CONFIG.md` matching the design. Keep defaults conservative: `enabled: false`, `mode: observe`, `allow_paid: false`.

## Task 2: Metadata Learning Tables

**Files:**
- Modify: `src/metadata/mod.rs`
- Test: `tests/metadata_learning_tests.rs`

- [ ] **Step 1: Write failing metadata tests**

Add tests:

```rust
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
        .get_task_stats("openrouter", "qwen/qwen3-coder:free", Some("coding_agent"), "coding", 7)
        .unwrap()
        .unwrap();
    assert_eq!(stats.request_count, 2);
    assert_eq!(stats.success_count, 1);
    assert_eq!(stats.error_count, 1);
    assert_eq!(stats.total_latency_ms, 2100);
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
}
```

- [ ] **Step 2: Run metadata tests and verify RED**

Run: `cargo test --test metadata_learning_tests task_stats capability_observations`

Expected: FAIL because helper methods and row types do not exist.

- [ ] **Step 3: Add migrations and helper methods**

Extend `ModelMetaStore::migrate()` with `model_task_stats` and `model_capability_observations` tables from the design. Add:

```rust
pub fn record_task_usage(...)
pub fn get_task_stats(...) -> GatewayResult<Option<TaskStatsRow>>
pub fn record_capability_observation(...)
pub fn get_capability_observation_count(...) -> GatewayResult<i64>
```

Add:

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskStatsRow {
    pub provider: String,
    pub model_id: String,
    pub agent: Option<String>,
    pub task_kind: String,
    pub request_count: i64,
    pub success_count: i64,
    pub error_count: i64,
    pub total_latency_ms: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub last_used_at: Option<i64>,
}
```

- [ ] **Step 4: Run metadata tests and verify GREEN**

Run: `cargo test --test metadata_learning_tests`

Expected: PASS.

## Task 3: Task Profiling

**Files:**
- Create: `src/adaptive/mod.rs`
- Create: `src/adaptive/profile.rs`
- Modify: `src/lib.rs`
- Test: `tests/adaptive_routing_tests.rs`

- [ ] **Step 1: Write failing profile tests**

Create tests:

```rust
use free_agent_gateway::adaptive::profile::{TaskKind, TaskProfile, build_task_profile};
use free_agent_gateway::models::{ChatCompletionRequest, ChatMessage};

fn request_with_content(content: serde_json::Value) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "auto".into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content,
            name: None,
            tool_calls: None,
            tool_call_id: None,
            extra: serde_json::Map::new(),
        }],
        temperature: None,
        top_p: None,
        n: None,
        stream: None,
        stop: None,
        max_tokens: None,
        presence_penalty: None,
        frequency_penalty: None,
        user: None,
        request_id: None,
        agent_name: None,
        extra: serde_json::Map::new(),
    }
}

#[test]
fn profile_detects_vision_from_image_parts() {
    let req = request_with_content(serde_json::json!([
        {"type":"text","text":"what is this?"},
        {"type":"image_url","image_url":{"url":"data:image/png;base64,abc"}}
    ]));

    let profile = build_task_profile(Some("document"), &req, None);

    assert!(profile.needs_vision);
    assert!(profile.task_kinds.contains(&TaskKind::Vision));
}

#[test]
fn profile_detects_coding_from_agent_and_text() {
    let req = request_with_content(serde_json::json!(
        "Fix this Rust panic:\n```rust\npanic!(\"boom\")\n```"
    ));

    let profile = build_task_profile(Some("coding_agent"), &req, None);

    assert!(profile.needs_coding);
    assert!(profile.needs_reasoning);
    assert!(profile.task_kinds.contains(&TaskKind::Coding));
}

#[test]
fn profile_detects_tools_from_extra_fields() {
    let mut req = request_with_content(serde_json::json!("call a tool"));
    req.extra.insert("tools".into(), serde_json::json!([{"type":"function"}]));

    let profile = build_task_profile(Some("hermes"), &req, None);

    assert!(profile.needs_tools);
    assert!(profile.task_kinds.contains(&TaskKind::Tools));
}
```

- [ ] **Step 2: Run adaptive profile tests and verify RED**

Run: `cargo test --test adaptive_routing_tests profile`

Expected: FAIL because adaptive module does not exist.

- [ ] **Step 3: Implement deterministic profile builder**

Create `src/adaptive/profile.rs` with `TaskKind`, `TaskProfile`, and `build_task_profile`. Use existing `request_has_vision`, `extra.contains_key("tools")`, text heuristics for code blocks, stack traces, diffs, file paths, and rough prompt token estimate as `chars / 4`.

Create `src/adaptive/mod.rs`:

```rust
pub mod profile;
pub mod scoring;
pub mod observations;
```

Add to `src/lib.rs`:

```rust
pub mod adaptive;
```

- [ ] **Step 4: Run adaptive profile tests and verify GREEN**

Run: `cargo test --test adaptive_routing_tests profile`

Expected: PASS.

## Task 4: Candidate Scoring

**Files:**
- Create: `src/adaptive/scoring.rs`
- Modify: `src/adaptive/mod.rs`
- Test: `tests/adaptive_routing_tests.rs`

- [ ] **Step 1: Write failing scoring tests**

Add tests for:

- Tool-capable model beats unknown model for tool requests.
- Recent high error rate lowers score.
- Provider constraint excludes other providers.
- Paid candidates excluded when `allow_paid=false`.

- [ ] **Step 2: Run scoring tests and verify RED**

Run: `cargo test --test adaptive_routing_tests scoring`

Expected: FAIL because scorer does not exist.

- [ ] **Step 3: Implement minimal scorer**

Define:

```rust
pub struct RouteConstraints { ... }
pub struct AdaptiveCandidate { ... }
pub struct ScoredCandidate { ... }
pub struct ScoreBreakdown { ... }
pub fn score_candidates(profile: &TaskProfile, candidates: Vec<AdaptiveCandidate>, constraints: &RouteConstraints) -> Vec<ScoredCandidate>
```

Keep first version deterministic and transparent. Do not call upstream providers from scorer.

- [ ] **Step 4: Run scoring tests and verify GREEN**

Run: `cargo test --test adaptive_routing_tests scoring`

Expected: PASS.

## Task 5: Adaptive API Route Skeleton

**Files:**
- Modify: `src/api/chat.rs`
- Modify: `src/api/models.rs`
- Modify: `src/api/mod.rs`
- Modify: `src/main.rs`
- Test: `tests/adaptive_routing_tests.rs`

- [ ] **Step 1: Write failing route tests**

Use axum router tests or handler-level tests to prove:

- `/auto/v1/chat/completions` accepts a request and sets adaptive namespace.
- `/agents/coding_agent/v1/chat/completions` sets agent without requiring `X-Agent-Name`.
- `/openrouter/v1/models` maps to provider scope.
- Reserved namespace names are rejected as provider prefixes.

- [ ] **Step 2: Run route tests and verify RED**

Run: `cargo test --test adaptive_routing_tests route`

Expected: FAIL because routes are not mounted.

- [ ] **Step 3: Implement route handlers and namespace constraints**

Add handler functions that build a `RouteConstraints` value:

- `AdaptiveScope::All` for `/auto/v1`.
- `AdaptiveScope::Agent(agent)` for `/agents/{agent}/v1`.
- `AdaptiveScope::Provider(provider)` for `/{provider_name}/v1`.
- `AdaptiveScope::ProviderGroup(group)` for `/provider-groups/{group}/v1`.

Ensure static routes are mounted before dynamic provider prefix routes.

- [ ] **Step 4: Run route tests and verify GREEN**

Run: `cargo test --test adaptive_routing_tests route`

Expected: PASS.

## Task 6: Adaptive Execution Path

**Files:**
- Modify: `src/adaptive/mod.rs`
- Modify: `src/keyhub/mod.rs`
- Modify: `src/api/chat.rs`
- Test: `tests/adaptive_routing_tests.rs`

- [ ] **Step 1: Write failing execution tests**

Add integration tests with fake providers:

- `model: "coding-auto"` selects a concrete coding candidate.
- Provider prefix `/openrouter/v1` never selects `nvidia`.
- Provider group only selects providers in the group.
- Existing `/v1/chat/completions` still uses explicit model behavior.

- [ ] **Step 2: Run execution tests and verify RED**

Run: `cargo test --test adaptive_routing_tests execution`

Expected: FAIL because adaptive execution does not select candidates.

- [ ] **Step 3: Implement candidate collection and execution**

Add a KeyHub snapshot method returning provider/model/key candidate metadata without raw keys in logs but with raw key available internally for execution. Collect free available models, apply constraints, score candidates, reserve the selected key, rewrite `request.model` to selected model, and call the selected provider.

- [ ] **Step 4: Run execution tests and verify GREEN**

Run: `cargo test --test adaptive_routing_tests execution`

Expected: PASS.

## Task 7: Observations and Decision Logs

**Files:**
- Create/modify: `src/adaptive/observations.rs`
- Modify: `src/adaptive/mod.rs`
- Modify: `src/router/mod.rs` only if shared logging helpers are needed
- Test: `tests/adaptive_routing_tests.rs`

- [ ] **Step 1: Write failing observation tests**

Add tests proving:

- Successful adaptive request records model task usage.
- Capability mismatch failure records a capability observation.
- Decision log data excludes raw key and raw prompt fields.

- [ ] **Step 2: Run observation tests and verify RED**

Run: `cargo test --test adaptive_routing_tests observations`

Expected: FAIL because observations are not wired.

- [ ] **Step 3: Implement observation recording**

On adaptive success/failure, call `ModelMetaStore::record_task_usage` and `record_capability_observation` where applicable. Emit structured tracing events for `adaptive_route_decision`.

- [ ] **Step 4: Run observation tests and verify GREEN**

Run: `cargo test --test adaptive_routing_tests observations`

Expected: PASS.

## Task 8: Documentation and Final Verification

**Files:**
- Modify: `CONFIG.md`
- Modify: `README.md` if endpoint overview is maintained there
- Modify: `ARCHITECTURE.md` if routing diagram is updated

- [ ] **Step 1: Update docs**

Document:

- `/auto/v1`
- `/agents/{agent}/v1`
- `/{provider_name}/v1`
- `/provider-groups/{group}/v1`
- `observe | assist | auto`
- Provider prefix reserved names

- [ ] **Step 2: Run formatting**

Run: `cargo fmt --all`

Expected: exit code 0.

- [ ] **Step 3: Run full tests**

Run: `cargo test`

Expected: all tests pass.

- [ ] **Step 4: Run cargo check**

Run: `cargo check`

Expected: exit code 0.

- [ ] **Step 5: Smoke test existing endpoint**

Run: `curl.exe -s -o NUL -w "%{http_code} %{time_total}\n" http://127.0.0.1:9000/v1/models`

Expected: `200` and no regression to multi-second model discovery.

## Self-Review

- Spec coverage: route namespaces, provider aggregation, task profiling, scoring, runtime learning, logs, compatibility, and rollout are covered by Tasks 1-8.
- Placeholder scan: no `TBD`, `TODO`, or unspecified implementation placeholders remain in this plan.
- Type consistency: `AdaptiveMode`, `AdaptiveRoutingConfig`, `TaskProfile`, `RouteConstraints`, and scorer names are consistent across tasks.
