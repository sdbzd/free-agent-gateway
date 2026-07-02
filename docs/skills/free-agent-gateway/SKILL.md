---
name: free-agent-gateway
description: Use this skill when an AI agent needs to call a local free-agent-gateway OpenAI-compatible proxy, choose model routing, inspect provider-prefixed routes, handle gateway errors, or explain how to configure tools such as Codex, OpenCode, OpenClaw, Hermes, or OpenAI SDK clients to use the gateway.
---

# free-agent-gateway

Use the local gateway as the single OpenAI-compatible endpoint for model calls:

```text
base_url: http://127.0.0.1:9000/v1
api_key: any non-empty string
model: auto
```

Prefer `model: auto` unless the user asks for a specific model or provider. The gateway decides among providers, keys, and models using availability, tier, routing groups, recent failures, learned capability observations, and token/rate pressure.

## Provider Routes

Use the normal route for automatic routing:

```text
GET  http://127.0.0.1:9000/v1/models
POST http://127.0.0.1:9000/v1/chat/completions
```

Use provider-prefixed routes when the user wants to force a provider:

```text
GET  http://127.0.0.1:9000/{provider}/v1/models
POST http://127.0.0.1:9000/{provider}/v1/chat/completions
```

Examples:

```text
http://127.0.0.1:9000/openrouter/v1/chat/completions
http://127.0.0.1:9000/groq/v1/chat/completions
http://127.0.0.1:9000/cerebras/v1/chat/completions
http://127.0.0.1:9000/huggingface/v1/chat/completions
```

## Request Guidance

When the client supports headers, include the agent name:

```http
X-Agent-Name: codex
```

For tool-using agents, preserve the `tools`, `tool_choice`, and streaming options exactly as the client needs. Do not strip tool calls. The gateway supports OpenAI-compatible chat, completions, responses, and streaming chat completions.

If a task needs a specific provider, use provider-prefixed routes instead of changing the model name. If a task needs a specific model, use the exact model id returned by `/v1/models` or `/{provider}/v1/models`.

## Error Handling

Do not assume every 403 means the key is invalid. Region errors, model availability errors, WAF blocks, and auth failures are different states.

Do not assume every 404 means the model is globally missing. It may be absent from the selected provider or absent from the selected key inventory.

For 429, let the gateway cool down the key. Avoid immediately retrying the same provider/key loop from the client.

If a stream starts and then fails, treat the response as failed unless the caller has already received a complete final answer. The gateway records stream-body failures separately from connection failures.

If the response is empty or malformed, retry through the gateway rather than bypassing it. The gateway can fail over to another key/provider when the model is available elsewhere.

## Admin Checks

Use the browser admin page for human inspection:

```text
http://127.0.0.1:9000/admin
```

Useful machine-readable endpoints:

```text
GET http://127.0.0.1:9000/health
GET http://127.0.0.1:9000/status
GET http://127.0.0.1:9000/metrics
GET http://127.0.0.1:9000/metrics/prometheus
```

## Token Accounting

Token usage should include both input and output tokens. Prefer provider-reported usage when present. Treat local estimates as estimates, not exact billing values. When reporting usage, distinguish reported totals from estimated totals when that distinction is available.

## Safety

Never ask the user to paste real keys into prompts. Keys belong in local environment variables or local `config.yaml`.

Never commit local runtime files such as `config.yaml`, `.env`, `state.db`, `models.cache`, logs, build output, or temporary backups.
