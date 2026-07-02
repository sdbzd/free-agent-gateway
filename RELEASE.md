# Release Checklist

## Preflight

- Update `Cargo.toml` version.
- Review `README.md`, `ARCHITECTURE.md`, `ROADMAP.md`, and `config.yaml.sample`.
- Confirm `config.yaml.sample` contains no real credentials and starts safely with placeholder remote providers disabled.
- Run formatting, tests, linting, and release build:

```powershell
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
```

## Smoke Test

```powershell
Copy-Item config.yaml.sample config.yaml
.\target\release\free-agent-gateway.exe
```

Then open:

- `http://127.0.0.1:9000/health`
- `http://127.0.0.1:9000/admin`
- `http://127.0.0.1:9000/metrics/prometheus`

## Notes

- OpenAI-compatible providers such as OpenRouter, Cerebras, and OpenCode use `type: "openai_compatible"`.
- Keep topped-up free-pool keys as `tier: free` and set explicit `rpm_limit` / `rpd_limit`.
- Use `tier: paid` only for last-resort paid escalation.
