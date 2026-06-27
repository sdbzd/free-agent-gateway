# 新增功能：Per-Key 速率追踪、Context Handoff、Vision 图片输入

> 对应 freellmapi 的三项功能移植。实现于 2026-06-26。

---

## 1. Per-Key 速率追踪 (Rate Tracking)

### 功能说明

在原有的 Key 自动轮换（RoundRobin/Random/LeastFailed/Priority）基础上，增加对每个 Key 的请求速率和 Token 用量的追踪。当一个 Key 超过配置的速率限制时，路由自动跳过该 Key，选择其他可用 Key。

### 追踪指标

| 指标 | 含义 | 窗口 |
|------|------|------|
| RPM | Requests Per Minute | 每分钟 |
| RPD | Requests Per Day | 每天 |
| TPM | Tokens Per Minute（Prompt + Completion） | 每分钟 |
| TPD | Tokens Per Day（Prompt + Completion） | 每天 |

### 配置方式

在 `config.yaml` 的 Key 配置中增加速率限制字段：

```yaml
providers:
  github:
    type: "github_models"
    base_url: "https://models.inference.ai.azure.com"
    keys:
      - value: "${GITHUB_TOKEN_1}"
        tier: free
        rpm_limit: 30        # 每分钟最多 30 请求
        rpd_limit: 1000      # 每天最多 1000 请求
        tpm_limit: 200000    # 每分钟最多 200K tokens
        tpd_limit: 5000000   # 每天最多 5M tokens
      - value: "${GITHUB_TOKEN_2}"
        tier: free
        rpm_limit: 30
        rpd_limit: 1000
```

所有限制均为可选，不配置则视为无限制。Legacy 字符串格式的 Key（如 `"${TOKEN}"`）不会设置任何速率限制。

### 实现原理

- **数据结构**：`KeyState` 新增 `rpm_count`、`rpd_count`、`tpm_prompt_count`、`tpm_completion_count` 等计数器和窗口时间戳。
- **窗口策略**：固定窗口（每分钟/每天），时间戳不匹配时自动归零。
- **过滤时机**：
  - `KeyPool::acquire_key()` — 选择 Key 时，跳过 `is_rate_limited()` 返回 true 的 Key。
  - `KeyPool::free_candidates()` — 构建 fallback 链时同样过滤。
  - `KeyPool::report_success()` — 成功后更新计数（Token 用量从响应体 `usage` 字段提取）。
- **Token 追踪**：非流式请求从 response 的 `usage.prompt_tokens` / `usage.completion_tokens` 提取；流式请求仅计数 RPM/RPD，Token 用量传 None。

### 涉及文件

- `src/config.rs` — `KeyConfig::Detailed` 新增 `rpm_limit`/`rpd_limit`/`tpm_limit`/`tpd_limit`
- `src/models/mod.rs` — `KeyState` 新增速率字段 + `is_rate_limited()` / `reset_rate_windows()` 方法 + `extract_usage()` 函数
- `src/keyhub/mod.rs` — `KeyPool` 在 acquire/report/filter 中集成速率检查

---

## 2. Context Handoff（上下文移交）

### 功能说明

当请求在多个 Provider 之间 fallback 时（例如 GitHub Models 失败→切换到 NVIDIA），新 Provider 的模型不知道之前发生了什么。Context Handoff 自动在请求消息头部注入一条系统消息，告知新模型发生了 Provider 切换，保证对话上下文的连续性。

### 注入消息示例

```
[Context handoff: Previous provider "github" failed.
 Continuing with "nvidia" (model: meta/llama-3.1-70b-instruct).
 The full conversation history is preserved below.]
```

### 触发时机

- `Router::chat()` — 非流式请求，当 `attempt_index > 0` 时注入。
- `Router::chat_stream()` — 流式请求，同样处理。
- 仅在 fallback 发生时注入，首次尝试不会注入。

### 实现原理

在 `router/mod.rs` 中新增 `inject_context_handoff()` 函数：

```rust
fn inject_context_handoff(
    mut request: ChatCompletionRequest,
    previous_provider: &str,
    new_provider: &str,
    model: &str,
) -> ChatCompletionRequest {
    let handoff_msg = format!(
        "[Context handoff: Previous provider \"{}\" failed. \
         Continuing with \"{}\" (model: {}). \
         The full conversation history is preserved below.]",
        previous_provider, new_provider, model,
    );
    request.messages.insert(0, ChatMessage {
        role: "system".into(),
        content: serde_json::Value::String(handoff_msg),
        name: None, tool_calls: None, tool_call_id: None,
    });
    request
}
```

每次 fallback 时，记录 `last_provider`，下一次尝试前调用此函数。

### 涉及文件

- `src/router/mod.rs` — `inject_context_handoff()` 函数 + `chat()` / `chat_stream()` 中的调用逻辑

---

## 3. Vision / 图片输入支持

### 功能说明

支持 OpenAI 格式的图片输入（`image_url` content parts），自动检测请求中是否包含图片内容，记录日志并提供视觉模型能力识别。

### 支持的输入格式

用户发送消息时使用 OpenAI Vision 标准格式：

```json
{
  "model": "gpt-4o",
  "messages": [
    {
      "role": "user",
      "content": [
        {"type": "text", "text": "这张图里有什么？"},
        {"type": "image_url", "image_url": {"url": "https://example.com/photo.jpg"}}
      ]
    }
  ]
}
```

### 检测与路由

- `request_has_vision()` — 扫描所有消息，检查 `content` 数组是否包含 `type: "image_url"` 的 part。
- 检测到 vision 请求时打印 `info` 日志。
- `model_supports_vision()` — 根据模型名称模式判断是否支持视觉（`gpt-4o`、`gpt-4.1`、`claude-3`、`claude-4`、`gemini-1.5`、`gemini-2`、`llava`、`qwen-vl`、`phi-3-vision` 等）。
- 如果模型名称不匹配视觉模式，打印 `warn` 日志，但仍会尝试路由（Provider 不支持时会返回 4xx，网关自动 fallback）。

### 数据模型

```rust
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

pub struct ImageUrl {
    pub url: String,
    pub detail: Option<String>,
}
```

这些类型主要用于序列化/反序列化和类型安全访问。实际消息体使用 `serde_json::Value` 存储，保持与 OpenAI API 的完全兼容。

### 涉及文件

- `src/models/mod.rs` — `ContentPart` / `ImageUrl` 枚举 + `message_has_vision()` / `request_has_vision()` 函数
- `src/router/mod.rs` — `model_supports_vision()` helper 函数 + vision 检测日志

---

## 测试

全部 73 个测试通过：

```
running 73 tests
test result: ok. 73 passed; 0 failed
```

包括新增的速率追踪行为（通过 `KeyState::is_rate_limited()` / `reset_rate_windows()` 的窗口机制以及与现有 `report_success` / `report_failure` 交互）。
