# Architecture

## System Overview

```
┌─────────────────────────────────────────────────────────────┐
│                OpenClaw / Hermes and Agent Ecosystem          │
│                                                             │
│  OpenClaw  │  Hermes-Agent  │  future Agent integrations    │
│  OpenHuman │  ZeroClaw      │  Coding Agent  │ MCP Agent    │
└─────────────┬───────────────────────────────────────────────┘
              │
              │  OpenAI Compatible API
              ▼
┌─────────────────────────────────────────────────────────────┐
│                    free-agent-gateway                         │
│                                                             │
│  ┌──────────┐  ┌───────────┐  ┌──────────┐  ┌──────────┐  │
│  │   API     │  │   KeyHub   │  │  Router   │  │ Watcher  │  │
│  │  Handler  │  │   (Key     │  │ (Model    │  │(Health   │  │
│  │           │  │  Rotation) │  │  Routing) │  │ Check)   │  │
│  └────┬─────┘  └─────┬─────┘  └────┬──────┘  └────┬─────┘  │
│       │               │             │               │        │
│       └───────────────┴─────────────┴───────────────┘        │
│                           │                                  │
│  ┌────────────────────────┴───────────────────────────────┐  │
│  │              Provider Registry (DashMap)                │  │
│  └────────────────────────┬──────────────────────────────┘  │
└───────────────────────────┼─────────────────────────────────┘
                            │
          ┌─────────────────┼─────────────────┬─────────────────┐
          ▼                 ▼                 ▼                 ▼
   ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐
   │ GitHub Models│  │ OpenAI Compat│  │ NVIDIA/Open  │  │   Ollama     │
   │  (Azure)     │  │Router/Cerebras│ │ Code/etc.    │  │  (Local)     │
   └──────────────┘  └──────────────┘  └──────────────┘  └──────────────┘
```

## Core Components

### 1. API Handler (`src/api/`)

Implements OpenAI-compatible endpoints:

- **`POST /v1/chat/completions`** — Streaming and non-streaming chat completions
- **`GET /v1/models`** — List available models (aliases + auto-discovered)
- **`GET /health`** — Quick health check
- **`GET /status`** — Detailed gateway status
- **`GET /metrics`** — Full metrics dump
- **`GET /metrics/prometheus`** — Prometheus text-format metrics
- **`GET /providers`** — Provider-specific status
- **`GET /admin`** — **Admin Dashboard** (single-page HTML with embedded JS/CSS, served from `admin_html.rs`)
- **`GET /admin/status`** — Real-time provider + key status for dashboard rendering
- **`GET /admin/providers/:name/models`** — Per-provider model list with enable/disable state
- **`POST /admin/providers/:name/refresh`** — Trigger provider model re-discovery
- **`POST /admin/providers/:name/test`** — Test provider connectivity
- **`POST /admin/providers/:name/models/:id/toggle`** — Enable/disable a specific model
- **`POST /admin/save`** — Persist model configuration changes
- **`GET /admin/config`** — Read-only current config dump
- **`GET /admin/events`** — SSE endpoint for real-time event stream (logs, health changes, provider tests)
- **`GET /admin/metadata`**, `/admin/metadata/sync`, `/admin/metadata/models`, `/admin/metadata/errors` — Model metadata management

Security: All sensitive headers (authorization, api-key, token, cookie) are automatically redacted from logs.

### 2. Provider (`src/providers/`)

Abstract trait for upstream AI services:

```rust
#[async_trait]
pub trait Provider: Send + Sync + Debug {
    fn name(&self) -> &str;
    fn base_url(&self) -> &str;
    async fn list_models(&self, api_key: &str) -> Result<Vec<String>>;
    async fn chat(&self, api_key: &str, request: ChatCompletionRequest) -> Result<ChatResponse>;
    async fn chat_stream(&self, api_key: &str, request: ChatCompletionRequest) -> Result<StreamResponse>;
    async fn health_check(&self, api_key: &str) -> Result<u64>;
    fn health_check_model(&self) -> &str;
    fn timeout_seconds(&self) -> u64;
    fn priority(&self) -> u32;
}
```

Implementations:
- **`GithubModelsProvider`** — GitHub Models via Azure
- **`NvidiaProvider`** — NVIDIA NIM API
- **`OpenAiCompatibleProvider`** — Generic OpenAI-compatible endpoint (OpenRouter, Cerebras, OpenCode, etc.)
- **`OllamaProvider`** — Local Ollama with OpenAI format translation

### 3. KeyHub (`src/keyhub/`)

Manages multiple API keys per provider with automatic rotation:

```
Provider (github)
├── key1 (Available)  ←── acquire_key() selects this
├── key2 (RateLimited, cooldown until epoch+600)
└── key3 (Disabled)
```

**Key Status Machine:**

```
Available ──429──→ RateLimited ──cooldown expires──→ Available
Available ──401/403──→ Disabled ──manual restore──→ Available
Available ──5xx/timeout──→ fail_count++ ──threshold reached──→ Cooldown
Cooldown ──cooldown expires──→ Available (fail_count reset)
```

401/403 indicates an auth or permission failure, so the gateway never auto-recovers
that key. Use `POST /admin/providers/:name/keys/:key_id/restore` (or the Restore
button in Admin) after fixing the key/provider issue.

**Routing Strategies:**
- **RoundRobin** — Cycle through keys sequentially
- **Random** — Random selection
- **LeastFailed** — Prefer key with fewest failures
- **LeastRate** — Prefer the key/provider with the most remaining local quota headroom
- **Priority** — Use first available key

### 4. Router (`src/router/`)

Model resolution and Provider fallback:

```
Request: model="coding", agent="hermes"
    │
    ▼
1. Agent default → "coding" is hermes' default model
2. Alias lookup → "coding" → github / openai/gpt-4.1-mini
3. Cross-provider same-model candidate pool → [github, nvidia, cerebras, ollama]
    │
    ▼
Reserve key quota → try provider → 429 → KeyPool rate-limits key → try next candidate → ✅
```

**Streaming token usage extraction:**
- `account_stream()` buffers the last SSE chunk; on stream completion it calls `extract_stream_usage()` to parse `usage.prompt_tokens` / `usage.completion_tokens` from the final chunk
- Requests are pre-reserved before upstream calls for RPM/RPD protection; tokens are forwarded on success for TPM/TPD tracking

**Resolution order:**
1. Agent default model (if agent name provided)
2. Model alias table
3. Direct `provider/model` format
4. Fallback chain (first provider with that model)

### 5. Watcher (`src/watcher/`)

Background task running every 60 seconds:

- Check each provider's health (latency measurement)
- List models from each provider
- Update HealthRegistry
- Report to tracing logs

### 6. Health Registry (`src/health/`)

Global provider health state from periodic health checks (every 60s):

```rust
pub struct HealthState {
    pub provider: String,
    pub status: String,       // "healthy", "unhealthy", "disabled", "unknown"
    pub latency_ms: u64,
    pub success_count: u64,
    pub fail_count: u64,
    pub last_error: String,
    pub cooldown_until: Option<u64>,
    pub models_count: usize,
    pub available_keys: usize,
    pub total_keys: usize,
}
```

**Dashboard status enhancement (`src/api/admin.rs`):**
The `/admin/status` endpoint overlays real-time keyhub snapshot data on top of the watcher data:
- `available_keys` — computed live from `KeyHub.snapshot()` instead of stale health_registry data
- `computed_status` — derived from both health_registry and keyhub:
  - `"unhealthy"` — health check failed (provider unreachable)
  - `"exhausted"` — provider reachable, all keys unavailable (cooldown/rate-limited/disabled)
  - `"degraded"` — some keys available, some exhausted
  - `"healthy"` — at least some keys working

### 7. State Persistence (`src/state/`)

No database — uses `state.json` file:

```json
{
  "version": 1,
  "updated_at": 1719000000,
  "providers": {
    "github": {
      "keys": [
        { "key": "", "status": "available", "fail_count": 0 }
      ]
    }
  }
}
```

Saved atomically (write to `.tmp` then rename). Loaded on startup.

## Request Flow

```
Client Request
    │
    ▼
API Handler (parse headers, extract agent name)
    │
    ▼
Router.resolve(model, agent) → ResolvedRoute { provider, model }
    │
    ▼
For each provider in fallback chain:
    ├─ Router builds cross-provider same-model candidate pool
    ├─ KeyHub.reserve_key() → pre-reserve RPM/RPD before upstream call
    ├─ Provider.chat() / Provider.chat_stream()
    │   ├─ Success → KeyPool.report_reserved_success() → return response
    │   └─ Failure → KeyPool.report_failure(status_code)
    │       ├─ 429 → RateLimited (Retry-After header/body if available, otherwise local backoff)
    │       ├─ 401/403 → Disabled (permanent)
    │       └─ 5xx/timeout → fail_count++
    │           └─ threshold reached → Cooldown
    └─ Try next provider in fallback chain
```

## Concurrency Model

- **DashMap** for provider registry — lock-free concurrent reads
- **parking_lot::RwLock** for state persistence — low contention
- **tokio::sync::mpsc** for SSE stream channels
- **AtomicU64** for request/error counters — lock-free increments

## Data Flow

```
config.yaml ──load──→ Config (Arc)
                      │
                      ├── Provider instances → DashMap<String, BoxedProvider>
                      ├── KeyHub → DashMap<String, KeyPool>
                      ├── HealthRegistry → DashMap<String, RwLock<HealthState>>
                      └── Router → holds refs to all above

state.json ◀──save──→ PersistedState (every 30s)
             ──load──→ Restore key states on startup

models.cache ◀─save──→ Provider model lists (optional future enhancement)
```

## Technology Choices

| Component | Choice | Reason |
|-----------|--------|--------|
| Web framework | axum | Tower ecosystem, async, widely adopted |
| HTTP client | reqwest | Async, streaming support |
| Concurrent map | dashmap | Lock-free reads, ideal for provider registry |
| Async runtime | tokio | Industry standard |
| Serialization | serde_json + serde_yaml | Config & state |
| Logging | tracing | Structured, async-aware |
| Error handling | thiserror | Ergonomic error types |
