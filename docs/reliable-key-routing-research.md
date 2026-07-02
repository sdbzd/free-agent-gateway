# Reliable Key Routing Research

Date: 2026-07-01

## Problem

The gateway still has weak long-running reliability when many free keys and providers are used together:

- A key can be misclassified because the probe model is unavailable, region-blocked, WAF-blocked, or the provider's `/models` endpoint is flaky.
- Key state is too coarse for real traffic. A key can be valid for one model/provider path but temporarily unusable for another.
- Retry/fallback behavior is mostly request-local. It does not yet build a durable model/key/provider health memory strong enough for hours of unattended service.
- UI status can imply certainty ("cooldown", "available") when the evidence is only a local inference.

## External Patterns

### LiteLLM

LiteLLM Router treats a "deployment" as the routing unit: a concrete model/provider/key/base-url combination. It supports load balancing, cooldowns, fallbacks, request timeouts, and fixed/exponential retries. Its docs also call out Redis for production cooldown and usage tracking across instances.

LiteLLM separates several fallback types:

- sibling deployments under the same model name,
- ordered deployments with priority levels,
- model-level fallbacks,
- context-window fallbacks.

LiteLLM health-check routing is also explicit about avoiding false positives: an `allowed_fails_policy` controls how many auth/rate-limit/timeout failures are required before cooldown, and transient health-check errors can be ignored so one noisy probe does not remove a working deployment.

Sources:

- https://docs.litellm.ai/docs/routing
- https://docs.litellm.ai/docs/proxy/reliability
- https://docs.litellm.ai/docs/proxy/health_check_routing
- https://docs.litellm.ai/docs/proxy/load_balancing

### Portkey

Portkey models reliability as composable strategies: load balancing, fallback, conditional routing, retry, timeout, and tracing. Important points for this project:

- Fallbacks can trigger only on selected status codes, e.g. `429` and `503`.
- A fallback target can itself be a load balancer or conditional router.
- Logs preserve the full fallback chain through a trace id.
- Portkey docs emphasize that fallback/load-balanced models must be compatible in capability, latency, and cost.

Sources:

- https://portkey.ai/docs/product/ai-gateway/fallbacks
- https://portkey.ai/docs/product/ai-gateway/load-balancing
- https://github.com/Portkey-AI/gateway

### FreeLLMAPI

Local reference: `G:\ai\FreeModels\freellmapi`.

Useful design choices already visible in that project:

- Health checks return `healthy`, `rate_limited`, `invalid`, or `error`.
- Transport failures are marked as `error`, not key-invalid.
- Confirmed credential failures are the only health-check path that auto-disables a key.
- Rate/cooldown tracking is keyed by platform/model/key instead of only provider/key.
- Request logs retain platform, model, key id, status, token counts, latency, and error.

### Codex Proxy / Codex Helper

The Codex proxy ecosystem has several reliability mechanisms that map directly to this gateway.

`codex-helper` moved to a runtime route graph instead of a single legacy station state. It supports ordered failover, tag-preferred routing, manual sticky routing, and multi-endpoint providers. Its default session affinity is `preferred-group`: after a temporary fallback, later requests return to the preferred group when it becomes viable again, instead of staying stuck to the fallback. It also exposes route attempts, provider endpoint, preference group, skip reasons, and compatibility context in logs/UI.

`codex-helper` also treats stream error shape as part of reliability: if a Codex stream request fails before route selection because all candidates are depleted, cooling down, or unroutable, it emits a Codex-parseable `response.failed` SSE instead of a raw HTTP error. It records special recovery attempts such as stale `previous_response_id` removal in route attempts.

`claude-code-proxy` demonstrates account-token reliability: auth status is explicit, token expiry is visible, and access tokens are refreshed before expiry with a single-flight guard so concurrent requests do not stampede the refresh endpoint. It also rejects unknown model ids with a clear supported-model list instead of silently selecting a default provider.

Generic Codex Proxy docs use separated status semantics:

- `401`: invalid/disabled proxy API key or missing bearer token.
- `402`: account unavailable or no usable quota.
- `403`: user banned or requested model restricted.
- `429`: concurrency/rate limit.
- `503`: no active upstream channel or all channels failed.

These distinctions are important because they separate credential, account/quota, model restriction, concurrency, and upstream availability instead of collapsing them into "key unavailable".

Sources:

- https://github.com/Latias94/codex-helper/blob/main/CHANGELOG.md
- https://github.com/Latias94/codex-helper/releases
- https://github.com/raine/claude-code-proxy
- https://codedocs.xxworld.org/en/

### Sub2API

Sub2API is closer to an account/subscription pool than a simple API-key pool. That difference matters for this gateway because some upstreams are not plain bearer-key providers: they may use OAuth, subscription entitlements, account reset windows, or product-specific quota headers.

Useful mechanisms:

- Account credentials are stateful: `access_token`, `refresh_token`, `expires_at`, `subscription_tier`, and `entitlement_status` are part of account health.
- Quota display is passive for xAI/Grok: Sub2API does not invent quota values; it records whitelisted rate-limit headers from successful or rate-limited responses, and shows quota as unknown until real evidence exists.
- Error semantics are account-aware: `401` means reauthorization is needed, `403` is entitlement/subscription-tier failure instead of token-refresh loop, and `429` uses `Retry-After` or short cooldown to remove the account from scheduling.
- Antigravity has dedicated Claude and Gemini endpoints, plus optional hybrid scheduling; the docs warn that Anthropic Claude and Antigravity Claude should not be mixed in one conversation context.
- Recent release notes mention protocol-aware thinking-block filtering, OpenAI `/responses` capability probing with tool-call validation, token refresh retry backoff, queue billing moved out of the hot path, SSE `event:error` body preservation, non-JSON 2xx triggering failover, upstream zstd response decompression, stream probe interception, and account-expiry autopause.

Implications for this gateway:

- Add an `account_state` layer for OAuth/subscription providers, separate from key state.
- Track `entitlement_status` and subscription/account capability separately from model availability.
- Treat unknown quota as unknown, not as zero or unlimited.
- Learn quota only from trusted upstream headers or confirmed rate-limit bodies.
- Preserve upstream SSE error body and classify it instead of collapsing it into generic `upstream_error`.
- Treat non-JSON `2xx` as a bad upstream response eligible for failover.
- Enforce conversation-context isolation when two providers use incompatible hidden state/signatures/thinking blocks.

Sources:

- https://github.com/Wei-Shaw/sub2api
- https://github.com/Wei-Shaw/sub2api/blob/main/README.md
- https://sourceforge.net/projects/sub2api.mirror/files/v0.1.137/

## Recommended Model For This Gateway

### 1. Routing Unit

Move from provider-level health to a deployment-level health record:

```text
Deployment = provider + model_id + key_id + base_url/proxy_url + tier
```

Provider-level state should only represent broad network/provider outages. A key-level state should represent credential and quota state. Model-level/deployment-level state should represent model availability, region restrictions, tool support, context limits, and recent quality failures.

### 2. Evidence Types

Every key/deployment decision should record what kind of evidence produced it:

| Evidence | Example | State Effect |
| --- | --- | --- |
| Confirmed credential failure | 401, known bad-key 403 body | Disable exact key |
| Confirmed rate limit | 429, quota-exceeded body, Retry-After | Cool down exact key or quota pool |
| Account auth expired | OAuth 401, invalid refresh token | Reauthorize or pause account |
| Entitlement/subscription failure | 403 subscription tier/account disabled | Pause account or deployment, not all keys |
| Model unavailable | 404 model, region/model forbidden 403 | Disable/cool down provider+model/key route, not key |
| Transient upstream | 5xx, decode body, timeout, connection reset | Increment transient score with threshold |
| Invalid upstream success | non-JSON 2xx, malformed SSE, broken tool-call body | Failover and mark deployment degraded |
| Probe inconclusive | `/models` failure, WAF, region blocked during validate | Record diagnostic only |
| Success | full chat/stream body success | Promote exact key/deployment, reset transient score |

### 3. State Dimensions

Add or evolve persisted state into three layers:

```text
key_state(provider, key_id)
  credential_status: available | probing | rate_limited | disabled | unknown
  quota_status: normal | rpm_limited | rpd_limited | tpm_limited | tpd_limited
  last_confirmed_success_at
  last_confirmed_auth_error_at
  validation_confidence

deployment_state(provider, key_id, model_id)
  availability: healthy | degraded | model_forbidden | region_forbidden | cooldown | unknown
  supports_tools / supports_vision / context_window
  recent_success_count / recent_error_count
  consecutive_failures_by_category
  cooldown_until

provider_state(provider, base_url/proxy_url)
  network_status: healthy | degraded | unreachable
  rolling_latency
  rolling_error_rate
  cooldown_until

account_state(provider, account_id)
  auth_type: api_key | oauth | subscription
  token_expires_at
  entitlement_status
  subscription_tier
  last_refresh_at
  refresh_cooldown_until
```

Implemented phase 1 on 2026-07-01:

- `request_attempts` records every concrete router attempt that reaches an upstream candidate.
- `deployment_state` aggregates health for `provider + model_id + key_id`.
- The router writes non-stream success/failure/empty-output attempts.
- The stream path writes connection success/failure attempts and later body failures.
- Admin read APIs expose the data:
  - `/admin/metadata/attempts?limit=100`
  - `/admin/metadata/deployments`

Implemented phase 2 on 2026-07-01:

- Candidate selection now loads `deployment_state` once per request route.
- Deployments with active `cooldown_until` are skipped before upstream attempts.
- Deployments with higher consecutive failures/error imbalance receive a routing penalty.
- `least_rate`, `least_failed`, and `priority` strategies include deployment penalty in ordering.
- `round_robin` and `random` only rotate inside the lowest-penalty candidate group, so degraded candidates are not promoted just by rotation.

Implemented phase 3 on 2026-07-01:

- Error categories now distinguish:
  - `region_forbidden`
  - `model_forbidden`
  - `rate_limited`
  - `malformed_stream`
  - `empty_response`
  - `auth_failed`
  - `upstream_error`
- Admin provider Test and per-key Validate now record validation probes into `request_attempts` and `deployment_state`.
- Validation still only mutates keyhub state for confirmed auth/rate-limit errors; region/model mismatches are recorded as deployment evidence without disabling the key.
- Browser admin now includes a Health view backed by:
  - `/admin/metadata/deployments`
  - `/admin/metadata/attempts?limit=40`

Next phase:

- Add time-window decay so old deployment failures lose weight after successful traffic.
- Add explicit deployment availability statuses instead of deriving health from counters at render time.

### 4. Validation Policy

`Validate` should not mean "try one random health_check_model." It should mean:

1. Use the selected key's discovered model inventory.
2. Prefer a known cheap text model that belongs to that provider/key.
3. Try up to `N` validation candidates.
4. Classify each result as `success`, `confirmed_key_failure`, `rate_limited`, `model_mismatch`, or `inconclusive`.
5. Only update key state for success, confirmed key failure, or rate-limit.
6. Return the full attempt list to the UI.

The current implementation has already moved in this direction; the next step is to persist attempt-level validation observations.

### 4.1 Codex-Style Route Graph And Affinity

Adopt a route graph rather than a flat provider fallback list:

```text
route_group(monthly/free/preferred)
  -> ordered-failover provider endpoints
  -> deployments(provider + endpoint + key + model)
  -> paid/emergency fallback group
```

Session affinity should be explicit:

- `preferred-group`: default. Return to the preferred group when viable.
- `fallback-sticky`: remain on fallback after a failover.
- `manual-sticky`: pin a provider/model for a user/session.
- `no-sticky`: choose best candidate every request.

This prevents a temporary fallback from becoming a long-term accidental route.

### 4.2 Subscription Account Handling

For OAuth/subscription-style providers:

1. Refresh tokens before expiry with a single-flight guard.
2. If refresh fails with a retriable transport error, mark account `refresh_error` without disabling it.
3. If refresh fails with `invalid_refresh_token` or session termination, mark account `reauthorization_required`.
4. Treat `403` entitlement/subscription failures as account/deployment state, not generic key failure.
5. Keep account reset/quota windows as a separate scheduler input.
6. Never mix providers with incompatible hidden conversation state inside one session unless the client explicitly resets context.

### 5. Runtime Routing Policy

For normal chat requests:

1. Build candidates at deployment granularity.
2. Exclude disabled keys and hard model-forbidden deployments.
3. Prefer deployments with matching capability: tools, vision, context, streaming.
4. Sort by:
   - not cooling down,
   - lower quota usage,
   - lower recent error score,
   - lower latency,
   - configured priority.
5. Retry within same model family/key pool first for 429/5xx/timeout.
6. Fallback across provider only after compatible candidates are exhausted.
7. Never fallback embeddings across incompatible models.
8. For streams, record success only after body completion.

### 5.1 Provider Cooldown Profiles

Provider cooldown rules should combine official limits, response headers, and
observed error bodies. They should not mark a key healthy merely because a local
timer expired.

#### OpenRouter

Confirmed rules:

- `Retry-After` or a parseable reset field in the error body is authoritative.
- OpenRouter documents that adding accounts or API keys does not increase the
  global capacity limit; free model variants have shared limits, including
  `:free` request-per-minute and request-per-day limits.
- `/api/v1/key` can expose key credit and usage data. This is useful for passive
  UI display, but routing still needs request-attempt evidence because upstream
  provider-specific free pools can fail before account credit is exhausted.

Routing rules:

- Treat `429` from OpenRouter itself as account/key quota pressure.
- Treat nested provider errors in OpenRouter metadata, such as upstream Google AI
  Studio or OpenInference `429`, as a deployment/model cooldown, not proof that
  every OpenRouter key is invalid.
- If the error says "temporarily rate-limited upstream" without seconds, use the
  local escalation ladder and require half-open proof before promoting the key.
- Do not assume that buying credits or seeing low displayed usage means a free
  upstream provider pool is available.

#### NVIDIA Hosted NIM

Public reset semantics are less explicit than OpenRouter/Gemini. The project
should therefore use conservative runtime evidence:

- `429`: immediate cooldown for the exact key/deployment; honor
  `Retry-After` if present.
- Long request timeout / connection send failures around the provider timeout:
  mark as transient deployment pressure, not credential failure.
- Repeated timeout/`5xx`/decode failures should open the deployment circuit and
  prefer another key/provider. Do not keep the user request waiting through many
  120s attempts.
- Lower concurrency for NVIDIA candidates when repeated long-hang failures are
  seen; timeout failures are more costly than fast `429` because they block the
  request path.

#### OpenCode

The official public limit surface is not stable enough to encode hard quota
numbers. Treat it as an OpenAI-compatible provider with evidence-based states:

- `/models` decode or `5xx` failures are inventory/discovery uncertainty, not
  proof that the key is bad.
- Chat `401` or confirmed auth `403` disables the exact key.
- Chat `429` cools down the exact key/deployment and escalates if no retry
  seconds are available.
- Chat `5xx`, body decode failure, malformed stream, or empty response degrades
  the deployment after threshold; it should not immediately disable a key.

#### Google Gemini Free API (future provider)

Gemini free API should not be modeled as simple independent API-key capacity.
Google documents that rate limits are applied per project, not per API key, and
that RPD resets at midnight Pacific time.

Routing rules for the future Google provider:

- Add a project/account quota pool above key state.
- Track RPM, input TPM, and RPD independently.
- Reset the local RPD window at Pacific midnight, not local server midnight.
- Multiple keys from the same Google Cloud project must share one quota pool.
- A `429 RESOURCE_EXHAUSTED` should cool down the project/model quota pool even
  if individual keys are syntactically valid.

### 5.2 Probe Budget Policy

Provider Test, key Validate, half-open recovery probe, and background health
probe all consume the same upstream request budget as user calls. They should
not be counted separately for quota purposes.

Rules:

- Every real chat probe increments the same per-key RPM/RPD counters.
- Background probes are disabled or very low frequency by default.
- A cooldown expiry only allows one half-open proof request; it does not make
  the key available immediately.
- Manual Validate/Test may spend a request, but its result is recorded with the
  same `request_attempts` and `deployment_state` machinery.
- `/models` discovery does not consume generation tokens, but it can still spend
  HTTP rate budget and trigger WAF/429 behavior.

### 5.3 Local Context Compression

RTK can be used as a local preprocessor for large tool/log outputs before they
reach an upstream LLM. This reduces model-side token pressure but does not change
provider request-count limits.

Rules:

- Only compress large `role=tool` content by default.
- Never compress ordinary user/system/assistant messages in the first phase.
- Use `rtk pipe --ultra-compact` with a short timeout.
- If RTK fails, times out, or produces larger output, keep the original content.
- Record actual upstream token usage when providers report it; estimated token
  usage should remain marked as estimated.

### 5.4 Attempt Log Analysis Without Auth APIs

Many free providers do not expose reliable auth/account quota APIs. For those
providers, the gateway should use its own structured attempt log as the primary
evidence source.

Implemented behavior:

- `GET /admin/metadata/attempts/analyze?limit=100` performs local rule-based
  analysis with no extra upstream model call.
- `GET /admin/metadata/attempts/analyze?limit=100&use_model=true&model=chat`
  asks an available model to analyze the recent attempts.
- Model analysis is intentionally explicit because it is a real routed chat
  request and consumes provider/key budget.
- The prompt includes provider, model, request id, error category, status,
  fallback, cooldown seconds, and stable key fingerprints only.
- The admin Health page shows the local rule-based analysis by default.

This does not replace quota APIs when they exist; it fills the gap for providers
that only expose behavior through success, 429, 403, timeout, malformed response,
or stream-body failure.

### 6. Circuit Breaker

Implement a small circuit breaker per provider and per deployment:

```text
closed -> open -> half_open -> closed
```

- `closed`: normal traffic.
- `open`: skip deployment until cooldown expires.
- `half_open`: allow one recovery probe/request.
- Success closes the circuit.
- Failure reopens with escalated cooldown.

Use category-specific thresholds:

- auth: 0-1 allowed failures
- 429: immediate cooldown, honor Retry-After
- timeout: 2-3 allowed failures
- 5xx/decode/upstream: 2-3 allowed failures
- model_forbidden: hard disable provider+model route until inventory refresh
- WAF/region: diagnostic, not key failure

### 7. Observability

Every user-visible failure should have an attempt trace:

```json
{
  "request_id": "...",
  "attempts": [
    {
      "provider": "openrouter",
      "model": "google/gemma-4-31b-it:free",
      "key_id": "key-...",
      "status": 429,
      "classification": "rate_limited",
      "cooldown_s": 600,
      "fallback": true
    }
  ],
  "final_provider": "opencode",
  "final_model": "...",
  "final_status": "success"
}
```

UI should show:

- key status with confidence and last evidence,
- account/subscription status when present,
- per-model/deployment availability,
- recent attempts by request id,
- why a key/model was skipped,
- whether Validate changed key state.
- Codex-compatible stream failures should be emitted in the client's expected event shape when the request is already in streaming mode.

## Implementation Phases

### Phase 1: Stop False Positives

Already partly implemented:

- Admin Test/Validate pick models from real key/provider inventory first.
- Model/region 403 and WAF 403 do not penalize key state.
- Watcher model discovery errors do not penalize key availability.
- Validate transient errors return `inconclusive`.

Remaining:

- Persist validation observations with `classification`, `model`, `status`, `changed_key_state`.
- Show these observations in UI.

### Phase 2: Deployment State

Add persisted `deployment_state(provider, key_id, model_id)`:

- `last_success_at`
- `last_error_at`
- `last_error_category`
- `consecutive_failures`
- `cooldown_until`
- `availability`

Router candidate selection should consult this before selecting a key.

Also add `account_state` for OAuth/subscription providers:

- `token_expires_at`
- `refresh_status`
- `entitlement_status`
- `subscription_tier`
- `reauthorization_required`
- `account_cooldown_until`

### Phase 3: Attempt Tracing

Create a bounded in-memory plus hourly persisted request-attempt log:

- record each provider/key/model attempt,
- include classification and fallback reason,
- expose admin endpoint and UI table.
- include route graph group, endpoint, affinity policy, skip reason, and compatibility reason.
- preserve structured upstream error bodies, including SSE `event:error`, where safe.

### Phase 4: Circuit Breakers And Half-Open Probes

Replace simple fail counters with category-specific circuit breakers:

- auth / rate limit / timeout / 5xx / decode / stream-body / model-forbidden.
- half-open recovery probes.
- no global provider cooldown unless enough independent keys fail.
- single-flight refresh/probe guards for account-token providers so concurrent traffic does not stampede refresh/validation.
- classify malformed successful responses, such as non-JSON `2xx`, as failover-eligible deployment failures.

### Phase 5: Capability-Aware Fallback

Make fallback compatibility explicit:

- tools required,
- vision required,
- min context window,
- structured output required,
- embedding dimension/family.

Fallback should prefer same model family, then compatible quality tier, then emergency lower-quality models only when configured.

## Near-Term Code Changes Recommended Next

1. Add `ValidationObservation` storage and admin endpoint.
2. Add `DeploymentStateStore` with tests for model-forbidden vs key-forbidden.
3. Change router candidate selection to skip deployment cooldowns before key cooldowns.
4. Add per-request attempt trace and include it in logs/admin UI.
5. Add a background health probe that validates one known-good model per key without changing key state on inconclusive errors.
6. Add route graph groups and affinity policies: `preferred-group`, `fallback-sticky`, `manual-sticky`, `no-sticky`.
7. Add client-shape-aware stream failure responses for pre-route failures.
8. Add `AccountStateStore` for OAuth/subscription providers with token refresh, entitlement, and reauthorization states.
9. Add response-shape validation: non-JSON `2xx`, malformed SSE, and broken tool-call JSON should trigger failover and deployment degradation.
