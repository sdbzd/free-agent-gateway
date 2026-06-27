# Key-Level Model Capability Routing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route chat requests only through free keys that independently advertise the exact requested model.

**Architecture:** Replace provider-wide key strings with a backward-compatible key configuration object carrying an explicit cost tier. Extend KeyHub state with per-key model inventories and make Router build exact-model candidate lists across providers and keys. Keep provider implementations protocol-oriented, refresh capability inventories through `/models`, and expose only free-key models from `/v1/models`.

**Tech Stack:** Rust 2024, Serde untagged enums, Axum, Tokio, Reqwest, DashMap, existing integration tests.

---

### Task 1: Parse key tiers and optional model configuration

**Files:**
- Modify: `src/config.rs`
- Modify: `src/providers/mod.rs`
- Modify: provider constructors and tests
- Test: `tests/config_state_health_tests.rs`
- Test: `tests/provider_tests.rs`

- [ ] Add failing tests for object-form keys, legacy unknown keys, omitted `models`, omitted `health_check_model`, and legacy provider type aliases.
- [ ] Run focused configuration tests and confirm compilation/assertion failures.
- [ ] Add `KeyTier` and untagged `KeyConfig`, make top-level `models` default empty, and make `health_check_model` optional.
- [ ] Canonicalize `github_models` and `nvidia` as Serde aliases of `openai_compatible`.
- [ ] Update provider construction to receive normalized key values without vendor-specific routing assumptions.
- [ ] Run configuration and provider tests.

### Task 2: Store independent model capability per key

**Files:**
- Modify: `src/models/mod.rs`
- Modify: `src/keyhub/mod.rs`
- Modify: `src/state/mod.rs`
- Test: `tests/keyhub_tests.rs`
- Test: `tests/config_state_health_tests.rs`

- [ ] Add failing tests proving two keys under one provider can retain different model lists and tiers.
- [ ] Add failing persistence tests proving model inventories and tiers restore by fingerprint without raw keys.
- [ ] Extend `KeyState` with tier, advertised models, discovery timestamp, and discovery error.
- [ ] Add KeyHub APIs to enumerate discovery targets, update per-key model inventory, and inspect exact free candidates.
- [ ] Preserve cooldown and failure behavior while restoring capability metadata.
- [ ] Run focused KeyHub and persistence tests.

### Task 3: Route exact models through free keys only

**Files:**
- Modify: `src/router/mod.rs`
- Modify: `src/error.rs`
- Modify: `src/providers/traits.rs`
- Test: `tests/router_tests.rs`

- [ ] Add failing tests where paid and unknown keys advertise the requested model but a free key is selected.
- [ ] Add a failing test where only a paid key supports the model and routing returns model unavailable without contacting it.
- [ ] Add failing same-model fallback tests across keys and providers, asserting the model ID never changes.
- [ ] Replace provider-wide acquisition with candidate selection by canonical model and `tier: free`.
- [ ] When no cached free candidate exists, refresh free-key inventories once through `list_models` and retry selection.
- [ ] Remove health-check-model fallback substitution.
- [ ] Run router tests.

### Task 4: Discover every key and expose only free models

**Files:**
- Modify: `src/watcher/mod.rs`
- Modify: `src/api/models.rs`
- Modify: `src/api/status.rs`
- Modify: `src/main.rs`
- Test: add focused unit/integration tests in existing test modules

- [ ] Add failing tests for per-key discovery and `/v1/models` exclusion of paid-only/unknown-only models.
- [ ] Make watcher query every configured key independently and aggregate provider health.
- [ ] Make `/v1/models` refresh inventories and return aliases/canonical IDs only when an available free key advertises them.
- [ ] Add free-eligible key counts to diagnostics without exposing fingerprints publicly.
- [ ] Update startup and persistence wiring for `KeyConfig`.
- [ ] Run endpoint, watcher, and status tests.

### Task 5: Documentation, migration, and verification

**Files:**
- Modify: `CONFIG.md`
- Modify: `README.md`
- Do not modify or stage the user's tracked `config.yaml`

- [ ] Document object-form keys and fail-closed legacy behavior.
- [ ] Document `type` as protocol adapter and `models`/`health_check_model` as optional.
- [ ] Run `cargo fmt --all`.
- [ ] Run `cargo test --all-targets`.
- [ ] Run `cargo clippy --all-targets -- -D warnings`.
- [ ] Run `cargo fmt --all -- --check`.
- [ ] Confirm no paid/unknown automatic routing and no model substitution.
- [ ] Commit code, tests, and docs with a Lore commit while leaving `.gitignore` and `config.yaml` unstaged.
