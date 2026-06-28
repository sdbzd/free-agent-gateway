# free-agent-gateway

Read this in: [中文](README.zh-CN.md) | [English](README.en.md)

## Safe key-level routing

Each API key has its own model inventory and cost tier. Configure keys as
objects and explicitly mark free credentials:

```yaml
providers:
  example:
    type: openai_compatible
    base_url: "https://example.com/v1"
    keys:
      - value: "${EXAMPLE_FREE_KEY}"
        tier: free
        rpm_limit: 20
        rpd_limit: 200
        tpm_limit: 20000
        tpd_limit: 200000
      - value: "${EXAMPLE_PAID_KEY}"
        tier: paid
```

Normal routing uses available `free` keys that advertise the exact requested
model. `paid` keys are reserved for last-resort paid escalation after free keys
are exhausted or rate-limited. `unknown` keys are never used automatically.
Legacy string keys are treated as `unknown` until migrated.

`models` aliases and `health_check_model` are optional. Provider fallback never
changes the requested model.

You do not need to know provider-side quotas up front. Set
`routing.strategy: least_rate` to balance requests across all providers and keys
by observed daily/minute usage first, then by learned headroom and failure
pressure. When a key returns 429, the gateway learns an observed RPM/RPD limit
for that key and keeps other keys from the same provider available.

For topped-up keys that should still participate in normal free-model routing,
keep `tier: free` and set the higher `rpm_limit` / `rpd_limit` if you know it.
Use `tier: paid` only for last-resort paid escalation.

> OpenClaw / Hermes 当前统一 AI 入口 — free-agent-gateway + KeyHub + Model Router + Health Watcher

一个单 EXE 部署的 AI 网关，统一管理 GitHub Models、NVIDIA NIM、OpenCode、Ollama 等多个 Provider。当前主要服务 OpenClaw 和 Hermes-Agent，后续可扩展到 OpenHuman、ZeroClaw、Coding Agent、MCP Agent 等其他 Agent。

## 特性

- 🦀 **Rust 编写** — 高性能、内存安全、单文件部署
- 🔑 **KeyHub** — 每个 Provider 支持多 Key 自动轮换
- 🔄 **自动故障切换** — 429/5xx/超时自动切换 Provider 和 Key
- 🧭 **智能路由** — 支持 RoundRobin/Random/LeastFailed/LeastRate/Priority 策略
- 🤖 **Agent 感知** — 根据 Agent 名称自动选择默认模型
- 📡 **SSE 流式输出** — 完整支持 `stream=true`
- 🏥 **健康监控** — 后台 Watcher 每 60 秒检查 Provider 健康状态
- 💾 **状态持久化** — 无数据库，使用 JSON 文件保存状态
- 🔒 **安全日志** — 自动过滤敏感信息（apikey/token/cookie）
- 📊 **管理面板** — 浏览器内建的 Admin Dashboard，实时监控 Provider/Key/Model 状态
- 💬 **Chat 测试** — 内嵌 Chat Test 页面，直接选择 Provider/Key/Model 进行消息测试
- 🔤 **流式 Token 用量追踪** — 流式响应自动解析最终 SSE chunk 提取 token 用量

## 支持的 Provider

| Provider | 类型 | 说明 |
|----------|------|------|
| `github_models` | GitHub Models | Azure 托管的 OpenAI 模型 |
| `nvidia` | NVIDIA NIM | NVIDIA NIM 推理 API |
| `openai_compatible` | OpenAI 兼容 | 任何兼容 OpenAI API 的服务 |
| `ollama` | Ollama | 本地 LLM 推理（最终 Fallback） |

OpenRouter、Cerebras、OpenCode 等 OpenAI-compatible 服务都使用
`type: "openai_compatible"`，只需要配置各自的 `base_url`。

## 快速开始

### 编译

```bash
# 需要 Rust Stable 1.85+
# Edition 2024 需要较新版本 Rust

cargo build --release
```

编译产物在 `target/release/free-agent-gateway.exe`（Windows）或 `target/release/free-agent-gateway`（Linux）。

Linux 推荐使用发布脚本，它会构建 release 二进制并打包 `config.yaml.sample`：

```bash
chmod +x scripts/build-linux.sh
./scripts/build-linux.sh
```

产物位置：

```text
dist/free-agent-gateway-linux-<arch>/free-agent-gateway
dist/free-agent-gateway-linux-<arch>.tar.gz
```

GitHub Actions 会在 Ubuntu 上自动生成 `free-agent-gateway-linux-x86_64` artifact。

### 配置

编辑 `config.yaml`，设置 Provider 的 API Key：

```yaml
providers:
  cerebras:
    type: "openai_compatible"
    enabled: true
    base_url: "https://api.cerebras.ai/v1"
    keys:
      - value: "${CEREBRAS_API_KEY}"
        tier: free
        rpm_limit: 30
        rpd_limit: 1000
    health_check_model: "llama3.1-8b"
  ollama:
    type: "ollama"
    enabled: false
    base_url: "http://localhost:11434"
    keys:
      - "ollama"
```

### 启动

```bash
# Windows
.\free-agent-gateway.exe

# Linux
./free-agent-gateway
```

启动后输出：

```
🦀 free-agent-gateway v0.1.0
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
🌐 free-agent-gateway listening on http://127.0.0.1:9000
📋 OpenAI-compatible API:  http://127.0.0.1:9000/v1
🔧 Management API:          http://127.0.0.1:9000/health
📊 Metrics:                 http://127.0.0.1:9000/metrics
📈 Prometheus:              http://127.0.0.1:9000/metrics/prometheus
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

## Windows 部署

### 前置条件

1. 安装 [Rust](https://rustup.rs/)（Stable channel）
2. 确认版本 `rustc --version` >= 1.85

### 构建与部署

```powershell
# 克隆项目
git clone <repo-url>
cd free-agent-gateway

# 编译 Release 版本
cargo build --release

# 复制产物和配置
Copy-Item target\release\free-agent-gateway.exe .
Copy-Item config.yaml .

# 设置环境变量
$env:GITHUB_TOKEN_1 = "ghp_xxxxxxxxxxxx"
$env:GITHUB_TOKEN_2 = "ghp_yyyyyyyyyyyy"
$env:NVIDIA_API_KEY_1 = "nvapi-xxxxxxxxxxxx"
$env:CEREBRAS_API_KEY = "csk-xxxxxxxxxxxx"

# 启动
.\free-agent-gateway.exe
```

## Linux 部署

```bash
# 安装 Rust（如果尚未安装）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# 编译
git clone <repo-url>
cd free-agent-gateway
chmod +x scripts/build-linux.sh
./scripts/build-linux.sh

# 部署
sudo mkdir -p /etc/free-agent-gateway
sudo cp dist/free-agent-gateway-linux-*/free-agent-gateway /usr/local/bin/
sudo cp config.yaml.sample /etc/free-agent-gateway/config.yaml

# 设置环境变量
export GITHUB_TOKEN_1="ghp_xxxxxxxxxxxx"
export NVIDIA_API_KEY_1="nvapi-xxxxxxxxxxxx"
export CEREBRAS_API_KEY="csk-xxxxxxxxxxxx"

# 启动
free-agent-gateway
```

运行后产生的 `config.yaml`、`state.json`、`state.db`、`models.cache`、`*.db`、`dist/` 和日志文件都属于本地运行态数据，不应提交到 Git。

## OpenClaw 接入

OpenClaw 只需将 API 地址指向 Gateway：

```yaml
# OpenClaw 配置
api:
  base_url: "http://127.0.0.1:9000/v1"
  # 使用模型别名（gateway 自动路由到对应 Provider）
  model: "chat"  # → nvidia / meta/llama-3.1-70b-instruct
```

也可以在请求头中指定 Agent 名称：

```http
POST /v1/chat/completions
X-Agent-Name: openclaw
```

Gateway 会根据 Agent 名称自动选择对应的默认模型。

## Hermes Agent 接入

```yaml
# Hermes-Agent 配置
llm:
  endpoint: "http://127.0.0.1:9000/v1"
  model: "coding"  # → github / openai/gpt-4.1-mini
```

## Curl 测试

### 查看模型列表

```bash
curl http://127.0.0.1:9000/v1/models
```

### 模型别名请求

```bash
# 使用别名 "coding"（自动路由到 GitHub Models / gpt-4.1-mini）
curl http://127.0.0.1:9000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "coding",
    "messages": [{"role": "user", "content": "Hello"}]
  }'
```

### 流式输出

```bash
curl http://127.0.0.1:9000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "coding",
    "messages": [{"role": "user", "content": "Explain Rust ownership"}],
    "stream": true
  }'
```

### Agent 感知请求

```bash
curl http://127.0.0.1:9000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "X-Agent-Name: hermes" \
  -d '{
    "model": "coding",
    "messages": [{"role": "user", "content": "Write a fibonacci function"}]
  }'
```

### 健康检查

```bash
curl http://127.0.0.1:9000/health
```

### 详细指标

```bash
curl http://127.0.0.1:9000/metrics
```

### Provider 状态

```bash
curl http://127.0.0.1:9000/providers
```

## API 接口

### OpenAI 兼容接口

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/v1/models` | 列出所有可用模型 |
| POST | `/v1/chat/completions` | 聊天补全（支持流式） |

### 管理接口

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/health` | 健康状态 |
| GET | `/status` | 网关状态 |
| GET | `/metrics` | 详细指标 |
| GET | `/metrics/prometheus` | Prometheus 文本格式指标 |
| GET | `/providers` | Provider 状态列表 |
| GET | `/admin` | **Admin Dashboard** — 浏览器内建管理面板 |
| GET | `/admin/status` | Dashboard 数据 API（Provider/Key 实时状态） |
| GET | `/admin/providers/:name/models` | 单个 Provider 的模型列表及启用状态 |
| POST | `/admin/providers/:name/refresh` | 刷新 Provider 模型列表 |
| POST | `/admin/providers/:name/test` | 测试 Provider 连通性 |
| POST | `/admin/providers/:name/keys/:key_id/restore` | 手工恢复 401/403 禁用的 Key |
| POST | `/admin/providers/:name/models/:id/toggle` | 启用/禁用模型 |
| POST | `/admin/save` | 保存模型配置变更 |
| GET | `/admin/config` | 当前配置（只读） |
| GET | `/admin/metadata` | 模型元数据统计 |
| GET | `/admin/metadata/sync` | 元数据同步状态 |
| GET | `/admin/metadata/models` | 已学习模型列表 |
| GET | `/admin/metadata/errors` | 错误记录汇总 |
| GET | `/admin/events` | SSE 实时事件流 |

### 自定义请求头

| 头部 | 说明 |
|------|------|
| `X-Agent-Name` | Agent 名称（用于模型路由） |
| `X-Request-Id` | 自定义请求 ID |

## 项目结构

```
src/
├── main.rs              # 入口
├── lib.rs               # 库根
├── config.rs            # 配置加载与解析
├── error.rs             # 统一错误类型
├── api/
│   ├── mod.rs           # API 路由注册
│   ├── chat.rs          # /v1/chat/completions
│   ├── models.rs        # /v1/models
│   ├── status.rs        # /health, /status, /metrics, /providers
│   ├── admin.rs         # Admin Dashboard 后端 API
│   └── admin_html.rs    # Admin Dashboard HTML/CSS/JS（内嵌单页）
├── providers/
│   ├── mod.rs           # Provider 工厂
│   ├── traits.rs        # Provider trait 定义
│   ├── github_models.rs # GitHub Models 实现
│   ├── nvidia.rs        # NVIDIA NIM 实现
│   ├── openai_compatible.rs  # OpenAI 兼容实现
│   └── ollama.rs        # Ollama 实现
├── keyhub/
│   └── mod.rs           # KeyHub + KeyPool（多 Key 轮换）
├── router/
│   └── mod.rs           # 模型路由 + Provider Fallback
├── watcher/
│   └── mod.rs           # 后台健康检查任务
├── health/
│   └── mod.rs           # 健康状态注册表
├── models/
│   └── mod.rs           # 数据模型（OpenAI 兼容格式）
├── metadata/
│   ├── mod.rs           # 模型元数据管理
│   ├── learner.rs       # 模型特征学习
│   └── sync.rs          # 元数据同步
└── state/
    └── mod.rs           # 状态持久化（JSON 文件）
tests/
├── provider_tests.rs    # Provider 测试
├── router_tests.rs      # 路由测试
├── keyhub_tests.rs      # KeyPool/KeyHub 测试
└── config_state_health_tests.rs  # 配置/状态/健康测试
```

## License

MIT
