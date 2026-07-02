# Roadmap

## v0.1 — Foundation ✅

- [x] Single EXE deployment
- [x] Windows + Linux support
- [x] OpenAI-compatible API (`/v1/chat/completions`, `/v1/models`)
- [x] Provider abstraction (GitHub Models, NVIDIA NIM, OpenAI-compatible, Ollama)
- [x] KeyHub — multi-key rotation per provider
- [x] Routing strategies (RoundRobin, Random, LeastFailed, LeastRate, Priority)
- [x] Automatic fault tolerance (429 → RateLimited, 401/403 → Disabled, 5xx → Cooldown)
- [x] Provider fallback chain
- [x] Agent-aware routing
- [x] Model aliases
- [x] SSE streaming support
- [x] Health watcher (background task)
- [x] State persistence (`state.json`)
- [x] Environment variable expansion in config
- [x] Security (log redaction for sensitive headers)
- [x] Management API (`/health`, `/status`, `/metrics`, `/providers`)
- [x] Unit and integration tests

## v0.2 — Enhanced Reliability

- [ ] Smart model discovery cache with TTL
- [x] Retry-After tolerant 429 backoff (header, body fallback, local escalation)
- [x] Per-key RPM/RPD pre-reservation before upstream calls
- [ ] Circuit breaker pattern (half-open state for recovery probing)
- [ ] Request deduplication
- [ ] Per-model rate limiting
- [ ] Graceful degradation modes
- [ ] Config hot-reload (watch `config.yaml` for changes)

## v0.3 — Observability ✅

- [x] **Admin Dashboard** — In-browser single-page HTML served at `/admin`, with Provider/Key/Model status, exhausted alerts, smart polling, cooldown countdown, real-time event stream
- [x] **Chat Test** — In-browser provider/key/model chat testing with streaming support
- [x] Prometheus metrics endpoint (`/metrics/prometheus`)
- [ ] Structured JSON logging mode
- [ ] Request tracing (OpenTelemetry compatible)

## v0.4 — Advanced Routing

- [x] Cross-provider same-model candidate pooling
- [ ] Per-model cost-based routing
- [ ] Latency-aware routing (route to fastest responding provider)
- [ ] Geographic routing (prefer closer providers)
- [ ] Request queueing with backpressure
- [ ] A/B testing between models

## v0.5 — Multi-Modal & Extensions

- [ ] Embedding API support (`/v1/embeddings`)
- [ ] Image generation support (`/v1/images/generations`)
- [ ] Audio transcription support (`/v1/audio/transcriptions`)
- [ ] Tool/function calling pass-through
- [ ] Vision model support

## v0.6 — Distribution

- [ ] WebSocket transport for bidirectional streaming
- [ ] gRPC transport option
- [ ] Gateway-to-gateway federation
- [ ] Load balancing across multiple gateway instances
- [ ] Shared state via file lock (multi-gateway coordination)

## v0.7 — Security Hardening

- [x] **Token usage tracking** — Streaming responses now extract real usage from final SSE chunk, tracked per-key (TPM/TPD)
- [ ] API key validation and enforcement
- [ ] IP allowlist/blocklist
- [ ] Request size limits
- [ ] Response content filtering

## v0.8 — Developer Experience

- [ ] Interactive CLI for testing and management
- [ ] Web management UI
- [ ] Config validation and linting
- [ ] Provider health check simulation mode
- [ ] OpenAPI spec generation

## Non-Goals

These are explicitly **out of scope** for this project:

- ❌ SaaS / multi-tenant
- ❌ User authentication / authorization system
- ❌ Enterprise admin console
- ❌ Billing / usage metering system
- ❌ Audit logging to external systems
- ❌ Database dependency (PostgreSQL, MySQL, Redis, etc.)
- ❌ Docker / Kubernetes deployment
- ❌ Nightly Rust features
