# free-agent-gateway

一个给个人 AI Agent 使用的 OpenAI 兼容网关。

它把 OpenRouter、OpenCode、NVIDIA NIM、Groq、Cerebras、Cloudflare Workers AI、Hugging Face Router、本地 Ollama 等不同来源统一成一个本地 API：

```text
http://127.0.0.1:9000/v1
```

你的 Agent 只需要接入这一个地址，网关负责在多个 provider、多个 key、多个模型之间自动选择、轮换、降级和记录状态。

## 适合解决什么问题

- 免费模型和免费 key 很多，但每家限制不同，手工切换很麻烦。
- 某个 key 429、403、5xx 或返回空内容时，希望自动换下一个可用 key。
- 同一个模型在不同 provider 上都可用，希望统一管理和调度。
- 希望 OpenAI SDK、Codex、OpenClaw、Hermes、各种 Agent 工具都用同一个入口。
- 希望在浏览器里看到 key、模型、provider、token、错误和冷却状态。

## 主要功能

- OpenAI 兼容接口：`/v1/models`、`/v1/chat/completions`、`/v1/completions`、`/v1/responses`
- 流式输出支持：兼容 `stream=true`，并处理 stream tool calls
- 多 provider 聚合：一个网关管理多个上游 API
- 多 key 自动轮换：同一 provider 下多个 key 自动接管
- 免费优先：`tier: free` 优先使用，`tier: paid` 只作为最后兜底
- 真实可用性判断：区分认证错误、额度限制、区域不可用、模型不可用、临时 5xx、WAF/Cloudflare 拦截
- 冷却和恢复：429 后按 `Retry-After` 或已学习规则冷却，避免反复打坏 key
- 模型列表缓存：`/v1/models` 不再每次慢速全量探测
- 模型合并显示：可把同一模型的 `:free` 等价格后缀合并展示，同时保留实际调用路由
- Provider 前缀路由：可以直接调用指定 provider
- 自动调度路由：可按任务、agent、能力、历史错误和可用额度选择模型
- Token 统计：记录输入、输出、总 token，区分 provider 上报值和本地估算值
- 用量面板：按 1 天、7 天、30 天、90 天等维度查看使用趋势
- 浏览器管理界面：查看 provider、key、模型、错误、token、路由分组和健康状态
- 本地 Ollama 支持：可作为本地 fallback
- Hugging Face Router 预留：后续填入 `HF_TOKEN` 即可启用

## 快速使用

启动 free-agent-gateway 后，打开浏览器管理页：

```text
http://127.0.0.1:9000/admin
```

把你的 Agent、OpenAI SDK 或其他兼容工具的 API 地址改成：

```text
http://127.0.0.1:9000/v1
```

## 配置 key

编辑 `config.yaml`，给需要启用的 provider 填入 key。

推荐用环境变量，不要把真实 key 写进 Git：

```yaml
providers:
  openrouter:
    type: "openai_compatible"
    enabled: true
    base_url: "https://openrouter.ai/api/v1"
    keys:
      - value: "${OPENROUTER_API_KEY}"
        tier: free
        rpm_limit: 20
        rpd_limit: 1000

  huggingface:
    type: "huggingface"
    enabled: false
    base_url: "https://router.huggingface.co/v1"
    keys:
      - value: "${HF_TOKEN}"
        tier: free
```

`tier` 的含义：

- `free`：正常自动调度使用
- `paid`：只在免费 key 不可用时兜底
- `unknown`：不会自动使用，适合还没确认额度和成本的 key

## 调用方式

查看模型：

```bash
curl http://127.0.0.1:9000/v1/models
```

普通聊天：

```bash
curl http://127.0.0.1:9000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "auto",
    "messages": [
      {"role": "user", "content": "写一个 Rust 版本的快速排序"}
    ]
  }'
```

流式输出：

```bash
curl http://127.0.0.1:9000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "auto",
    "stream": true,
    "messages": [
      {"role": "user", "content": "解释一下这个项目适合怎么给 Agent 使用"}
    ]
  }'
```

指定 provider：

```text
http://127.0.0.1:9000/openrouter/v1/models
http://127.0.0.1:9000/openrouter/v1/chat/completions
```

也可以使用配置里的 provider 名称，例如：

```text
http://127.0.0.1:9000/groq/v1/chat/completions
http://127.0.0.1:9000/cerebras/v1/chat/completions
http://127.0.0.1:9000/huggingface/v1/chat/completions
```

## Agent 接入

把任何支持 OpenAI API 的工具指向：

```text
base_url: http://127.0.0.1:9000/v1
api_key: 任意非空字符串
model: auto
```

如果工具支持自定义请求头，可以带上 Agent 名称：

```http
X-Agent-Name: codex
```

网关会根据 agent、任务内容、模型能力、历史错误、key 可用状态和额度压力选择更合适的模型。

## 管理页面

打开：

```text
http://127.0.0.1:9000/admin
```

可以查看：

- 每个 provider 是否可用
- 每个 key 是否可用、冷却、禁用或待验证
- 每个模型来自哪些 provider
- 自动生成的 provider 路由和分组路由
- token 使用量、请求数、活跃天数和模型占比
- 近期错误、失败原因和自动切换记录
- 手动刷新模型、测试 provider、恢复 key

## 常用端点

```text
GET  /v1/models
POST /v1/chat/completions
POST /v1/completions
POST /v1/responses

GET  /{provider}/v1/models
POST /{provider}/v1/chat/completions

GET  /admin
GET  /health
GET  /status
GET  /metrics
GET  /metrics/prometheus
```

## 给 AI 使用的 Skill

仓库提供了一个可直接给 Codex、Claude Code、OpenCode 或其他 Agent 阅读的技能文件：

```text
docs/skills/free-agent-gateway/SKILL.md
```

用途：

- 让 AI 知道应该把 OpenAI API 地址指向 `http://127.0.0.1:9000/v1`
- 让 AI 默认使用 `model: auto`
- 让 AI 在需要锁定 provider 时使用 `/{provider}/v1/...`
- 让 AI 遇到 429、403、模型不存在、空回复、stream 中断时不要自己猜测原因，而是查看管理页状态或错误日志
- 让 AI 知道 token 统计应包含输入和输出，并区分 provider 上报和本地估算

你可以把这个 `SKILL.md` 复制到自己的 Agent 技能目录，或在 Agent 提示词里引用它。

## License

MIT
