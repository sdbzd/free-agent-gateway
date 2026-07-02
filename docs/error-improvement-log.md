# Error And Improvement Log

This document records observed routing, statistics, and availability issues plus the changes made or planned.

## 2026-06-30: `/v1/models` Was Slow

Observed:

- `GET /v1/models` was slow because model listing could trigger live provider discovery.

Improvement:

- Prefer cached inventory where possible.
- Increase model cache TTL default to 10 minutes.
- Keep model list changes visible after cache refresh rather than per-request rediscovery.

Remaining risk:

- Provider discovery can still be slow or fail for providers whose `/models` response is unstable.

## 2026-06-30: User Needed Visible Provider Groups

Observed:

- Provider groups and generated routes were not easy to see in the UI.

Improvement:

- Added admin APIs:
  - `/admin/routing/routes`
  - `/admin/routing/groups`
  - `/admin/routing/adaptive`
- Added browser Web console route view under `/admin`.
- Exposed generated prefixes, models routes, chat routes, providers, and agents.

Current verified state:

- `free_cloud` group is visible.
- `/provider-groups/free_cloud/v1/models` works.
- `/provider-groups/free_cloud/v1/chat/completions` is exposed.

## 2026-06-30: Token Statistics Were Misleading

Observed:

- Token counts looked inaccurate.
- The UI did not clearly show that regular token accounting should include both input and output.

Improvement:

- Total reported tokens are now prompt/input + completion/output.
- Added `token_reported_requests`.
- Added token reporting coverage so missing upstream usage fields are visible.
- Added daily usage endpoint:
  - `/admin/metadata/usage/daily?days=1|7|30|90`
- Added Kun-style browser statistics UI with heatmap and model breakdown.

Remaining risk:

- Providers that do not return `usage` cannot produce exact token counts. Coverage shows this gap.

Follow-up on 2026-07-02:

- Added separate reported and estimated token aggregates:
  - `reported_prompt_tokens`
  - `reported_completion_tokens`
  - `estimated_prompt_tokens`
  - `estimated_completion_tokens`
- Added hourly and lifetime aggregate preservation for the same split, so raw row cleanup does not erase long-term totals.
- Updated the web statistics UI to show `Reported Tokens` and `Estimated Tokens` separately instead of presenting all tokens as one equally precise number.

## 2026-06-30: Browser-Only Admin Direction

Observed:

- The project should provide a browser UI service only, not Electron or a desktop app.

Improvement:

- Added standalone web project under `web/admin`.
- `/admin` now serves the built browser console.
- `/admin/legacy` keeps the old embedded admin page as a fallback.
- `/admin/usage` remains as a compatibility alias.

## 2026-06-30: Local Config Was Mis-indented

Observed:

- Local `config.yaml` had `routing`, `fallback`, `agents`, `models`, and `providers` nested under `server`, so the service could not parse top-level providers.

Improvement:

- Backed up the file as `config.yaml.bak-before-admin-usage`.
- Moved those sections to top level.
- Added default handling for missing top-level `routing`.

Test:

- Added `test_config_parse_without_routing_uses_defaults`.

## 2026-06-30: OpenRouter `:free` Model Fell Through To `model_not_found`

Observed logs:

- `model=google/gemma-4-31b-it:free`
- OpenRouter key hit 429 and entered cooldown.
- Emergency fallback tried `nvidia` with the OpenRouter-specific model ID and got 404.
- A later request failed before provider call with:
  - `No free keys found for model (stream)`
  - `Model not found: google/gemma-4-31b-it:free`

Analysis:

- Initial request did partly fail over and eventually succeeded through `opencode`.
- Later requests did not correctly take over.
- The router required exact keyhub model inventory for `google/gemma-4-31b-it:free`.
- OpenRouter model discovery cache did not contain that model, so the router returned `model_not_found` before trying OpenRouter.
- This is wrong for OpenRouter-native suffixed model IDs.

Improvement in progress:

- Added provider-level free-key candidate selection for OpenRouter suffixed model IDs.
- Router now treats `openrouter + :free/:paid/:extended` as provider-native and can select available OpenRouter keys without requiring model discovery cache membership.

Tests added:

- `test_openrouter_free_model_uses_openrouter_key_without_inventory_cache`
- `test_rate_limited_key_is_removed_and_next_key_takes_over`

Current state:

- The code change is present.
- The targeted test initially failed due to a test-only snapshot indexing bug, which has been fixed.
- Final verification still needs to be rerun after the current service/process locks are clear.

## 2026-06-30: Rate-Limited Key Must Be Removed From Service

Observed:

- Logs show `Key rate limited, escalating cooldown for 600s`.
- This part is expected: the key is moved to `RateLimited`.

Expected behavior:

- The limited key must not be selected again until cooldown expires.
- Another key from the same provider should take over.
- If provider keys are unavailable, provider fallback should continue.
- The API should keep serving when any alternative candidate exists.

Existing mechanism:

- `report_gateway_error` sends HTTP 429 to `report_failure_with_retry_after`.
- The key becomes `RateLimited`.
- `reserve_key`, `free_candidate_infos`, discovery keys, and fallback skip unavailable or rate-limited keys.

Improvement in progress:

- Added regression coverage ensuring a 429 key is marked `RateLimited` and the next healthy key takes over.

Remaining risk:

- Streaming still uses legacy routing; stream failover must continue to be tested separately from non-stream adaptive selection.

## 2026-06-30: Stream Body Decode Error After Successful Fallback

Observed logs:

- Attempt 3 used OpenRouter key `sk-o...9576`.
- The key received HTTP `429` and entered rate-limit cooldown.
- Attempt 4 used another OpenRouter key `sk-o...2ae8`.
- The upstream stream was established and the chat request was accepted.
- About 29 seconds later the response body failed with:
  - `Provider stream body failed`
  - `Upstream request failed: error decoding response body`

Analysis:

- Provider/key fallback did work before the stream was accepted.
- This failure happened after the SSE stream was already established.
- Once response bytes may have reached the client, the gateway cannot safely splice a second upstream stream into the same client stream.
- Treating this as an ordinary transient failure was too weak because the key would only enter cooldown after the general failure threshold.

Improvement:

- Stream body failures now immediately move the active key to `Cooldown`.
- The current client stream still reports the error because in-band transparent takeover is unsafe at that point.
- The client's next retry should skip the failed key and use another healthy key or provider.

Tests added/updated:

- `test_stream_body_error_records_failure_without_success` now verifies no success is recorded, the key enters `Cooldown`, and the key cannot be reserved immediately after the body failure.

Follow-up on 2026-07-02:

- OpenAI-compatible non-stream responses now read the upstream body as text first and then parse JSON.
- HTTP error responses that are not JSON keep their body preview instead of surfacing as a raw `Reqwest error: error decoding response body`.
- HTTP success responses with non-JSON bodies are classified as upstream response-format errors with a body preview. This gives routing/health analysis evidence without treating the error as auth failure.

## 2026-07-02: Groq And Cerebras Valid Keys Were Shown As Unhealthy

Observed:

- Groq and Cerebras keys were known to work elsewhere, but gateway Test/Validate could still report failure or cooldown-like status.

Analysis:

- Local `config.yaml` enabled `Cerebras` and `groq` but did not set `health_check_model`.
- Generic OpenAI-compatible providers defaulted to `gpt-4o-mini`.
- `gpt-4o-mini` is not a valid provider-native probe model for Groq/Cerebras, so Test/Validate could fail for the probe model rather than the key.
- Provider name casing also matters for grouping and history: local config uses `Cerebras`, while some examples/docs use `cerebras`.

Improvement:

- Removed generic and provider-specific hardcoded validation fallback models from OpenAI-compatible provider construction.
- Removed local/sample Groq and Cerebras fixed probe models. Test/Validate now relies on provider/key model inventory, or on an explicitly configured `health_check_model` when the user deliberately sets one.
- If no inventory or explicit probe model exists, Test/Validate reports that no enabled validation model is available instead of guessing.
- Test/Validate remains conservative: only confirmed `401`, real auth `403`, or `429` updates key state. Model mismatch, region mismatch, WAF/proxy blocks, decode failures, and upstream `5xx` remain diagnostic/inconclusive.
- FreeRideV3 reinforces this rule: its Cerebras provider keeps a known-broken catalog exclusion list because some models can appear in `/models` while inference rejects them. Static probe IDs are weaker evidence than refreshed inventory plus live probe results.

Tests added/updated:

- `test_openai_compatible_provider_does_not_guess_default_health_model`

## 2026-07-02: Hugging Face Provider Interface Added

Observed:

- The gateway needs a Hugging Face provider entry so HF keys can be added later without reworking routing.

Improvement:

- Added `huggingface` as a first-class `ProviderType`.
- The first implementation uses Hugging Face's OpenAI-compatible Inference Providers router at `https://router.huggingface.co/v1`.
- Added disabled `huggingface` provider blocks to `config.yaml` and `config.yaml.sample`.
- The provider intentionally does not invent a health-check model. Refresh Models should populate model inventory, or the user can explicitly set `health_check_model` after confirming a model works for the key.

Tests added:

- `test_huggingface_provider_type_parses`
- `test_create_provider_factory` now covers the `huggingface` provider type.

## 2026-06-30: OpenRouter Native Model Was Misreported As 404 When Keys Were Unavailable

Observed logs:

- `model=google/gemma-4-31b-it:free`
- `providers_checked` included `openrouter`.
- `free_model_counts` did not include `openrouter`.
- Other providers had model counts, but not this exact OpenRouter-native suffixed model.
- The request returned `Model not found: google/gemma-4-31b-it:free`.

Analysis:

- `google/gemma-4-31b-it:free` is an OpenRouter-native model ID.
- If OpenRouter has no currently available free key, the problem is availability, not model existence.
- Returning `404 model_not_found` is misleading and causes clients to treat the model ID as invalid.

Improvement:

- OpenRouter suffixed model routes now return `NoAvailableKeys(openrouter)` when no OpenRouter key is available.
- The stream path logs an extra OpenRouter key status summary, including available, cooldown, rate-limited, disabled, and provider-cooldown counts.
- This separates three cases:
  - model truly not found
  - OpenRouter key pool temporarily unavailable
  - running process has not been restarted onto the newer routing code

Tests added:

- `test_openrouter_free_model_without_available_key_is_not_model_not_found`

## 2026-06-30: Streaming Body Failed Around 30 Seconds

Observed logs:

- `Provider stream established`
- Around 28-29 seconds later:
  - `Provider stream body failed`
  - `Upstream request failed: error decoding response body`

## 2026-07-01: Key/Provider Availability Needed Structured Evidence

Observed:

- Logs could prove that a request switched from one key/provider to another, but the service did not persist the full attempt chain.
- UI/key status could still disagree with real traffic because keyhub status, validation probes, model discovery, and router attempts were not backed by one shared deployment-level fact table.
- Stream requests could be accepted and then fail later in the body; that body failure was visible in logs but hard to query afterward.

Improvement:

- Added `request_attempts` to persist each concrete routing attempt:
  - request id
  - attempt index
  - provider
  - model id
  - stable key fingerprint
  - success/failure
  - error category/status/message
  - cooldown hint
  - whether another fallback was expected
- Added `deployment_state` keyed by provider + model + key fingerprint.
- Non-stream chat attempts now record success, upstream errors, and empty-response failures.
- Stream chat connection attempts now record success/failure, and stream body failures update the same deployment state.
- Added read-only admin endpoints:
  - `/admin/metadata/attempts?limit=100`
  - `/admin/metadata/deployments`

Why this matters:

- The scheduler can now evolve toward LiteLLM/Portkey-style deployment routing rather than relying on provider-wide guesses.
- A provider model that is region-forbidden, rate-limited, malformed, or body-failing can be separated from a truly invalid key.
- UI can show the actual failover chain instead of inferring availability from one probe.

Tests:

- `cargo check`
- `cargo test --test metadata_learning_tests`
- `cargo test router::tests`

Follow-up improvement:

- `deployment_state` is now used during candidate selection.
- Active deployment cooldowns are skipped before attempting upstream calls.
- Consecutive failures and error imbalance add a routing penalty.
- Round-robin/random rotation is limited to the lowest-penalty candidate group so unhealthy deployments are not randomly promoted.

Second follow-up improvement:

- Error categories now split region restrictions, model restrictions, malformed streams, and empty responses instead of collapsing them into generic upstream errors.
- Admin provider Test and per-key Validate now persist their probe attempts into `request_attempts` and `deployment_state`.
- Region/model validation mismatches remain non-key-attributable, so they do not disable otherwise usable keys.
- Browser admin gained a Health view showing deployment state and recent attempts.

Analysis:

- The provider stream had already been accepted, so routing and key selection had succeeded.
- The failure timing matched the configured provider `timeout_seconds` value.
- The stream request used reqwest's per-request `.timeout(...)`, which is a total request lifetime timeout. For SSE, that includes the whole response body, so normal long generations can be aborted by the gateway itself.

Improvement:

- Streaming requests now use a dedicated streaming HTTP client.
- The streaming client uses a bounded connect timeout and a per-read idle timeout.
- It does not set a total response-body timeout for SSE.
- Streaming requests also send `Accept-Encoding: identity` to avoid compressed SSE body handling by intermediaries.

Verification:

- `cargo test --test provider_tests`
- `cargo test --test router_tests`
- `cargo test --test keyhub_tests`
- `cargo check`

## 2026-06-30: Rate-Limited Keys Were Marked Available Before Real Recovery

Observed:

- A key reached `rate_limited`.
- After local cooldown expiry, the gateway marked it `available`.
- The upstream quota had not actually recovered yet, so the key could be selected as healthy and fail again.

Analysis:

- Local cooldown expiry is only a time to retry, not proof of recovery.
- Upstream providers can use hidden/shared pools, delayed quota reset, or imprecise `retry shortly` guidance.
- Treating expiry as full availability made UI and routing too optimistic.

Improvement:

- Expired `rate_limited` keys now enter `probing`, not `available`.
- `probing` keys are not counted as available in status/UI.
- A `probing` key can be selected as a low-priority recovery probe when normal candidates are exhausted.
- Once reserved for a probe, it is temporarily moved to cooldown to avoid concurrent repeated probes.
- Only a successful upstream request promotes the key back to `available`.
- Another 429 sends it back to `rate_limited` with escalated cooldown.

Tests added/updated:

- `test_rate_limited_key_enters_probe_after_expiry`
- `test_rate_limited_probe_success_marks_key_available`

## 2026-07-01: Provider Test Was Not Key-Level Validation

Observed:

- NVIDIA showed two `probing` keys after restart.
- Provider-level `Test` could succeed, but that did not prove every `probing` key was restored.
- The old UI used similar low-contrast button styles for different actions.

Analysis:

- Provider `Test` selects one currently selectable key and sends one real non-streaming chat request.
- It is a provider health sample, not exhaustive key validation.
- A key in `probing`, `rate_limited`, `cooldown`, or `disabled` should only be marked available after that exact key succeeds upstream.

Improvement:

- Added `POST /admin/providers/{name}/keys/{key_id}/validate`.
- The endpoint resolves `key_id` to the exact configured key, sends a real chat request with the provider health-check model, and records success or failure against that key.
- Success promotes blocked/probing key states to `available`.
- Failure still uses the normal key state machine: 429 rate-limit cooldown, auth disable, or transient cooldown/fail counting.
- The legacy admin UI now shows a key-level `Validate` button.
- `Validate` and provider `Test` use distinct blue styling; `Refresh`/restore actions remain visually separate.

Tests added:

- `test_keyhub_can_find_raw_key_by_fingerprint_for_validation`
- `test_rate_limited_manual_validation_success_marks_key_available`

## 2026-07-01: Duplicate `:free` And Bare Model IDs In Model Lists

Observed:

- Some model lists showed both `model-id` and `model-id:free`.
- They may represent different providers or pricing tiers, so collapsing them in routing without retaining variants would be unsafe.

Analysis:

- `:free`, `:paid`, and `:extended` are OpenRouter-style pricing suffixes.
- A bare provider model and an OpenRouter suffixed model can be different upstream services, but they are useful to present as one family to users.
- Display should be merged; execution must preserve the exact upstream variant.

Improvement:

- `/v1/models` and adaptive scoped model routes now merge OpenRouter pricing suffix variants into one canonical family id for display.
- `/admin/models/families` keeps full variant detail: provider, tier, metadata, and exact model id.
- Router candidate selection expands a canonical request like `google/gemma-4-31b-it` to valid variants such as `google/gemma-4-31b-it:free` when available.
- The upstream request still sends the selected exact variant, so provider-specific behavior is preserved.

Tests added:

- `merge_model_families_collapses_pricing_suffixes_for_model_list`
- `test_canonical_model_routes_to_openrouter_free_variant`

## 2026-07-01: Usage Totals Were Too Dependent On Daily Rows And Upstream `usage`

Observed:

- Token totals were hard to reason about when raw/detailed rows might be cleaned later.
- Some providers omit OpenAI `usage`, so token charts could undercount real traffic.
- The UI needed 1-day/hourly views plus lifetime totals.

Analysis:

- Daily aggregates are useful for charts but are not enough for all-time accounting.
- Upstream-reported token usage should remain distinguishable from gateway-estimated token usage.
- A simple fallback estimator is less accurate than provider usage, but better than counting missing usage as zero.

Improvement:

- Added `model_usage_hourly` for dense hourly trend buckets.
- Added `usage_lifetime` for all-time totals that survive future cleanup of lower-level rows.
- Added `GET /admin/metadata/usage/hourly?hours=...`.
- Added `GET /admin/metadata/usage/lifetime`.
- `/admin/metadata/usage` now includes `lifetime`.
- Missing upstream usage is estimated from message/response text, while `token_reported_requests` only tracks true upstream usage.

Tests added/updated:

- `successful_requests_accumulate_model_token_usage`
- `test_usage_estimation_marks_tokens_as_estimated`

## 2026-07-01: Empty Completions And Malformed Tool Arguments Could Be Treated As Success

Observed:

- Some upstreams could return HTTP 200 with an empty assistant reply.
- Tool-call arguments may appear as JSON objects instead of the OpenAI-compatible string form.
- Streaming tool-call deltas can carry provider-specific argument shapes.

Analysis:

- HTTP success is not enough; the response must contain useful assistant content or a tool call.
- Returning an empty answer prevents fallback from rescuing the request.
- Tool-call arguments must be normalized before client SDKs parse them.

Improvement:

- Non-streaming empty completions are now treated as retryable upstream failures and the router tries the next fallback candidate.
- Non-streaming `tool_calls[].function.arguments` objects are serialized to JSON strings.
- Streaming SSE chunks normalize non-string `delta.tool_calls[].function.arguments` before forwarding.
- Stream body completion with no content/tool-call output is recorded as an upstream failure and the key is cooled down for the next retry.

Tests added/updated:

- `test_non_stream_empty_completion_falls_back`
- `test_tool_call_argument_objects_are_repaired_to_strings`
- `test_stream_success_is_recorded_only_after_body_completes`
- `test_stream_body_error_records_failure_without_success`

## 2026-07-01: Fragmented Streaming Tool Calls Could Finish With Broken Arguments

Observed:

- Some providers stream `delta.tool_calls[].function.arguments` in multiple fragments.
- A stream could include a tool call fragment and then end before the accumulated arguments formed valid JSON.
- The gateway previously treated the stream as successful as soon as any tool-call delta appeared.

Analysis:

- OpenAI-compatible clients assemble streaming tool calls client-side, but the gateway still owns key accounting and provider health.
- Recording success for a stream with incomplete tool-call JSON hides provider failure and keeps routing traffic to a bad key/model path.
- `null` tool arguments should be normalized to a valid empty JSON object string for no-argument tools.

Improvement:

- Streaming tool-call argument fragments are now accumulated internally by choice/index until stream completion.
- Complete fragmented arguments are accepted and still streamed normally.
- Incomplete fragmented arguments now produce a stream error, record an upstream failure, and cool down the key instead of recording success.
- Non-streaming `null` tool arguments are normalized to `"{}"` and empty tool-call shells no longer count as useful output.

Tests added/updated:

- `stream_tool_call_state_accepts_complete_fragmented_arguments`
- `stream_tool_call_state_rejects_incomplete_fragmented_arguments`
- `test_stream_fragmented_tool_call_arguments_success_records_success`
- `test_stream_incomplete_tool_call_arguments_records_failure`
- `test_null_tool_call_arguments_are_repaired_to_empty_object_string`
- `test_empty_tool_call_shell_is_not_useful_output`

## 2026-07-01: OpenAI-Compatible Surface Only Covered Chat And Models

Observed:

- The gateway exposed `/v1/chat/completions` and `/v1/models`.
- Some personal tools expect OpenAI's newer `/v1/responses`, older `/v1/completions`, or `/v1/embeddings`.

Analysis:

- Chat remains the strongest common denominator across the configured LLM providers.
- Responses and legacy Completions can be served as shims over chat for non-streaming requests.
- Embeddings should not be converted to chat; it must be forwarded to OpenAI-compatible upstream `/embeddings`.

Improvement:

- Added `POST /v1/responses` as a non-streaming Responses API shim.
- Added `POST /v1/completions` as a non-streaming legacy completions shim.
- Added `POST /v1/embeddings` as a generic OpenAI-compatible upstream pass-through.
- Added router support for generic OpenAI JSON endpoints using existing model resolution, free-key selection, cooldown, and fallback.
- Responses streaming and Completions streaming return explicit unsupported errors instead of silently downgrading.

Tests added:

- `responses_input_string_maps_to_chat_request`
- `completions_prompt_maps_to_chat_request`
- `chat_response_maps_to_responses_shape`

## 2026-07-01: Admin Test/Validate And Discovery Could Misclassify Usable Keys

Observed:

- OpenRouter validation could fail with `403 This model is not available in your region` even when the key quota was not exhausted.
- Some keys worked in OpenClaw but appeared as `cooldown` in the gateway.
- Groq could show forbidden/unavailable when it needed a proxy path.

Analysis:

- Admin `Test` and key `Validate` used the configured health-check model first, even when that model was not actually present in the selected key's discovered provider inventory.
- Region/model forbidden errors are about the model/provider path, not proof that the key is invalid or exhausted.
- Model discovery errors were incorrectly reported through the key failure state machine, so repeated `/models` decode failures could cool down a usable key.
- Groq/proxy failures can be network/WAF path problems, not key problems.

Improvement:

- Admin `Test` and `Validate` now choose test models from the selected key's real discovered provider inventory before falling back to `health_check_model`.
- Region/model forbidden `403` is skipped as a failed model candidate and does not mark the key bad.
- WAF/Cloudflare-style `403` and model/region `403` no longer increment key failure counters or send keys to cooldown.
- Model discovery failures no longer reserve or penalize keys; they only update model discovery error/backoff state.
- Providers now support optional `proxy_url`, useful for Groq or other providers that require a specific outbound proxy.
- Watcher model discovery now follows the same rule: `/models` failures are inventory/probe errors, not key availability failures.
- Admin `Test`/`Validate` now treat upstream `5xx`, timeouts, decode errors, WAF blocks, and model/region mismatches as `inconclusive`; they return diagnostics without changing key state.
- Failed `Test`/`Validate` responses include `validation_status` and `key_state_updated`.

Tests added/updated:

- `region_unavailable_403_is_model_candidate_mismatch`
- `validation_candidates_are_deduped_and_skip_disabled_models`
- `test_keyhub_can_return_models_for_validation_key`
- `test_gateway_waf_403_does_not_penalize_key_state`
- `test_gateway_region_403_does_not_penalize_key_state`
- `test_gateway_auth_403_still_disables_key`
- `test_model_discovery_failure_does_not_cooldown_key`
- `model_discovery_error_does_not_penalize_key_availability`
- `validation_transient_errors_are_inconclusive_not_key_state_updates`
- `validation_confirmed_auth_or_rate_limit_updates_key_state`

## 2026-07-02: Probe Budget, Provider Cooldown Profiles, And RTK Compression

Problem:

- Manual and automatic Test/Validate probes were easy to discuss as diagnostics, but every real chat probe also consumes upstream request budget.
- Long-running reliability needs provider-specific cooldown interpretation instead of one generic key timer.
- Large tool/log outputs can waste upstream tokens when routed to remote models.

Changes:

- Clarified that Test, Validate, half-open probes, and background probes all affect the same key/provider quota budget as user calls.
- Added provider cooldown profiles for OpenRouter, NVIDIA, OpenCode, and future Google Gemini free API support.
- Added optional `context_compression` configuration using the local RTK executable at `G:\ai\AgentsTools\rtk.exe`.
- Added router-side RTK compression before provider candidate attempts. It only compresses large `role=tool` messages, preserves ordinary user/system/assistant messages, times out quickly, and keeps the original content if RTK fails or does not shrink output.
- Added tests for context compression config parsing and large tool-message compression behavior.

Impact:

- Probe traffic is now treated as real quota-affecting traffic in the design rules.
- Future Google free API support has a clear project/account quota-pool requirement rather than naive per-key rotation.
- Long tool outputs can be locally compressed without changing normal chat semantics.

## 2026-07-02: Attempt Log Analysis For Providers Without Auth APIs

Problem:

- Some providers do not expose usable auth/account/quota APIs.
- Without those APIs, the gateway still needs a way to infer which provider,
  model, and key fingerprint is currently hurting reliability.

Changes:

- Added `GET /admin/metadata/attempts/analyze?limit=100` for local rule-based
  analysis of recent routing attempts.
- Added optional model-assisted analysis:
  `GET /admin/metadata/attempts/analyze?limit=100&use_model=true&model=chat`.
- Model-assisted analysis is explicit because it is a real routed chat request
  and consumes the same key/provider quota budget as user traffic.
- The model prompt uses stable key fingerprints only and never raw key values.
- The web admin Health page now shows local routing analysis and recommendations
  without triggering a model call.

Impact:

- Providers without quota APIs can still be scheduled from observed behavior:
  success, 429, auth failure, region/model restriction, upstream error, timeout,
  malformed stream, and fallback patterns.
- Operators can ask a configured model to analyze the same attempts when deeper
  diagnosis is worth spending one request.

Follow-up design:

- See `docs/reliable-key-routing-research.md` for the longer-term routing and key availability design based on LiteLLM, Portkey, and FreeLLMAPI patterns.

## 2026-07-02: GitHub Publish Fallback When HTTPS Push Is Reset

Problem:

- `git push origin master` repeatedly failed after upload with:
  `RPC failed; curl 56 Recv failure: Connection was reset`.
- `git push --porcelain --progress` showed the pack upload completed
  successfully, then the connection reset while waiting for the receive-pack
  response.
- `git ls-remote origin refs/heads/master` confirmed the remote ref had not
  moved despite Git printing `Everything up-to-date`.
- SSH port 22 timed out, SSH-over-443 also did not complete in time.

Diagnosis:

- The failing HTTPS connection presented a local `scholar.verify` certificate
  chain and reset the `git-receive-pack` POST after the upload finished.
- The object pack was small, so this was not a repository size problem.
- Switching Git HTTP version, postBuffer, or SSL backend did not fix it.

Successful recovery:

- Use GitHub's Git Database API with the existing Git Credential Manager token.
- Do not print or store the token; obtain it through `git credential fill` and
  keep it only in memory for the publishing script.
- Recreate each local commit missing from the remote in order:
  1. Read the current remote `master` SHA with `git ls-remote`.
  2. List missing commits with `git rev-list --reverse remote..HEAD`.
  3. For each commit, upload changed blobs with `POST /git/blobs`.
  4. Create a tree with `POST /git/trees` using the previous remote tree as
     `base_tree`.
  5. Create the commit with `POST /git/commits`, preserving message, author,
     committer, date, parent, and tree.
  6. Move `refs/heads/master` with `PATCH /git/refs/heads/master` and
     `force=false`.
  7. Run `git fetch origin master` and `git status --short --branch` to verify
     local and remote are synchronized.

Important details:

- Prefer this only as a fallback after ordinary `git push` fails and
  `ls-remote` proves the remote ref did not move.
- If the recreated commit SHA matches the local commit SHA, the tree, parent,
  author, committer, date, and message were preserved correctly.
- PowerShell pitfalls from this run:
  - Older Windows PowerShell does not support `&&`.
  - Some .NET versions do not expose `ProcessStartInfo.ArgumentList`; use
    `Arguments` or another compatible byte-safe subprocess wrapper.
  - Avoid text encoding when reading blobs. Read `git show <commit>:<path>` as
    raw bytes before base64 encoding.
  - In PowerShell strings, write `${Spec}:` instead of `$Spec:` to avoid drive
    syntax parsing.
