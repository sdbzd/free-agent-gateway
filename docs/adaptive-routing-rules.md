# Adaptive Routing Rules

This document records the current routing and failover rules for the browser-served gateway.

## Route Namespaces

The gateway exposes several OpenAI-compatible route namespaces:

- `/v1/...`: legacy model-first routing with provider fallback.
- `/auto/v1/...`: adaptive automatic model selection.
- `/agents/{agent}/v1/...`: adaptive selection scoped by agent profile.
- `/{provider}/v1/...`: adaptive selection constrained to one provider.
- `/provider-groups/{group}/v1/...`: adaptive selection constrained to a configured provider group.

The provider group format is intentionally explicit. Example:

- `/provider-groups/free_cloud/v1/models`
- `/provider-groups/free_cloud/v1/chat/completions`

The direct provider format remains:

- `/openrouter/v1/models`
- `/openrouter/v1/chat/completions`

## Provider Groups

Provider groups are configured under `adaptive_routing.routing_groups`.

Current local group:

- `free_cloud`
  - providers: `openrouter`, `nvidia`, `opencode`, `Cerebras`, `groq`, `cloudflare`, `agnes_ai`
  - agents: `coding_agent`, `hermes`, `document_agent`

Provider group routes restrict candidate providers before scoring. A model selected through `/provider-groups/free_cloud/v1/...` must come from one of the group providers.

## Agent Profiles

Agent profiles are configured under `adaptive_routing.agent_profiles`.

Current agent intent:

- `coding_agent`: coding and reasoning tasks.
- `hermes`: tools and reasoning tasks.
- `document_agent`: document, long-context, and vision tasks.

Agent routes first apply the agent's provider groups, then score candidates by task fit, health, quota, cost, and reliability.

## Model Capability Sources

Model capability metadata is not purely guessed. It is assembled from these sources:

1. Public OpenRouter metadata from `https://openrouter.ai/api/v1/models`.
2. Provider `/v1/models` responses when providers include extended metadata.
3. Runtime observations from real requests.
4. Limited model-name heuristics, currently only for coding-like names.

Supported fields include:

- `supports_vision`
- `supports_tools`
- `supports_reasoning`
- `context_window`
- prompt/completion pricing

Unknown capability is treated as unknown, not true. Explicit `false` can hard-exclude a model for vision or tools. Unknown support receives only a small score bonus when required.

## Candidate Scoring

Adaptive candidates are scored by:

- capability match
- recent success/error counts
- recent 429 and timeout penalties
- quota headroom
- provider priority
- cost/free tier

By default, paid and unknown-tier keys are excluded unless `adaptive_routing.allow_paid` is enabled.

## Key Limit Handling

A key that receives a rate-limit error must leave the available pool immediately.

Rules:

- HTTP `429` sets key status to `RateLimited`.
- `Retry-After` is honored when present.
- Without `Retry-After`, cooldown escalates: 120s, 600s, 3600s, then 86400s.
- Rate-limited keys are skipped by key acquisition, candidate selection, discovery, and fallback.
- Expired rate-limit cooldowns move to `Probing`, not directly to `Available`.
- `Probing` is a retry-eligible state, but it is not counted as available capacity.
- Only a successful upstream request promotes `Probing`, `RateLimited`, `Cooldown` from a 429, or manually restored blocked keys back to `Available`.
- `401` and real auth `403` disable the key.
- Cloudflare/WAF-style `403` is treated as transient, not credential failure.
- Model/region forbidden `403` is a model/provider-access result, not proof the key is bad.
- Model discovery failures do not change key availability; they only update discovery error state/backoff.
- Repeated transient upstream failures move a key to `Cooldown`.
- Streaming response body failures move the key to `Cooldown` immediately.
- Provider Test, key Validate, half-open recovery probes, and background health probes are real upstream requests. They consume the same key/provider quota budget as user calls and are counted by the same key counters.
- Provider Test and key Validate must choose a model from that provider/key's known model inventory, or from an explicitly configured `health_check_model`. There is no generic fallback such as `gpt-4o-mini`, and no provider-specific hardcoded probe model. If no model inventory or explicit probe model exists, validation is inconclusive and should ask for model refresh/configuration instead of guessing.

The intended behavior is continuous service takeover: if one key is limited, the same provider's next healthy key should be tried; if that fails, provider fallback should continue.

Provider-specific first-pass profiles:

- OpenRouter: honor `Retry-After`; nested upstream provider `429` is a deployment/model cooldown, not proof that every OpenRouter key is invalid. Free model `:free` capacity can be exhausted even when account credit display is low.
- NVIDIA: `429` cools down the exact key/deployment; repeated long timeouts, send failures, or `5xx` open the deployment circuit and should quickly move to another key/provider.
- OpenCode: `/models` decode/`5xx` failures are discovery uncertainty. Chat `401`/confirmed auth `403` disables the exact key; chat `429` cools it down; `5xx`/decode/malformed-stream failures degrade after threshold.
- Groq and Cerebras: if the provider is otherwise reachable but validation fails on one model, treat that as model-candidate mismatch or upstream/transient evidence first. Do not mark the whole provider or key bad unless the failure is confirmed `401`, real auth `403`, or `429`. Prefer refreshed provider/key model inventory over static probe IDs.
- Hugging Face: use the Inference Providers OpenAI-compatible router at `https://router.huggingface.co/v1`. HF model suffixes such as `:fastest`, `:cheapest`, `:preferred`, or `:<provider>` are provider-native routing policy and should be preserved when present. HF credits are budget-based, so quota exhaustion should be treated as provider/key budget state rather than model invalidity.
- Google Gemini free API, when added, must be scheduled by project/account quota pool as well as key, because free limits are project-scoped and daily request windows reset on Google's documented schedule.

## Token Accounting

Regular token totals include both input/prompt tokens and output/completion tokens.

The gateway now records token totals by source:

- `reported_*_tokens`: exact upstream `usage` fields returned by the provider.
- `estimated_*_tokens`: local estimates used when the provider response did not include usage.
- `token_reported_requests`: request-count coverage, not token-count precision.
- `token_estimated_requests`: requests whose token totals came from local estimation.

Dashboards should display reported and estimated token totals separately. A large
estimated share means the usage trend is still useful for scheduling/load shape,
but it should not be read as exact billing or quota usage.

## Context Compression

`context_compression` is an optional local preprocessing step. When enabled, the router uses RTK only for large `role=tool` message content before selecting the upstream provider.

```yaml
context_compression:
  enabled: false
  command: "G:\\ai\\AgentsTools\\rtk.exe"
  min_message_tokens: 2000
  timeout_seconds: 3
```

Normal user, system, and assistant messages are not compressed in the first phase. If RTK fails, times out, or produces output that is not smaller, the gateway keeps the original tool output.

## Attempt Log Analysis

When auth/account quota APIs are unavailable, the gateway can still analyze its
own structured routing attempts.

Endpoints:

```text
GET /admin/metadata/attempts/analyze?limit=100
GET /admin/metadata/attempts/analyze?limit=100&use_model=true&model=chat
```

Default behavior is local and free: it groups recent attempts by
provider/model/key fingerprint, error category, fallback count, and hot failing
deployments. The web admin Health page displays this local analysis.

In the current operating model, one key is treated as one independent account
quota pool. If multiple key fingerprints for the same provider/model are
rate-limited in the sample, the analyzer treats that as provider/model account
pool saturation and recommends switching provider/model family before continuing
to rotate the same exhausted accounts.

`use_model=true` is explicit because it sends a real chat request through the
normal router. That model call consumes the same key/provider quota budget as
any user request and is recorded by the same routing/key state machinery.

The model prompt contains stable key fingerprints only, never raw API keys.

## Provider Proxy

Providers can use an explicit proxy:

```yaml
providers:
  groq:
    type: "openai_compatible"
    base_url: "https://api.groq.com/openai/v1"
    proxy_url: "http://127.0.0.1:7890"
```

If `proxy_url` is unset, the HTTP client still honors process-level `HTTP_PROXY`, `HTTPS_PROXY`, and `ALL_PROXY` environment variables. Explicit provider proxy is preferred when only one provider, such as Groq, needs a special network path.

## Admin Test vs Validate

Provider-level `Test` and key-level `Validate` have different meanings:

- `Test` sends one real non-streaming chat request through one selected provider key. It is a provider health sample.
- `Validate` targets one exact key by stable `key_id` and sends one real non-streaming chat request through that key.
- A successful `Validate` is proof for that exact key and can promote it to `Available`.
- A failed `Validate` only updates key state for confirmed credential/rate-limit outcomes: `401`, real auth `403`, or `429`.
- Transport errors, upstream `5xx`, decode failures, WAF blocks, and model/region mismatches are reported as validation diagnostics but are not proof that the key is bad.
- Failed `Test`/`Validate` responses include `validation_status` and `key_state_updated` so the UI can distinguish `inconclusive` from `key_limited_or_invalid`.

`Refresh` is not proof of key recovery. It refreshes provider discovery/health information and may update model inventory, but it should not be read as exhaustive per-key validation.

## OpenRouter Suffixed Models

OpenRouter model IDs such as `google/gemma-4-31b-it:free` are provider-native IDs. The `:free`, `:paid`, and `:extended` suffixes must be preserved when calling OpenRouter.

Important rule:

- If a requested model has an OpenRouter suffix and resolves to OpenRouter, the router should use available OpenRouter free keys even if model discovery cache does not currently contain that model.

Reason:

- OpenRouter model discovery can be incomplete or stale.
- The upstream model ID is already OpenRouter-specific.
- Falling back to generic model inventory matching can incorrectly produce `Model not found`.

Other providers still require model inventory matching to avoid sending OpenRouter-only suffixed IDs to incompatible upstreams.

## Model Family Merge

User-facing model lists merge OpenRouter pricing suffix variants into one canonical family id:

- `model-id`
- `model-id:free`
- `model-id:paid`
- `model-id:extended`

The merge is display-level only. Exact variants remain visible in `/admin/models/families` and remain distinct for routing.

Routing rule:

- If the user requests an exact suffixed id, preserve it.
- If the user requests the canonical bare id, candidate selection can expand to available variants such as `:free`.
- The upstream request always uses the exact selected variant, so provider-specific behavior is preserved.

## Known Limitation

Adaptive streaming is not fully implemented. Streaming requests currently use the legacy `/v1` router unless sent through a route that explicitly rejects adaptive streaming. This means stream failover safety in the legacy router remains important.

Streaming fallback has a hard protocol boundary:

- Before the upstream stream is accepted, the router can try another key or provider.
- After the SSE stream is accepted and bytes may have been sent to the client, the router must not silently splice in another upstream stream.
- If the stream body later fails, the current client request receives a stream error and `[DONE]`; the failed key is immediately cooled down so the client's next retry uses another healthy key or provider.

## Usage Accounting

Usage accounting has three persistence layers:

- `model_usage`: per-provider, per-model, per-day aggregate.
- `model_usage_hourly`: per-provider, per-model, per-hour aggregate for short-window charts.
- `usage_lifetime`: all-time totals that survive future raw row or daily/hourly cleanup.

Token accounting rules:

- If upstream returns `usage.prompt_tokens` / `usage.completion_tokens`, treat those as reported tokens.
- If upstream omits `usage`, estimate prompt and completion tokens from text length using a conservative `chars / 4` fallback.
- Estimated tokens still count toward displayed token volume and key quota accounting.
- `token_reported_requests` only increments for upstream-reported usage, not gateway estimates.
- UI/API consumers should display `token_reporting_coverage` so low-coverage totals are understood as partially estimated.

Exposed endpoints:

- `GET /admin/metadata/usage` returns model summary and includes `lifetime`.
- `GET /admin/metadata/usage/daily?days=7` returns dense daily buckets.
- `GET /admin/metadata/usage/hourly?hours=24` returns dense hourly buckets.
- `GET /admin/metadata/usage/lifetime` returns all-time totals.

## OpenAI Compatibility Surface

The gateway exposes these OpenAI-compatible routes:

- `GET /v1/models`
- `POST /v1/chat/completions`
- `POST /v1/completions`
- `POST /v1/responses`
- `POST /v1/embeddings`

Compatibility rules:

- `/v1/chat/completions` is the canonical execution path.
- `/v1/completions` is a legacy shim. It converts `prompt` into one user chat message, calls the chat router, and returns a text-completion-shaped response.
- `/v1/responses` is a non-streaming shim. It converts `input` and optional `instructions` into chat messages, calls the chat router, and returns a Responses-shaped body with `output_text`.
- `/v1/embeddings` is a generic OpenAI-compatible upstream pass-through to `/embeddings`; it uses the same model resolution, free-key selection, cooldown, and fallback machinery as chat.
- Responses streaming and legacy completions streaming are deliberately rejected until the gateway emits the correct OpenAI event formats.

## Empty And Tool-Call Responses

A non-streaming upstream response is only considered successful if it contains either:

- non-empty assistant content, or
- at least one meaningful assistant `tool_calls` entry with a function name.

If a non-streaming provider returns an empty completion, the router records it as an upstream failure, updates key/model failure learning, and tries the next fallback candidate.

Tool-call normalization rules:

- Non-streaming `tool_calls[].function.arguments` must be a string before returning to clients.
- If a provider returns tool arguments as a JSON object, the gateway serializes that object to a compact JSON string.
- If a provider returns `null` tool arguments, the gateway normalizes them to `"{}"` so no-argument tools remain valid JSON.
- Streaming SSE chunks preserve normal OpenAI delta behavior.
- Streaming chunks with non-string `delta.tool_calls[].function.arguments` are normalized to string chunks before forwarding.
- Streaming tool-call argument fragments are accumulated internally until stream completion.
- If accumulated streaming tool-call arguments are not valid JSON, the stream is recorded as an upstream failure; the reserved key is cooled down and the client receives a stream error instead of a silent success.
