# Configuration Reference

## Key-level model capability and cost safety

`type` selects a protocol adapter. Use `openai_compatible` for services that
implement the OpenAI `/models` and `/chat/completions` APIs, regardless of
vendor. `github_models` and `nvidia` remain accepted as legacy aliases.

Keys should explicitly declare their cost tier:

```yaml
providers:
  example:
    type: openai_compatible
    base_url: "https://example.com/v1"
    keys:
      - value: "${EXAMPLE_FREE_KEY}"
        tier: free
      - value: "${EXAMPLE_PAID_KEY}"
        tier: paid
```

Valid tiers are `free`, `paid`, and `unknown`. Legacy string keys are parsed as
`unknown`. Only `free` keys are eligible for automatic chat routing; paid and
unknown keys are never selected automatically.

The gateway calls `/models` separately for every key because keys under the same
provider may expose different models. Normal fallback prefers the exact same
model ID on another available free key. If no usable exact-model candidate
exists, the router can choose a concrete automatic fallback model from the
discovered free inventory while respecting disabled models and filtering
non-chat/aggregate routes.

Both the top-level `models` alias map and provider `health_check_model` are
optional. `health_check_model` is accepted for compatibility but is not used for
routing or health decisions.

## File Location

Default: `config.yaml` in the same directory as the binary.

## Environment Variables

Configuration supports `${VAR_NAME}` expansion. The gateway reads environment variables at startup and substitutes them in the YAML.

Example:

```yaml
providers:
  github:
    keys:
      - "${GITHUB_TOKEN_1}"    # → reads GITHUB_TOKEN_1 from env
```

## Full Configuration

### server

```yaml
server:
  host: "127.0.0.1"          # Bind address
  port: 9000                  # Bind port
  log_level: "info"          # error | warn | info | debug
  request_timeout: 120        # Request timeout in seconds
  sse_keepalive: 15           # SSE keep-alive interval in seconds
```

### logging

Console logging is always enabled. File logging is enabled by default, rolls
daily, and is cleaned up automatically by age and total size.

```yaml
logging:
  file_enabled: true
  directory: "logs"
  file_prefix: "gateway.log"
  retention_days: 14
  max_total_mb: 256
```

Set `file_enabled: false` if a supervisor already captures stdout/stderr and
handles rotation externally.

### routing

```yaml
routing:
  strategy: "least_failed"   # round_robin | random | least_failed | priority
  fail_threshold: 3          # Consecutive failures before cooldown
  cooldown_seconds: 600      # Key cooldown duration in seconds (default: 10 min)
  auto_discover: true         # Auto-discover models from providers
```

| Strategy | Description |
|----------|-------------|
| `round_robin` | Cycle through keys/providers in order |
| `random` | Random selection |
| `least_failed` | Prefer the key/provider with fewest failures (recommended) |
| `priority` | Use the first available key/provider |

### fallback

Provider fallback chain order. When a provider fails, the gateway tries the next in the list.

```yaml
fallback:
  - "github"      # Try GitHub Models first
  - "nvidia"      # Then NVIDIA NIM
  - "opencode"    # Then OpenCode
  - "ollama"      # Finally Ollama (local, always available)
```

**Important**: Ollama should always be the last in the fallback chain since it runs locally and is the ultimate fallback.

When the requested model has no currently usable free-key candidate, the router
can also choose a concrete automatic fallback model from the discovered free
inventory. This respects disabled models and key availability, skips
non-chat-style models such as embeddings/guards/audio/image models, and avoids
OpenRouter aggregate routes such as `openrouter/free` and `openrouter/auto`.
Manual model blocks therefore remain the source of truth; automatic fallback
only chooses from the models that are still visible and available.

### agents

Agent-aware routing. Each agent can have a default model that's automatically selected when the agent sends requests.

```yaml
agents:
  hermes:
    default_model: "coding"         # → github / openai/gpt-4.1-mini
  openclaw:
    default_model: "chat"            # → nvidia / meta/llama-3.1-70b-instruct
  zeroclaw:
    default_model: "chat"
  document:
    default_model: "local"           # → ollama / qwen2.5:7b
  coding_agent:
    default_model: "coding"
  mcp_agent:
    default_model: "chat"
```

Usage: Set `X-Agent-Name: hermes` header in the request.

### models

Model alias definitions. These map friendly names to actual `provider/model` combinations.

```yaml
models:
  coding:
    provider: "github"
    model: "openai/gpt-4.1-mini"
  chat:
    provider: "nvidia"
    model: "meta/llama-3.1-70b-instruct"
  local:
    provider: "ollama"
    model: "qwen2.5:7b"
  reasoning:
    provider: "github"
    model: "openai/o3-mini"
  embedding:
    provider: "nvidia"
    model: "nvidia/nv-embedqa-e5-v5"
```

Clients can then use `"model": "coding"` instead of `"model": "openai/gpt-4.1-mini"`.

### providers

Individual provider configurations.

#### github

```yaml
providers:
  github:
    type: "github_models"                              # Provider type
    enabled: true                                        # Can disable without removing config
    base_url: "https://models.github.ai/inference"       # API endpoint
    keys:                                               # Multiple keys for rotation
      - value: "${GITHUB_TOKEN_1}"
        tier: free
        rpm_limit: 15
        rpd_limit: 150
      - value: "${GITHUB_TOKEN_2}"
        tier: free
        rpm_limit: 15
        rpd_limit: 150
      - value: "${GITHUB_TOKEN_3}"
        tier: free
        rpm_limit: 15
        rpd_limit: 150
    health_check_model: "openai/gpt-4.1-mini"            # Model used for health checks
    timeout_seconds: 30                                  # Request timeout
```

GitHub Models requires a GitHub PAT with `models:read` permission. Free API
usage is rate-limited by model category. As a conservative default, low-tier
models can start at `15 RPM / 150 RPD` per token; high-tier and special models
can be lower, so keep automatic 429 learning enabled.

#### nvidia

```yaml
  nvidia:
    type: "nvidia"
    enabled: true
    base_url: "https://integrate.api.nvidia.com/v1"
    keys:
      - "${NVIDIA_API_KEY_1}"
      - "${NVIDIA_API_KEY_2}"
    health_check_model: "meta/llama-3.1-70b-instruct"
    timeout_seconds: 30
```

#### openai_compatible

For any service implementing the OpenAI API:

```yaml
  opencode:
    type: "openai_compatible"
    enabled: true
    base_url: "https://opencode.ai/zen/v1"
    keys:
      - "${OPENCODE_API_KEY}"
    health_check_model: "gpt-4o-mini"
    timeout_seconds: 30
```

#### huggingface

Hugging Face Inference Providers can use the OpenAI-compatible router. Use a
token with Inference Providers permission enabled. The gateway supports multiple
HF tokens with the same rotation, cooldown, RPM/RPD, and probing logic as other
remote providers.

```yaml
  huggingface:
    type: "huggingface"
    enabled: true
    base_url: "https://router.huggingface.co/v1"
    keys:
      - value: "${HF_TOKEN_1}"
        tier: free
        rpm_limit: 10
      - value: "${HF_TOKEN_2}"
        tier: free
        rpm_limit: 10
    health_check_model: ""
    timeout_seconds: 30
```

Hugging Face free usage is credit-based rather than a guaranteed unlimited free
RPM tier, so keep `enabled: false` until tokens are configured and keep watcher
refresh low-frequency. HF router model IDs and routing suffixes such as
`:fastest`, `:cheapest`, and `:preferred` are passed through as normal model
names.

#### gemini

Google Gemini can use the OpenAI-compatible Gemini API. Multiple keys are
supported with the same per-key rotation, RPM/RPD limits, cooldown, and probing
logic as other remote providers.

```yaml
  gemini:
    type: "gemini"
    enabled: true
    base_url: "https://generativelanguage.googleapis.com/v1beta/openai"
    keys:
      - value: "${GEMINI_API_KEY_1}"
        tier: free
        rpm_limit: 12
      - value: "${GEMINI_API_KEY_2}"
        tier: free
        rpm_limit: 12
    health_check_model: "gemini-3.1-flash-lite"
    timeout_seconds: 60
```

#### ollama

Local Ollama instance (no real API keys needed):

```yaml
  ollama:
    type: "ollama"
    enabled: true
    base_url: "http://localhost:11434"    # Default Ollama port
    keys:
      - "ollama"                          # Placeholder, Ollama doesn't use auth
    health_check_model: "qwen2.5:7b"
    timeout_seconds: 120                  # Longer timeout for local inference
    priority: 100                         # Low priority (fallback)
```

### watcher

Background health check configuration:

```yaml
watcher:
  enabled: true                  # Enable/disable health checks
  startup_check: false           # Run model discovery immediately on startup
  interval_seconds: 7200         # Average background refresh interval
  min_interval_seconds: 3600     # Minimum interval after jitter
  jitter_percent: 0.5            # Randomize each interval by +/- this fraction
  check_timeout_seconds: 10      # Timeout per individual check
```

Background refresh uses provider model-list endpoints. It does not generate
chat tokens, but it still consumes upstream request quota and can contribute to
RPM limits, so free-key deployments should keep it low-frequency.

### context_compression

Optional local compression for large tool/log outputs before they are sent to an
upstream model.

```yaml
context_compression:
  enabled: false
  command: "G:\\ai\\AgentsTools\\rtk.exe"
  min_message_tokens: 2000
  timeout_seconds: 3
```

When enabled, the router calls `rtk pipe --ultra-compact` only for large
`role=tool` message content. Normal user, system, and assistant messages are not
compressed. If RTK fails, times out, or produces output that is not smaller than
the original content, the gateway keeps the original message.

### adaptive_routing

Adaptive routing is opt-in and does not change the existing `/v1` routes.

New route namespaces:

```text
POST /auto/v1/chat/completions
GET  /auto/v1/models

POST /agents/{agent}/v1/chat/completions
GET  /agents/{agent}/v1/models

POST /{provider_name}/v1/chat/completions
GET  /{provider_name}/v1/models

POST /provider-groups/{group}/v1/chat/completions
GET  /provider-groups/{group}/v1/models
```

Diagnostic and learning endpoints:

```text
GET /admin/routing/adaptive?model=auto&q=fix%20rust%20panic
GET /admin/routing/adaptive?agent=coding_agent&q=stack%20trace
GET /admin/routing/adaptive?provider=openrouter&q=hello
GET /admin/routing/adaptive?group=free_cloud&q=tool%20call
GET /admin/routing/routes
GET /admin/routing/groups

GET /admin/metadata/tasks?days=7
GET /admin/metadata/capabilities
```

The routing diagnostic endpoint returns the inferred task kinds, candidate
models, score breakdown, and selected candidate without exposing API keys.
The routes endpoint returns every visible adaptive route prefix, including
`/auto/v1`, `/agents/{agent}/v1`, `/{provider}/v1`, and
`/provider-groups/{group}/v1`. The groups endpoint returns visible provider
groups, their route prefixes, attached agents, provider health, available key
counts, and model counts. Task and capability metadata are learned from adaptive
non-streaming requests.

Provider names used as route prefixes must not collide with reserved gateway
namespaces: `v1`, `auto`, `agents`, `provider-groups`, `admin`, `health`,
`status`, or `metrics`.

```yaml
adaptive_routing:
  enabled: false
  mode: "observe"       # observe | assist | auto
  allow_paid: false
  candidate_limit: 20
  learning_window_days: 7
  hard_override_on_capability_mismatch: true
  auto_models:
    coding-auto:
      task: "coding"
    document-auto:
      task: "document"
  agent_profiles:
    coding_agent:
      default_auto_model: "coding-auto"
      preferred_tasks: ["coding", "tools", "reasoning"]
      provider_groups: ["coding"]
  routing_groups:
    coding:
      providers: ["openrouter", "nvidia"]
    free-cloud:
      providers: ["openrouter", "github", "nvidia", "gemini", "groq", "huggingface", "cloudflare"]
```

The first implementation supports non-streaming adaptive chat selection. Streaming
requests should continue using `/v1` until adaptive stream replay semantics are
explicitly implemented.

### state

Persistence configuration:

```yaml
state:
  save_interval_seconds: 30      # Auto-save interval
  state_file: "state.json"       # Key state persistence file
  models_cache_file: "models.cache"  # Model discovery cache
```

### cors

CORS configuration for HTTP responses:

```yaml
cors:
  allowed_origins:
    - "*"                         # Wildcard allows all origins
  allowed_methods:
    - "GET"
    - "POST"
    - "OPTIONS"
  allowed_headers:
    - "Authorization"
    - "Content-Type"
    - "X-Request-Id"
    - "X-Agent-Name"
```

## Key Status Transitions

```
                  ┌──────────────────────────┐
                  │                          │
    ┌───────┐     │     ┌─────────────┐      │
    │       │─────│────▶│  Available  │◀─────│──── Cooldown expires
    │       │ 429 │     └──────┬──────┘      │     (fail_count reset)
    │       │     │            │              │
    │       │     │     ┌──────▼──────┐      │
    │       │     │     │ RateLimited │      │
    │       │     │     │ (cooldown)  │──────┘
    │       │     │     └─────────────┘
    │       │     │
    │       │401 │     ┌─────────────┐
    │       │403 │     │  Disabled   │
    │       │─────│────▶│ (permanent)│
    │       │     │     └─────────────┘
    │       │     │
    │       │ 5xx │     ┌─────────────┐
    │       │timeout    │  Cooldown   │──────▶ Available
    │       │─────│────▶│ (temporary) │
    └───────┘     │     └─────────────┘
                  │
            (fail_count++)
            (threshold reached)
```

## Minimal Configuration

The simplest working configuration:

```yaml
server:
  host: "127.0.0.1"
  port: 9000
  log_level: "info"

routing:
  strategy: "least_failed"
  fail_threshold: 3
  cooldown_seconds: 600
  auto_discover: true

fallback:
  - "ollama"

providers:
  ollama:
    type: "ollama"
    enabled: true
    base_url: "http://localhost:11434"
    keys:
      - "ollama"
```

This configuration uses only Ollama locally with no external providers.
