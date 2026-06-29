# Adaptive Model Routing Design

## Goal

Add a new routing surface that can automatically choose models per request based on agent identity, request shape, provider inventory, learned model capability, historical reliability, and current key capacity.

The existing OpenAI-compatible routes remain unchanged:

- `POST /v1/chat/completions`
- `GET /v1/models`

New adaptive routes are added under isolated namespaces so current clients keep working while new clients opt in.

## Non-Goals

This design does not:

- Replace existing `/v1` behavior by default.
- Use paid or unknown-tier keys automatically unless explicitly configured.
- Require an LLM classifier for the first version.
- Trust provider marketing labels without runtime validation.
- Expose raw keys, request bodies, or sensitive headers in routing logs.

## Route Surfaces

The gateway supports multiple compatible entry points. They share the same request/response shape as OpenAI chat completions unless noted.

### Existing Stable API

```text
POST /v1/chat/completions
GET  /v1/models
```

Behavior stays model-explicit and backward compatible.

### Adaptive API

```text
POST /auto/v1/chat/completions
GET  /auto/v1/models
```

The request may use:

```json
{ "model": "auto", "messages": [...] }
```

or an agent-specific auto alias:

```json
{ "model": "coding-auto", "messages": [...] }
```

The router builds a task profile, scores candidate models, chooses a concrete provider/model/key, and forwards the request upstream with the selected concrete model.

### Agent-Scoped API

```text
POST /agents/{agent}/v1/chat/completions
GET  /agents/{agent}/v1/models
```

This is equivalent to sending `X-Agent-Name: {agent}` plus adaptive routing. It is useful for clients that cannot set custom headers.

Examples:

```text
POST /agents/hermes/v1/chat/completions
POST /agents/coding_agent/v1/chat/completions
POST /agents/document/v1/chat/completions
```

### Provider-Scoped API

```text
POST /providers/{provider}/v1/chat/completions
GET  /providers/{provider}/v1/models
```

This limits candidate selection to one provider. It still selects among that provider's eligible keys and models.

Examples:

```text
POST /providers/openrouter/v1/chat/completions
GET  /providers/nvidia/v1/models
```

### Provider-Group API

```text
POST /provider-groups/{group}/v1/chat/completions
GET  /provider-groups/{group}/v1/models
```

Groups are configured sets of providers, such as:

```yaml
routing_groups:
  free-cloud:
    providers: ["openrouter", "nvidia", "groq", "cloudflare"]
  local-first:
    providers: ["ollama", "opencode", "openrouter"]
  coding:
    providers: ["github", "openrouter", "nvidia"]
```

Provider groups let operators expose stable routing lanes without hardcoding provider names into agents.

## Configuration

Add an optional adaptive routing section:

```yaml
adaptive_routing:
  enabled: true
  default_mode: explicit
  allow_paid: false
  candidate_limit: 20
  learning_window_days: 7
  hard_override_on_capability_mismatch: true

  auto_models:
    auto:
      task: balanced
    coding-auto:
      task: coding
    document-auto:
      task: document
    vision-auto:
      task: vision

  agent_profiles:
    hermes:
      default_auto_model: coding-auto
      preferred_tasks: ["coding", "reasoning", "tools"]
      provider_groups: ["coding", "free-cloud"]
    coding_agent:
      default_auto_model: coding-auto
      preferred_tasks: ["coding", "reasoning", "tools"]
    document:
      default_auto_model: document-auto
      preferred_tasks: ["long_context", "summarization"]

  routing_groups:
    free-cloud:
      providers: ["openrouter", "nvidia", "groq", "cloudflare"]
    coding:
      providers: ["github", "openrouter", "nvidia"]
```

`default_mode` controls how aggressive adaptive routing is:

- `explicit`: only adaptive routes or `*-auto` model names use adaptive selection.
- `assist`: explicit model requests are respected unless hard capability mismatch is detected.
- `auto`: adaptive routes can replace explicit models when scoring says another candidate is better.

The recommended first release uses `explicit`.

## Task Profile

Every adaptive request is classified into a deterministic `TaskProfile`.

Inputs:

- Route namespace: `/auto`, `/agents/{agent}`, `/providers/{provider}`, `/provider-groups/{group}`
- `X-Agent-Name` or URL agent
- Requested model name
- Presence of `tools`
- Presence of `image_url` content parts
- Approximate prompt size
- Stream vs non-stream
- Message text signals: code blocks, stack traces, diffs, file paths, JSON/schema, planning language

Profile fields:

```rust
struct TaskProfile {
    agent: Option<String>,
    task_kinds: Vec<TaskKind>,
    needs_vision: bool,
    needs_tools: bool,
    needs_reasoning: bool,
    needs_coding: bool,
    needs_long_context: bool,
    estimated_prompt_tokens: u32,
    latency_preference: LatencyPreference,
    cost_preference: CostPreference,
}
```

The first implementation uses rules, not an LLM classifier. Rules are predictable, testable, and cheap.

## Candidate Pool

The candidate pool is the intersection of:

- Models currently discovered in KeyHub for available free keys.
- Optional provider/provider-group route constraints.
- Optional agent profile provider groups.
- Optional requested model or auto-model task.
- Disabled model filters.
- Required capabilities.

Hard filters:

- Vision request requires `supports_vision = true` or a strong model-name heuristic.
- Tool request requires `supports_tools = true` or a configured allowlist.
- Estimated prompt length must fit the model context window when known.
- Paid/unknown keys are excluded unless the route or config explicitly allows them.
- Unavailable keys, cooldown keys, and locally rate-limited keys are excluded.

Unknown capabilities are handled conservatively:

- Unknown optional capability lowers score.
- Unknown required capability is allowed only when no known-capable candidate exists and config permits probing.

## Scoring

Each candidate receives a score. The highest score is attempted first.

Primary dimensions:

- Capability match: vision, tools, reasoning, coding, long context.
- Agent fit: configured preferences for the agent.
- Historical success rate for provider/model.
- Recent error rate and error category.
- Current key quota headroom from KeyHub.
- Provider order or provider-group priority.
- Latency when measured.
- Cost/pricing when known.
- Freshness and confidence of model metadata.

Example scoring model:

```text
score =
  capability_match * 40
+ agent_fit * 20
+ success_rate * 15
+ quota_headroom * 10
+ provider_priority * 5
+ latency_score * 5
+ cost_score * 5
- recent_429_penalty
- timeout_penalty
- unknown_required_capability_penalty
```

The scoring formula should be simple and observable in the first version. Every adaptive decision logs a compact explanation with the top candidates and rejection reasons.

## Learning Model Capabilities

The current `ModelMetaStore` already stores core metadata, usage, and error categories. Adaptive routing extends this into a capability and performance loop.

### Static Capability Sources

- Provider `/models` responses.
- OpenRouter public metadata sync.
- Configured model annotations.
- Model-name heuristics.

Static fields:

- `context_window`
- `max_completion_tokens`
- `supports_vision`
- `supports_tools`
- `supports_reasoning`
- `pricing_prompt`
- `pricing_completion`
- `architecture_modality`

### Runtime Learning

Runtime observations update model quality by task and agent:

- Success count
- Error count
- 429 count
- Timeout count
- Auth/not-found/upstream error categories
- Average latency
- Prompt/completion token totals
- Long-context failures
- Tool-call failures
- Vision failures
- Agent-specific success/failure

Add a table or equivalent view for task-aware stats:

```sql
CREATE TABLE model_task_stats (
    provider        TEXT NOT NULL,
    model_id        TEXT NOT NULL,
    agent           TEXT,
    task_kind       TEXT NOT NULL,
    date            TEXT NOT NULL,
    request_count   INTEGER DEFAULT 0,
    success_count   INTEGER DEFAULT 0,
    error_count     INTEGER DEFAULT 0,
    total_latency_ms INTEGER DEFAULT 0,
    prompt_tokens   INTEGER DEFAULT 0,
    completion_tokens INTEGER DEFAULT 0,
    last_used_at    INTEGER,
    UNIQUE(provider, model_id, agent, task_kind, date)
);
```

Add a table for learned capability failures:

```sql
CREATE TABLE model_capability_observations (
    provider        TEXT NOT NULL,
    model_id        TEXT NOT NULL,
    capability      TEXT NOT NULL,
    outcome         TEXT NOT NULL,
    count           INTEGER DEFAULT 0,
    last_observed_at INTEGER,
    UNIQUE(provider, model_id, capability, outcome)
);
```

If a model repeatedly fails tool requests, vision requests, long-context requests, or coding-agent requests, the router lowers its score for that specific capability or agent instead of globally banning it.

## Decision Logging

Every adaptive request receives structured logs:

- `request_id`
- `route_namespace`
- `agent`
- `requested_model`
- `selected_provider`
- `selected_model`
- `selected_key`
- `task_kinds`
- `candidate_count`
- `top_candidates`
- `rejection_reasons`
- `score_breakdown`
- `fallback_attempt`
- `elapsed_ms`

Example log fields:

```text
stage=adaptive_route_decision
agent=coding_agent
requested_model=coding-auto
selected_provider=openrouter
selected_model=qwen/qwen3-coder:free
task_kinds=coding,tools
candidate_count=12
reason="best coding/tool fit with available free quota"
```

Logs must not include raw prompts, raw keys, or credentials.

## Request Flow

Adaptive route flow:

```text
Client
  -> /auto/v1 or /agents/{agent}/v1 or /providers/{provider}/v1
  -> parse OpenAI-compatible request
  -> build TaskProfile
  -> resolve route constraints
  -> collect discovered model/key candidates
  -> enrich candidates with ModelMetaStore and KeyHub state
  -> score candidates
  -> select provider/model/key
  -> rewrite request.model to concrete upstream model
  -> Provider.chat/chat_stream
  -> update KeyHub request accounting
  -> update ModelMetaStore usage/error/capability observations
  -> return OpenAI-compatible response
```

Fallback behavior:

- If a selected candidate fails before response body streaming begins, try the next scored candidate.
- If a candidate fails due to capability mismatch, record that observation and try another candidate.
- Streaming body failures are recorded but not replayed after bytes may have reached the client.
- If all candidates fail, return an OpenAI-compatible error with a safe summary.

## Provider Aggregation

Provider aggregation is exposed in three ways:

1. `/auto/v1/models`
   Lists adaptive-visible models across all eligible providers.

2. `/providers/{provider}/v1/models`
   Lists models available through that provider's eligible keys.

3. `/provider-groups/{group}/v1/models`
   Lists the union of eligible models in a configured provider group.

Model entries include existing metadata fields and may add non-sensitive routing hints:

```json
{
  "id": "qwen/qwen3-coder:free",
  "object": "model",
  "owned_by": "openrouter",
  "provider": "openrouter",
  "supports_tools": true,
  "supports_reasoning": true,
  "context_window": 32768,
  "routing": {
    "adaptive": true,
    "eligible_agents": ["coding_agent", "hermes"],
    "recent_success_rate": 0.98
  }
}
```

The existing `/v1/models` can remain simpler and backward compatible.

## Admin and Diagnostics

Add read-only diagnostics before adding write controls:

```text
GET /admin/routing/decisions
GET /admin/routing/models
GET /admin/routing/agents
GET /admin/routing/providers
```

Useful views:

- Model quality by task.
- Model quality by agent.
- Top rejected models and reasons.
- Models with unknown capabilities.
- Models recently penalized for 429, timeout, vision failure, or tool failure.
- Chosen model distribution per agent.

Optional later controls:

- Manually pin agent to model/group.
- Manually disable model for a task kind.
- Reset learned penalties.
- Promote a learned capability to explicit metadata.

## Compatibility and Rollout

Recommended rollout:

1. Add adaptive routes without changing `/v1`.
2. Add deterministic task profiling and scoring in observe-only mode.
3. Log selected candidate while still routing through existing explicit model path.
4. Enable adaptive routing for `/auto/v1`.
5. Move selected agents to `/agents/{agent}/v1` or `*-auto` defaults.
6. Enable capability-mismatch override for explicit requests.
7. Optionally support more aggressive `default_mode: auto`.

This keeps existing clients stable and gives logs enough time to build confidence.

## Testing

Regression coverage should include:

- Existing `/v1` behavior remains unchanged.
- `/auto/v1` accepts OpenAI-compatible chat requests.
- `/agents/{agent}/v1` sets agent context without requiring headers.
- `/providers/{provider}/v1` restricts candidates to one provider.
- `/provider-groups/{group}/v1` restricts candidates to configured providers.
- Vision request rejects or deprioritizes non-vision models.
- Tool request prefers tool-capable models.
- Long-context request prefers models with adequate context window.
- Coding-agent request prefers coding/reasoning-capable candidates.
- Recent 429/timeout errors lower score.
- Runtime capability failures are recorded and affect future scoring.
- Paid/unknown keys remain excluded by default.
- Streaming success and streaming body failure keep existing accounting semantics.
- Decision logs contain selected model and reason but no raw keys or prompts.

## Stop Condition

The design is complete when adaptive routing can be added under new route namespaces, existing `/v1` clients remain unaffected, provider and provider-group aggregation are defined, model scoring is explainable, and runtime logs can automatically improve per-model/per-agent capability estimates over time.
