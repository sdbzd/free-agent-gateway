# OpenAI API Reliability Design

## Goal

Improve the reliability and diagnosability of the gateway's existing OpenAI-compatible endpoints without expanding the public API surface.

The supported scope is:

- `POST /v1/chat/completions`
- `GET /v1/models`
- Streaming and non-streaming chat completions
- OpenAI-compatible JSON error responses

The following are explicitly out of scope:

- Ollama-specific fixes
- `/v1/embeddings`
- New provider types
- Large-scale router or provider abstraction rewrites

## Compatibility

Existing request and successful response formats remain unchanged. Existing configuration keys remain valid. Internal changes may add structured log fields and improve status accounting, but they must not require client changes.

Error responses use the OpenAI-compatible envelope:

```json
{
  "error": {
    "message": "human-readable message",
    "type": "gateway_error_type",
    "param": null,
    "code": "stable_error_code"
  }
}
```

The gateway must not expose upstream credentials, authorization headers, raw API keys, or sensitive request headers in either HTTP responses or logs.

## Key Lifecycle

`KeyPool` owns all key-state transitions. Availability checks must perform expiration recovery before deciding whether a key is available.

Both temporary states recover when `cooldown_until` expires:

- `Cooldown` becomes `Available`
- `RateLimited` becomes `Available`

Recovery clears `cooldown_until` and resets the consecutive `fail_count`. `Disabled` remains permanent for the lifetime of the configured key unless configuration changes.

Router code must not perform a stale availability precheck that prevents `acquire_key()` from running recovery logic.

## State Persistence

Persisted state stores operational metadata, not credentials. Key states are matched back to configured keys by a stable, non-reversible key identifier derived from the real key.

The persisted representation contains:

- Key identifier
- Status
- Consecutive failure count
- Cooldown expiry
- Success count
- Total failure count

At startup, each configured provider is registered using its configured keys, then matching persisted metadata is restored into its `KeyPool`. Unknown persisted keys are ignored. Newly configured keys start as available. Removed keys are not recreated from persisted state.

Expired temporary states are normalized to `Available` during restoration.

## Request Routing and Failure Accounting

For non-streaming requests:

- Each provider attempt logs its start and result.
- Upstream HTTP and transport failures update the selected key.
- The next configured provider is attempted when the current provider fails.
- The gateway reports `AllProvidersFailed` only after all eligible providers are exhausted.

For streaming requests:

- Failure before a successful upstream response is returned may trigger provider fallback.
- Once a provider has returned a successful streaming response, the gateway cannot safely replay after bytes may have reached the client.
- The key is not marked successful merely because upstream response headers were accepted.
- Stream completion marks the key successful.
- A stream body error marks the key failed and increments the gateway error counter.
- A stream body error is emitted to the client as an OpenAI-compatible SSE error event, then the stream terminates.

This design does not attempt cross-provider fallback after streaming output has begun because replay could duplicate partial assistant output.

## Structured Diagnostic Logging

Every chat request receives or reuses an `X-Request-Id`. Structured logs correlate the complete request lifecycle using that identifier.

Provider-attempt logs include:

- `request_id`
- `provider`
- Resolved upstream `model`
- Masked key identifier
- `attempt`
- `stream`
- `stage`
- `elapsed_ms`
- HTTP status when available
- Error category
- Sanitized error message
- Whether another fallback will be attempted

Stages use stable names:

- `route_resolution`
- `key_acquisition`
- `upstream_connect`
- `upstream_response`
- `stream_body`
- `response_serialization`

The API handler logs final request outcome and total elapsed time. Provider implementations continue returning typed errors; logging ownership stays primarily in the router and API boundary to avoid duplicate exception records.

Sensitive values are sanitized before logging. Keys are represented only by the existing masked form or stable identifier.

## OpenAI-Compatible Errors

Gateway errors map to stable HTTP statuses and OpenAI-style codes:

- Invalid input: `400`
- Authentication failure from all attempted routes: gateway-level failure without exposing upstream credentials
- Unknown model: `404`
- Rate limiting after fallback exhaustion: `429` when all failures are rate limits
- Upstream failure after fallback exhaustion: `502` or `503`
- Timeout after fallback exhaustion: `504`

When multiple providers fail, the client receives a safe summary. Full attempt details remain in structured logs under the request ID.

SSE errors use:

```text
data: {"error":{"message":"...","type":"upstream_error","param":null,"code":"stream_error"}}

data: [DONE]
```

## `/v1/models`

Configured aliases are always returned.

Automatic discovery:

- Attempts only registered providers with an available or recoverable key.
- Logs provider discovery failures with provider and stage fields.
- Does not fail the entire endpoint because one provider is unavailable.
- Does not disable or penalize a key solely because optional model discovery failed, unless the failure clearly indicates authentication or rate limiting.
- Avoids duplicate model IDs.

## Configuration Enforcement

This change makes existing relevant settings effective:

- `server.request_timeout` applies to the full non-streaming request workflow.
- `server.sse_keepalive` produces SSE keepalive comments while a stream is idle.
- `cors.allowed_methods` and `cors.allowed_headers` are honored rather than replaced with unrestricted values.

Provider-specific request timeouts remain the upper bound for individual upstream calls. The smaller applicable timeout wins.

Provider priority and model-cache behavior are not changed in this reliability pass because the current fallback order is explicitly configured and model caching is not required for correctness.

## Testing

Regression tests cover:

- `RateLimited` recovery after expiry.
- `Cooldown` recovery through normal Router acquisition.
- Disabled keys do not recover.
- Persisted metadata restores onto matching configured keys.
- Unknown and removed persisted keys are ignored.
- Expired persisted cooldowns normalize to available.
- Non-stream fallback records failure and succeeds through the next provider.
- Streaming success is recorded only after stream completion.
- Streaming body failure records a key failure and increments error accounting.
- Error responses contain OpenAI-compatible fields.
- Request IDs and diagnostic fields are present without raw keys.
- `/v1/models` remains available when one provider's discovery fails.
- Configured request timeout, SSE keepalive, and CORS restrictions are effective.

All existing tests must continue passing. Final verification includes:

```text
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```

## Stop Condition

The reliability pass is complete when the existing two OpenAI-compatible endpoints preserve their public success formats, the listed lifecycle and diagnostic regressions are covered by tests, and formatting, tests, and Clippy all pass without warnings.
