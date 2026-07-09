# Key Availability Improvement Plan

Date: 2026-07-05

This is the execution plan for long-running key availability reliability.
Keep this file focused on implementation order and acceptance criteria.
Do not put classification rules here; rules belong in
`docs/key-availability-rules.md`.

## Goal

Make the gateway stable for personal multi-key use even when individual keys,
models, or providers become temporarily unreliable.

The goal is not perfect one-shot judgment of every upstream error. The goal is:

- bad keys do not block healthy alternatives,
- temporarily unavailable keys can recover automatically,
- permanent-looking decisions require strong evidence,
- status output explains why a key is skipped and which model triggered it,
- manual config-based key management remains simple.

## Non-Goals

- No Hermes-style `auth add/list/remove` work for now.
- No separate runtime credential store for now.
- No attempt to write dynamic keys back into `config.yaml`.
- No automatic disabling from ambiguous provider errors.

Manual `config.yaml` key management is acceptable for this project.

## Current Gap

The current system already has:

- provider-level key pools,
- multiple keys per provider,
- key rotation strategies,
- `RateLimited`, `Cooldown`, `Probing`, and `Disabled`,
- deployment attempt records,
- deployment routing penalty and cooldown,
- admin status and recent attempt views.

The important remaining gap is that recovery and classification are uneven:

- `Cooldown` and `RateLimited` can recover,
- `Disabled` has a narrow recovery path,
- ambiguous `403`/quota/model errors can still be over-interpreted,
- key-level state and deployment-level state are not yet cleanly separated in all paths,
- status does not always show the reason, confidence, and next probe time.

## Today's Plan: 2026-07-05

Direction:

- Reference Hermes' practical multi-key/multi-provider behavior: when one key
  is bad, quickly skip it and keep serving with other keys/providers.
- Use `provider + key_id` as the primary scheduling state for key availability.
- Keep `model_id` only as evidence: which real model triggered the key decision.
- For today's personal-use scheduling policy, treat a strong real-model failure
  as key-wide unavailable by default.
- Use a 24-hour automatic recovery window for likely daily/free-pool recovery,
  unless upstream `Retry-After` or reset evidence says otherwise.
- Recovery must be half-open/probing, not immediate healthy promotion.

Work items:

1. Add a central failure classification vocabulary for real chat outcomes:
   - short rate limit,
   - daily/free-pool exhausted,
   - monthly/balance exhausted,
   - real-model temporarily unavailable,
   - model/region diagnostic,
   - WAF/proxy diagnostic,
   - confirmed auth failure,
   - transient upstream failure.
2. Change key state handling so strong real-model availability failures create
   a key-wide recovery cooldown.
3. Keep writing exact model evidence for `provider + key_id + model_id`.
4. Candidate selection should skip a key in key-wide recovery cooldown before
   considering model evidence.
5. After 24 hours, move the key into probing/half-open recovery and allow only
   one proof request.
6. On probe success, restore the key to normal routing.
7. On probe failure, reopen cooldown and keep other keys/providers serving.
8. Status/admin should show:
   - key-wide cooldown reason,
   - affected model that triggered it,
   - cooldown remaining,
   - next probe time,
   - last evidence category.

Today's acceptance:

- A real target-model quota/unavailable failure makes only that exact key
  unavailable for normal routing.
- Another key from the same provider can immediately take over.
- Other providers remain eligible.
- The exact failing model is still recorded as evidence.
- A guessed validation-model failure does not key-wide cool the key.
- A model/region/WAF diagnostic does not key-wide cool the key.
- After about 24 hours, the key can automatically recover through one probe.

## Phase 1: Stop Permanent Misclassification

Priority: highest.

Changes:

- Only confirmed invalid credential evidence can set a key to `Disabled`.
- Plain or ambiguous `403` must not disable a key.
- Model/region/provider-path failures must be recorded as deployment evidence.
- `Disabled` keys should have an automatic low-frequency recheck path.
- Add fields or derived status for:
  - `disabled_reason`
  - `last_evidence`
  - `next_probe_at`
  - `confidence`

Acceptance:

- A model-forbidden or region-forbidden response does not disable the key.
- A disabled key can be rechecked automatically without manual intervention.
- Status can explain why a disabled key is disabled.

## Phase 2: Explicit Quota And Recovery States

Priority: high.

Changes:

- Split short rate limits from quota exhaustion.
- Add or derive states equivalent to:
  - `rate_limited_minute`
  - `quota_exhausted_daily`
  - `quota_exhausted_monthly`
  - `insufficient_balance`
- Use `Retry-After` and reset hints when present.
- If daily quota is detected, cool down until a reasonable reset window.
- If monthly or balance exhaustion is detected, use long cooldown plus low-frequency probe.

Acceptance:

- A daily quota error does not behave like a short RPM limit.
- A short `429` can recover through half-open probing.
- Long quota states do not repeatedly consume traffic.

## Phase 3: Key Availability With Model Evidence

Priority: high.

Changes:

- Treat the stable key availability scheduling unit as:

```text
provider + key_id
```

- Keep `model_id` as evidence for the request that changed key state.
- Keep deployment/model-level state only for diagnostics and future precision.
- Candidate selection must prioritize key-level availability before any model evidence.

Acceptance:

- A strong real-model failure blocks the exact key, not only one model route.
- Status shows the model that triggered the key state.
- Model/region diagnostics still do not disable or key-wide cool the key.

## Phase 4: Central Error Classifier

Priority: medium-high.

Changes:

- Add one central classifier:

```text
classify_upstream_error(provider, status, headers, body) -> FailureKind
```

- Route all admin validation, chat, stream, model discovery, and health logic
  through the same classification vocabulary.
- Keep provider-specific text matching inside this classifier, not scattered
  throughout router/keyhub/provider code.

Acceptance:

- The same upstream error receives the same classification from chat and Validate.
- Tests cover OpenRouter, GitHub Models, NVIDIA, Groq, Cerebras, Cloudflare,
  and generic OpenAI-compatible cases where fixtures are available.

## Phase 5: Half-Open Recovery And Probe Budget

Priority: medium.

Changes:

- Cooldown expiry means "eligible for one proof request", not "healthy".
- Add single-flight behavior for recovery probes so concurrent traffic does not
  stampede one recovering key.
- Count all real chat probes against the same request budget as user traffic.
- Keep background probes low-frequency or disabled by default.

Acceptance:

- One expired rate-limited key does not receive concurrent probe traffic.
- A successful probe promotes the exact key.
- A failed probe reopens cooldown with escalation.

## Phase 6: Status And Admin Explanation

Priority: medium.

Changes:

- Status/admin output should answer:
  - current key state,
  - model evidence that triggered current key state,
  - reason for skip,
  - last success,
  - last error category,
  - cooldown remaining,
  - next probe time,
  - whether state came from strong or weak evidence.

Acceptance:

- Looking at `/status` or admin health is enough to understand why a key was not selected.
- Raw key values are never exposed.

## Implementation Order

1. Update classification and `Disabled` policy.
2. Add disabled-key low-frequency recovery probe.
3. Add explicit quota-exhaustion classification and cooldown durations.
4. Expand key-level availability use in candidate filtering and keep model evidence for explanation.
5. Centralize classifier and migrate scattered checks.
6. Improve status/admin explanation fields.
7. Add regression tests for every rule in `docs/key-availability-rules.md`.

## Regression Test Themes

- ambiguous `403` does not disable key,
- confirmed invalid key disables exact key,
- model forbidden remains diagnostic unless the real-model failure is classified as key availability,
- region forbidden does not penalize key,
- WAF/proxy block is diagnostic only,
- daily quota uses long cooldown,
- short rate limit uses half-open recovery,
- disabled key can be restored by successful recheck,
- key-wide cooldown is skipped during candidate selection,
- success only promotes the exact key that succeeded.
