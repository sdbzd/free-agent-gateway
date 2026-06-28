# free-agent-gateway

`free-agent-gateway` 是一个面向个人 Agent 工作流的 OpenAI 兼容网关。它统一管理多个上游 Provider 和多个 API Key，自动做模型路由、免费 key 轮换、速率限制、失败退避、用量统计和健康检查。

## 核心能力

- OpenAI Chat Completions 兼容接口：`/v1/chat/completions`
- 模型列表接口：`/v1/models`
- 多 provider / 多 key 自动轮换
- 同名模型可跨 provider 一起参与候选池
- 每个 key 独立统计请求、token、错误、冷却和恢复时间
- 支持 RPM / RPD / TPM / TPD 本地限速
- 429 支持 `Retry-After`，缺失时使用本地退避策略
- 401 / 403 自动禁用，并支持后台手工恢复
- Prometheus 指标：`/metrics/prometheus`
- 管理后台：`/admin`

## Provider

内置支持：

- GitHub Models
- NVIDIA
- Ollama
- OpenAI-compatible provider

OpenRouter、Cerebras、OpenCode 等 OpenAI 兼容服务都使用：

```yaml
type: "openai_compatible"
base_url: "https://api.example.com/v1"
```

## 快速开始

复制样例配置：

```powershell
Copy-Item config.yaml.sample config.yaml
```

编辑 `config.yaml`，填入你的 provider 和 key。真实配置文件已被 `.gitignore` 忽略，不应提交到仓库。

构建并启动：

```powershell
cargo build --release
.\target\release\free-agent-gateway.exe
```

访问：

- `http://127.0.0.1:8080/health`
- `http://127.0.0.1:8080/admin`
- `http://127.0.0.1:8080/v1/models`

## Key 分级

推荐为 key 标注 tier：

```yaml
keys:
  - value: "${CEREBRAS_API_KEY}"
    tier: free
    rpm_limit: 30
    rpd_limit: 1000
```

默认路由只使用 `free` key。`paid` key 作为最后兜底，`unknown` key 不参与普通请求，避免意外消耗。

## 发布前检查

```powershell
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
```
 

# free-agent-gateway

`free-agent-gateway` is an OpenAI-compatible gateway for personal agent workflows. It manages multiple upstream providers and API keys with model routing, free-key rotation, local rate limiting, retry backoff, usage tracking, and health monitoring.

## Features

- OpenAI-compatible Chat Completions endpoint: `/v1/chat/completions`
- Model listing endpoint: `/v1/models`
- Automatic rotation across providers and keys
- Same model names can be pooled across different providers
- Per-key request, token, error, cooldown, and recovery tracking
- Local RPM / RPD / TPM / TPD limits
- 429 handling with `Retry-After` support and local fallback backoff
- 401 / 403 keys are disabled automatically and can be restored manually
- Prometheus metrics at `/metrics/prometheus`
- Admin dashboard at `/admin`

## Providers

Built-in provider support:

- GitHub Models
- NVIDIA
- Ollama
- OpenAI-compatible providers

OpenRouter, Cerebras, OpenCode, and similar services should use:

```yaml
type: "openai_compatible"
base_url: "https://api.example.com/v1"
```

## Quick Start

Copy the sample config:

```powershell
Copy-Item config.yaml.sample config.yaml
```

Edit `config.yaml` and add your providers and keys. The real config file is ignored by Git and should never be committed.

Build and run:

```powershell
cargo build --release
.\target\release\free-agent-gateway.exe
```

Open:

- `http://127.0.0.1:8080/health`
- `http://127.0.0.1:8080/admin`
- `http://127.0.0.1:8080/v1/models`

## Key Tiers

It is recommended to declare key tiers explicitly:

```yaml
keys:
  - value: "${CEREBRAS_API_KEY}"
    tier: free
    rpm_limit: 30
    rpd_limit: 1000
```

Normal routing uses `free` keys. `paid` keys are reserved for last-resort escalation, and `unknown` keys are excluded from normal traffic to avoid accidental spending.

## Release Checks

```powershell
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
```

 

