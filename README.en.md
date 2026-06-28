# free-agent-gateway

An OpenAI-compatible API gateway for personal agent workflows. It currently targets OpenClaw and Hermes, with room to add more free OpenAI-style API providers over time.

## Highlights

- Multi-provider, multi-key automatic rotation.
- Free keys first; paid keys are used only as a last-resort fallback.
- Automatic OpenRouter quota detection for normal free keys and topped-up high-quota keys.
- Separate handling for 429, 5xx, Cloudflare/WAF blocks, and authentication failures.
- Manual restore flow for 401/403-disabled keys.
- Automatic request and token usage accounting.
- Admin UI for per-key, per-provider, and per-model availability and remaining quota.
- Cloudflare Workers AI model discovery through the official `models/search` endpoint.
- Linux release build script and GitHub Actions artifact.

## Safety

Do not commit real `config.yaml`, `state.json`, `state.db`, logs, monitor data, or local temporary files. Publish only `config.yaml.sample`.

## Quick Start

```bash
cp config.yaml.sample config.yaml
cargo run
```

Open:

```text
http://127.0.0.1:9000/admin
http://127.0.0.1:9000/v1/models
```

## Linux Build

```bash
chmod +x scripts/build-linux.sh
./scripts/build-linux.sh
```

Output:

```text
dist/free-agent-gateway-linux-<arch>.tar.gz
```
