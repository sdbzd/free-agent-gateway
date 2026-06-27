# OpenAI API Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the existing `/v1/chat/completions` and `/v1/models` endpoints reliable under key cooldowns, restarts, provider fallback, and streaming failures while adding safe structured diagnostics.

**Architecture:** Keep the existing provider trait and public HTTP success formats. Centralize temporary-key recovery and persisted-state restoration in `KeyPool`, wrap successful upstream streams so final success/failure is recorded when the body actually terminates, and enrich router/API-boundary logs with request and attempt context. Apply existing timeout, SSE keepalive, and CORS configuration without adding new endpoints or Ollama-specific parsing changes.

**Tech Stack:** Rust 2024, Axum 0.8, Tokio, Reqwest, Futures, Tower HTTP, Serde, existing integration-test structure.

---

### Task 1: Recover temporary key states consistently

**Files:**
- Modify: `src/keyhub/mod.rs`
- Test: `tests/keyhub_tests.rs`

- [ ] **Step 1: Write failing recovery tests**

Add tests that construct keys with expired `RateLimited` and `Cooldown` metadata through a restoration API, assert `has_available_keys()` becomes true, and assert `Disabled` remains unavailable.

- [ ] **Step 2: Verify the tests fail**

Run:

```text
cargo test --test keyhub_tests recovery -- --nocapture
```

Expected: failure because no restoration API exists and `has_available_keys()` does not normalize expired states.

- [ ] **Step 3: Add key fingerprint and restoration behavior**

Add:

```rust
pub fn key_fingerprint(key: &str) -> String
```

using a deterministic standard-library hash, plus:

```rust
pub fn restore_states(&self, persisted: &[KeyState])
```

Match configured keys by fingerprint, copy operational counters/status, normalize expired `Cooldown` and `RateLimited` states, and never recover `Disabled`.

- [ ] **Step 4: Make all availability paths normalize temporary states**

Extract a private `recover_expired(&mut [KeyState])` helper and call it from both `acquire_key()` and `available_count()`. Change `available_count()` to take a write lock because recovery mutates state.

- [ ] **Step 5: Verify focused and existing key tests**

Run:

```text
cargo test --test keyhub_tests
```

Expected: all keyhub integration tests pass.

### Task 2: Restore persisted state onto configured keys

**Files:**
- Modify: `src/models/mod.rs`
- Modify: `src/keyhub/mod.rs`
- Modify: `src/main.rs`
- Test: `tests/config_state_health_tests.rs`

- [ ] **Step 1: Write failing persistence tests**

Add round-trip tests proving persisted key metadata contains a fingerprint but not a raw key, and restoration applies matching metadata while ignoring unknown fingerprints.

- [ ] **Step 2: Verify the tests fail**

Run:

```text
cargo test --test config_state_health_tests state_
```

Expected: failure because snapshots currently store masked strings that are skipped during serialization and startup never restores KeyHub.

- [ ] **Step 3: Persist stable identifiers**

Keep `KeyState.key` out of serialization and add a serialized `key_id` field with a default for backward compatibility. Populate it from `key_fingerprint()` in snapshots.

- [ ] **Step 4: Add KeyHub restoration**

Add:

```rust
pub fn restore_provider_states(&self, provider_name: &str, states: &[KeyState])
```

and call it in `main.rs` immediately after each provider's configured keys are registered.

- [ ] **Step 5: Save every registered KeyHub pool**

Build persisted state directly from `keyhub.snapshot()` instead of joining through health states, so temporary health-registry inconsistencies cannot omit a provider.

- [ ] **Step 6: Verify persistence tests**

Run:

```text
cargo test --test config_state_health_tests
```

Expected: all configuration/state/health tests pass.

### Task 3: Exercise real provider fallback and preserve the decisive error

**Files:**
- Modify: `src/router/mod.rs`
- Modify: `src/error.rs`
- Test: `tests/router_tests.rs`

- [ ] **Step 1: Write a failing non-stream fallback test**

Use two local mock HTTP servers and real `OpenAiCompatibleProvider` instances. Make the first provider return `503`, the second return a successful OpenAI chat response, then assert the Router returns the second response and the first key records a failure.

- [ ] **Step 2: Verify the test fails or exposes missing assertions**

Run:

```text
cargo test --test router_tests fallback -- --nocapture
```

Expected: the new test initially fails where current Router construction/test support is insufficient.

- [ ] **Step 3: Remove stale key prechecks**

Call `acquire_key()` directly and log `key_acquisition` failures. This allows key recovery to happen before the provider is skipped.

- [ ] **Step 4: Track failure categories across attempts**

Record whether all provider failures were rate limits, timeouts, authentication failures, or mixed. Return a safe typed terminal error instead of discarding every cause into an unconditional `AllProvidersFailed`.

- [ ] **Step 5: Make JSON errors OpenAI-compatible**

Change the error envelope to always contain:

```json
{
  "error": {
    "message": "...",
    "type": "...",
    "param": null,
    "code": "..."
  }
}
```

- [ ] **Step 6: Verify router and error tests**

Run:

```text
cargo test --test router_tests
```

Expected: all router tests pass.

### Task 4: Record streaming success only after body completion

**Files:**
- Modify: `src/router/mod.rs`
- Modify: `src/api/chat.rs`
- Modify: `src/lib.rs`
- Test: `tests/router_tests.rs`

- [ ] **Step 1: Write failing stream lifecycle tests**

Add test providers or local mock streams that:

- yield chunks and terminate normally;
- yield a chunk then return a stream-body error.

Assert normal termination increments key success count, while body error increments key failure count and does not increment success.

- [ ] **Step 2: Verify the tests fail**

Run:

```text
cargo test --test router_tests stream_ -- --nocapture
```

Expected: failure because Router currently reports success immediately after receiving response headers.

- [ ] **Step 3: Wrap the upstream stream**

Use `futures::stream::unfold` to wrap the selected provider stream with provider/key context. On `None`, call `report_success`; on `Err`, call `report_failure` once and forward the error.

- [ ] **Step 4: Count asynchronous stream errors**

Clone `error_counter` into the API streaming task. Increment it when the wrapped stream returns an error or returns no data. Emit an OpenAI-compatible SSE error event followed by `[DONE]`.

- [ ] **Step 5: Add SSE keepalive**

Drive the stream through `tokio::time::timeout` using `server.sse_keepalive`; when idle, emit:

```text
: keep-alive

```

without treating it as model output.

- [ ] **Step 6: Verify stream tests**

Run:

```text
cargo test --test router_tests stream_
```

Expected: all stream lifecycle tests pass.

### Task 5: Add request-correlated structured diagnostics

**Files:**
- Modify: `src/router/mod.rs`
- Modify: `src/api/chat.rs`
- Modify: `src/api/models.rs`
- Modify: `src/error.rs`
- Test: `tests/router_tests.rs`

- [ ] **Step 1: Add request context to Router APIs**

Pass `request_id: &str` into Router chat methods. Log each attempt with stable fields:

```text
request_id, provider, model, key, attempt, stream, stage, elapsed_ms,
http_status, error_category, fallback
```

- [ ] **Step 2: Sanitize diagnostic values**

Keep raw keys out of fields and error messages. Use masked key output and a centralized sanitizer for bearer tokens, `key=`, `token=`, and configured key values.

- [ ] **Step 3: Add final request logs**

At the API boundary, log completion/failure and total elapsed time. Distinguish synchronous setup errors from stream-body errors.

- [ ] **Step 4: Add model-discovery diagnostics**

Log provider discovery failures using `stage = "model_discovery"` while continuing to return configured aliases and models from healthy providers.

- [ ] **Step 5: Verify request-path tests**

Run:

```text
cargo test --all-targets
```

Expected: all tests pass without exposing raw test keys in emitted diagnostic messages.

### Task 6: Enforce existing request timeout and CORS settings

**Files:**
- Modify: `src/api/chat.rs`
- Modify: `src/main.rs`
- Test: add unit tests in `src/main.rs` and request timeout tests in the closest existing test module

- [ ] **Step 1: Write failing timeout and CORS tests**

Assert a deliberately slow non-stream route returns a timeout error under `server.request_timeout`. Assert configured CORS methods and headers are parsed instead of replaced with `Any`.

- [ ] **Step 2: Verify the tests fail**

Run the focused tests and confirm the current unrestricted/unused behavior.

- [ ] **Step 3: Apply the full request timeout**

Wrap non-stream Router execution in `tokio::time::timeout` and return `GatewayError::Timeout` when the gateway-level deadline expires.

- [ ] **Step 4: Parse CORS methods and headers**

Build `AllowMethods` and `AllowHeaders` from configured values. Invalid values must be logged and ignored; they must not silently become unrelated permissive defaults.

- [ ] **Step 5: Verify focused tests**

Run the timeout and CORS test filters and confirm they pass.

### Task 7: Normalize formatting and verify the whole project

**Files:**
- Modify: Rust source and tests only as required by `cargo fmt` and Clippy

- [ ] **Step 1: Format**

Run:

```text
cargo fmt --all
```

- [ ] **Step 2: Run the full test suite**

Run:

```text
cargo test --all-targets
```

Expected: zero failed tests.

- [ ] **Step 3: Run Clippy**

Run:

```text
cargo clippy --all-targets -- -D warnings
```

Expected: zero warnings and exit code 0.

- [ ] **Step 4: Re-run format check**

Run:

```text
cargo fmt --all -- --check
```

Expected: exit code 0.

- [ ] **Step 5: Review scope**

Confirm no Ollama NDJSON parsing changes and no new OpenAI endpoints were introduced.

