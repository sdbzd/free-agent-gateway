# Key Availability Rules

Date: 2026-07-05

This is the rulebook for key availability decisions.
Keep this file focused on classification and state-transition rules.
Implementation sequencing belongs in `docs/key-availability-improvement-plan.md`.

## Rule Change Process

Do not change these rules casually.

Before changing any availability rule, first write down:

- the observed problem or failure case,
- the evidence gathered from logs, code, provider behavior, or external references,
- why the current rule is wrong or incomplete,
- the proposed new rule,
- the expected effect on key, deployment, provider, and quota-pool state,
- the regression tests or manual verification needed.

Only after that reasoning is recorded should the rule itself be changed.

Rule changes should prefer narrow fixes over broad policy changes. If evidence is
ambiguous, record the uncertainty and avoid making permanent key-disabling
behavior more aggressive.

## Rule Change Notes

### 2026-07-05: Prefer Key-Wide Recovery Cooldown For Real-Model Failures

Observed problem:

- The project has already gone through several rounds of key availability
  improvements, but long-running personal use still suffers when a key is
  repeatedly selected after a real requested model fails.
- Hermes-style multi-key/multi-provider behavior is valuable because a bad key
  is skipped quickly and healthy keys keep serving.
- For this personal gateway, keys are added manually and usually used against a
  small set of real target models. If one real target model becomes unavailable
  on a key, retrying the same key for nearby routes often wastes quota and hurts
  reliability more than it helps.

Evidence and reasoning:

- The current design can track `provider + key_id + model_id`, which is useful
  as model evidence for diagnostics and future provider-specific precision.
- In practice, free-provider failures often reflect key/account/free-pool
  availability rather than a single isolated model.
- Many such failures are likely to recover after a daily reset window, so a
  24-hour automatic recovery probe is safer than permanent disablement.
- This is a personal-use policy tradeoff: prefer stable skip-and-recover over
  maximally fine-grained retrying.

Rule change:

- Use `provider + key_id` as the primary scheduling key for key availability.
- Keep model evidence as `provider + key_id + model_id`.
- For real chat requests against a real target model, a strong availability
  failure should also place the whole key into a key-wide recovery cooldown.
- Default key-wide recovery cooldown for this case is 24 hours unless upstream
  `Retry-After` or reset evidence says otherwise.
- After the cooldown, the key must return through probing/half-open recovery,
  not directly to healthy.
- Do not apply this key-wide policy to guessed validation models, `/models`
  discovery failures, WAF/proxy failures, or explicit model/region mismatch
  diagnostics.

Expected state effect:

- Model evidence records the exact failing model path.
- Key state prevents the same key from being immediately reused for normal
  routing.
- Other keys under the same provider remain available and can take over.
- The key can automatically recover after the 24-hour window if a probe succeeds.

Verification needed:

- A real-model quota/unavailable failure cools the exact key for about 24 hours.
- Another key from the same provider is selected on the next request.
- A guessed validation-model failure does not key-wide cool the key.
- A model/region/WAF diagnostic still does not disable or key-wide cool the key.
- After cooldown expiry, only one recovery probe is allowed before promotion.

## Core Principle

Never permanently punish a key from weak evidence.

Use the narrowest state that explains the evidence:

- credential facts belong to key state,
- quota and real-model availability facts can belong to key/account/quota-pool state,
- model and region diagnostics still belong to model evidence/deployment diagnostics,
- provider/network facts belong to provider or endpoint state,
- uncertain probe failures are diagnostics.

## State Layers

Use these conceptual layers even if the current code stores them differently.

```text
key_state(provider, key_id)
  credential and broad quota state

deployment_state(provider, key_id, model_id)
  model evidence and diagnostics; not the default key-availability scheduling key

provider_state(provider, base_url, proxy_url)
  endpoint/network/provider outage state

account_or_quota_pool_state(provider, account_id/project_id)
  shared account, subscription, or project quota state
```

## Evidence Strength

Strong evidence can change scheduling state.
Weak evidence can be recorded but must not remove a key from normal scheduling.

Strong evidence:

- known invalid credential response,
- explicit invalid API key/token message,
- clear `401` auth failure from a provider endpoint,
- clear rate-limit/quota response with `429` or quota-exhausted body,
- successful chat or stream completion from the exact key,
- repeated same-category failures that pass the configured threshold.

Weak or inconclusive evidence:

- model not available for this provider/key,
- region restriction,
- Cloudflare/WAF/proxy block,
- `/models` endpoint failure,
- non-JSON model discovery response,
- transient `5xx`,
- timeout,
- stream body decode error,
- malformed provider response,
- one-off empty completion,
- validation with a guessed or unavailable probe model.

## Credential Rules

Only disable an exact key when the credential itself is strongly proven bad.

Disable exact key:

- `401` with normal provider auth semantics,
- `403` with explicit invalid API key/token/authentication wording,
- provider-specific body that clearly says the key/token is invalid,
- manual disable, if such a feature is later added.

Do not disable key:

- model forbidden,
- region forbidden,
- WAF/Cloudflare block,
- provider entitlement mismatch unless it clearly applies to the whole account,
- upstream `5xx`,
- timeout,
- malformed response,
- `/models` discovery failure,
- quota exhaustion.

Disabled keys must still have a recovery path:

- low-frequency automatic recheck,
- manual Validate,
- successful model discovery only if the provider's `/models` endpoint is a
  meaningful credential check,
- successful chat probe from the exact key.

## Rate Limit And Quota Rules

Do not collapse all `429` into the same state.

Short rate limit:

- use when evidence says RPM/concurrency/temporary rate limit,
- honor `Retry-After` if present,
- otherwise use local escalation,
- after cooldown, move to half-open/probing, not directly healthy.

Daily quota exhaustion:

- use when body says daily quota, requests per day, free daily limit, RPD, or
  reset tomorrow,
- cool down until the likely reset window or at least a long daily cooldown,
- allow low-frequency probe after reset.

Monthly/balance/subscription exhaustion:

- use when body says monthly quota, credits exhausted, insufficient balance, no
  credits, billing, subscription, or account quota exhausted,
- do not keep retrying in normal routing,
- use long cooldown plus explicit status explanation.

Shared quota:

- if a provider applies quota per account/project rather than per key, mark the
  shared quota pool, not each individual key.
- multiple keys from the same quota pool must not be treated as independent
  capacity.

## Real-Model, Model, And Region Rules

Keep model-level evidence, but use `provider + key_id` as the default scheduling
state for real chat traffic.

Always record exact model evidence for:

- model not found,
- model forbidden,
- not authorized for model,
- model unavailable,
- region unavailable for this model,
- provider-specific model route rejects the request.

For guessed validation probes, model discovery, WAF/proxy failures, and explicit
model/region mismatch diagnostics:

- do not disable the key,
- do not key-wide cool the key,
- keep the evidence at model/provider diagnostic level.

For real chat requests against a real target model:

- if the failure indicates quota exhaustion, free-pool exhaustion, model
  temporarily unavailable for this key, or another strong key-availability
  failure, place the whole key into a recovery cooldown,
- default that recovery cooldown to 24 hours unless upstream reset evidence is
  more specific,
- also record the exact `provider + key_id + model_id` model evidence,
- after cooldown, recover through probing/half-open state.

This is intentionally more conservative than pure deployment-only scheduling.
For personal multi-key use, stable key rotation is more valuable than repeatedly
testing whether one key can still serve a neighboring model.

## Provider And Network Rules

Provider/path failures should not punish keys by default.

Provider or endpoint evidence:

- WAF/Cloudflare block,
- proxy required or proxy failed,
- DNS/connect failure,
- provider-wide `5xx`,
- malformed provider-wide response,
- stale base URL,
- non-JSON HTML error page.

State effect:

- record provider/endpoint diagnostic,
- maybe provider cooldown after enough independent keys fail,
- do not disable keys.

## Streaming Rules

Streaming success is only confirmed after the body completes successfully.

Rules:

- stream connection success is not final success,
- stream body decode error degrades/cools the exact key only when classified as key availability,
- incomplete tool-call arguments degrade/cool the exact key only when classified as key availability,
- empty stream output degrades/cools the exact key only when classified as key availability,
- once bytes may have reached the client, do not splice in a new upstream stream;
  make the next client retry skip the degraded key when classified as key availability.

## Recovery Rules

Cooldown expiry is not proof of recovery.

Recovery states:

- `cooldown` expiry may return transient failures to selectable state,
- `rate_limited` expiry should enter `probing`,
- `quota_exhausted` should enter low-frequency probe only after reset window,
- `disabled` should enter rare recheck/probe only when safe.

Promotion rules:

- only a successful exact-key request promotes that exact key,
- success resets consecutive failures for that exact state,
- success should not erase unrelated model evidence.

Failure rules:

- failed half-open probe reopens cooldown,
- repeated ambiguous failures may degrade deployment after threshold,
- repeated independent key failures may degrade provider/endpoint after threshold.

## Candidate Selection Rules

Before routing a request, exclude or penalize candidates in this order:

1. key has confirmed disabled credential state,
2. shared quota/account pool is exhausted,
3. key is in active rate/quota cooldown,
4. provider/endpoint is in active cooldown,
5. local RPM/RPD/TPM/TPD counters are exhausted,
6. model/region evidence is considered only when the classifier says it is
   diagnostic and should not key-wide cool the key.

After filtering, rank by:

- lower key/deployment penalty,
- lower local quota usage,
- lower recent failure count,
- lower latency when available,
- configured priority or routing strategy.

## Validation Rules

Validate/Test is not allowed to guess a random model and then punish the key.

Validation must:

- prefer models discovered for that exact key/provider,
- use explicit configured health model only if it is known to be valid,
- classify each attempt,
- mutate key state only for confirmed auth/rate/quota/success evidence,
- record inconclusive failures without changing key availability.

## Status Rules

Status must expose decisions without raw secrets.

Show:

- stable key fingerprint or masked key,
- state,
- reason,
- last success time,
- last error time,
- last error category/status,
- cooldown remaining,
- next probe time,
- confidence/evidence strength,
- affected or triggering model when available.

Never show:

- raw key values,
- bearer tokens,
- full authorization headers.
