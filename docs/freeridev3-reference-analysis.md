# FreeRideV3 Reference Analysis

Source reviewed: `G:\ai\FreeModels\FreeRideV3`.

## Why It Is Relevant

FreeRideV3 has the same core goal as this gateway: one local OpenAI-compatible endpoint, multiple free-tier providers, per-key failover, model auto-selection, and agent compatibility layers.

## Directly Useful Patterns

1. Provider/key chain walking
   - FreeRide builds a `(provider, [keys])` chain per request.
   - Providers and keys are sorted by rolling health before attempts.
   - Rate limits cool the exact key, then the loop tries sibling keys before other providers.

2. Provider-owned error classification
   - FreeRide keeps provider quirks inside each provider plugin's `classify_error`.
   - The failover loop branches on normalized `ErrorKind` values.
   - This is cleaner than spreading provider-specific 403/404/body parsing rules across router/admin paths.

3. Model health cache
   - `freeride audit-models` probes provider catalog models and persists `(provider, model)` verdicts.
   - Auto routing excludes known-broken models without probing on the hot path.
   - This directly addresses catalog ghosts: models returned by `/models` but rejected by inference.

4. Buffer-first-chunk streaming failover
   - FreeRide holds the first SSE chunk until upstream proves headers plus first event.
   - Failover remains possible before any bytes are shipped to the client.
   - After first bytes are sent, mid-stream takeover is not attempted.

5. Structured failure reports
   - Exhausted requests return a structured `tried` list with provider, keys tried, last error, and retry hints.
   - This makes client-visible failures easier to diagnose than one flattened upstream error.

6. Real usage collection
   - FreeRide forces `stream_options.include_usage = true` for OpenAI-compatible streams.
   - That reduces undercounting for providers that support final usage chunks.

## Cautionary Findings

- FreeRide's Cerebras provider excludes known-broken catalog IDs, including `gpt-oss-120b`, because their audit found `/models` can advertise IDs that inference rejects.
- Therefore fixed probe model IDs are weaker than refreshed provider/key model inventory plus live probe results.
- Provider Test and key Validate in this gateway should not invent fallback models. They should use discovered inventory or explicit user configuration only.

## Current Gateway Alignment

Already implemented or mostly aligned:

- Per-key cooldown and rate-limit state.
- Deployment attempt logs with provider/model/key fingerprints.
- Conservative validation updates: only confirmed auth/rate-limit outcomes change key state.
- Stream body failure accounting and next-request key avoidance.
- Reported vs estimated token splits for usage statistics.

Gaps to consider next:

- Add explicit `(provider, model)` health audit cache like FreeRide's `model_health.py`.
- Use model-health verdicts in adaptive routing candidate scoring.
- Add structured all-provider failure payloads with a `tried` breakdown for user-facing API errors.
- Force `stream_options.include_usage = true` when forwarding OpenAI-compatible streaming requests, when the upstream accepts it.
- Move more provider-specific error classification into provider modules.

## Recommended Probe/Refresh Design

Separate model refresh from model health probing:

- Refresh:
  - Calls provider model-list APIs.
  - Updates model/key inventory.
  - Should be cheap and safe to run from UI.
  - Does not prove inference works.

- Audit/probe:
  - Sends real minimal chat requests, for example `max_tokens: 5`.
  - Consumes the same provider/key quota as user traffic.
  - Runs only when explicitly requested, scheduled at a conservative interval, or needed for half-open recovery.
  - Writes a TTL cache keyed by `(provider, key_id, model_id)` or at least `(provider, model_id)`.

Initial policy:

- Do not probe on every UI refresh.
- Default audit TTL: 24h for model health, shorter only for half-open key recovery.
- Probe no more than a small sample per provider by default; full model audit should be an explicit action.
- Route `auto` away from models with fresh `model_not_found`, `quota_exhausted`, `auth`, `rate_limit`, `timeout`, or malformed-response verdicts.
- A successful user request should also refresh the same deployment's health verdict.
- Failed probes should not automatically disable keys unless the error is confirmed auth or rate-limit.

UI should expose both actions:

- `Refresh Models`: inventory only.
- `Audit Models`: real quota-consuming probe, with a warning and progress/result table.
