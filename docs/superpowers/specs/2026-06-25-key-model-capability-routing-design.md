# Key-Level Model Capability Routing Design

## Goal

Route an OpenAI-compatible model request only to a currently usable free key that explicitly advertises the requested model.

The gateway must not:

- Assume every key under one provider has the same model access.
- Substitute a provider health-check model during fallback.
- Automatically use paid or unknown-cost keys.
- Silently replace the requested model with a different model.

## Configuration

Provider `type` describes the wire protocol adapter, not the vendor.

Supported canonical values:

- `openai_compatible`
- `ollama`

For backward compatibility, `github_models` and `nvidia` deserialize as aliases of `openai_compatible`.

Keys support an explicit object form:

```yaml
providers:
  example:
    type: openai_compatible
    base_url: https://example.com/v1
    keys:
      - value: "${FREE_KEY}"
        tier: free
      - value: "${PAID_KEY}"
        tier: paid
```

Supported tiers:

- `free`
- `paid`
- `unknown`

Legacy string keys remain parseable:

```yaml
keys:
  - "${LEGACY_KEY}"
```

A legacy string key is assigned `tier: unknown`. Unknown and paid keys are never eligible for automatic chat routing.

The top-level `models` map remains optional. It is only an alias map from a friendly request name to a canonical upstream model name. A configured provider is a preference, not permission to bypass key capability or cost rules.

`health_check_model` remains accepted for configuration compatibility but becomes optional and is not used for routing or health decisions.

## Key Capability State

Each configured key owns independent runtime metadata:

- Stable key fingerprint
- Cost tier
- Availability status
- Failure and success counters
- Cooldown expiry
- Advertised model IDs
- Last successful model-discovery timestamp
- Last model-discovery error

Raw key values are never serialized.

Capability metadata is persisted with existing key state. On restart it is restored only when the fingerprint matches a currently configured key.

## Model Discovery

For OpenAI-compatible providers, model capability is discovered by calling `GET /models` separately with every configured key.

Discovery behavior:

- Free, paid, and unknown keys may be queried for inventory.
- Discovery itself does not make paid or unknown keys route-eligible.
- A successful response replaces that key's advertised model set.
- Authentication and rate-limit failures update only that key's state.
- Other discovery failures retain the previous cached model set and record a diagnostic error.
- Models are deduplicated per key.

The watcher performs discovery for every configured key. The `/v1/models` endpoint may refresh inventories, and a chat request may perform one bounded refresh when no eligible cached candidate exists.

## Routing

For a request model:

1. Resolve a friendly alias to its canonical model ID, if configured.
2. Find every configured key whose advertised model set contains that exact canonical ID.
3. Keep only keys with `tier: free`.
4. Recover expired cooldown/rate-limit states.
5. Keep only currently available keys.
6. Rank candidates by:
   - Preferred alias provider, when configured.
   - Provider order in the configured fallback list.
   - Lowest consecutive failure count.
   - Existing configured key order.
7. Attempt candidates in order.

If no cached candidate exists, refresh free-key model inventories once and repeat selection.

If no eligible free key supports the exact model, return a safe model-unavailable error. Paid and unknown keys remain unused even when they support the model.

Provider fallback no longer changes the model ID. Fallback means trying another free key or provider that advertises the same model.

## `/v1/models`

The endpoint exposes:

- Configured aliases whose canonical model currently has at least one eligible free key.
- Canonical model IDs advertised by at least one currently available free key.

Models available only through paid or unknown keys are omitted.

The response remains OpenAI-compatible and does not expose key fingerprints, tiers, or raw credentials.

## Health and Diagnostics

Provider health is aggregated from per-key discovery:

- `healthy`: at least one key completed model discovery successfully.
- `unhealthy`: no key completed discovery successfully.
- `available_keys`: exact number of available keys, regardless of tier.
- Additional metrics distinguish free route-eligible keys from total keys.

Structured logs for discovery and routing include:

- Request ID when applicable
- Provider
- Masked key/fingerprint
- Tier
- Canonical model
- Discovery or routing stage
- Error category and HTTP status
- Whether the key was rejected for tier, capability, or availability

No raw key value is logged.

## Compatibility and Migration

Existing configurations continue to parse.

However, existing string keys become `unknown` and therefore cannot serve chat requests until explicitly migrated to object form with `tier: free`. This fail-closed behavior prevents accidental paid usage.

Existing `models` and `health_check_model` sections are optional. Omitting `models` means clients request canonical model IDs directly.

## Testing

Regression coverage includes:

- Object-form key parsing for all tiers.
- Legacy string keys default to unknown.
- Old provider type names map to `openai_compatible`.
- `models` and `health_check_model` can be omitted.
- Different keys under one provider retain different model sets.
- Paid and unknown keys are never selected.
- A free key supporting the exact model is selected.
- A free key not advertising the model is skipped.
- Same-model fallback works across keys and providers without changing model ID.
- No free candidate returns model unavailable even when paid candidates exist.
- `/v1/models` excludes paid-only and unknown-only models.
- Capability state persists without raw credentials.
- Existing key cooldown, stream accounting, and OpenAI error tests remain green.

## Stop Condition

The change is complete when all chat routing decisions are based on exact per-key model capability and explicit free tier, paid/unknown keys cannot be used automatically, existing endpoints retain OpenAI-compatible formats, and formatting, tests, and Clippy pass.
