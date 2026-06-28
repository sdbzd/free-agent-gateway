# free-agent-gateway

面向个人 Agent 工作流的 OpenAI-compatible API 网关。当前主要服务 OpenClaw 和 Hermes，后续可继续接入更多免费 OpenAI 格式 API。

## 重点能力

- 多 provider、多 key 自动轮换。
- 免费 key 优先，paid key 只作为最后兜底。
- 自动识别 OpenRouter 普通免费 key 与充值后高额度 key。
- 429、5xx、Cloudflare/WAF 拦截与认证失败分开处理。
- 401/403 认证失败支持手工恢复。
- 请求数、token、模型用量自动累计。
- Admin 页面展示每个 key、provider、model 的可用状态与剩余额度。
- Cloudflare Workers AI 使用官方 `models/search` 发现可用文本模型。
- Linux release 构建脚本与 GitHub Actions artifact。

## 安全说明

不要提交真实 `config.yaml`、`state.json`、`state.db`、日志、监控数据或本地临时文件。仓库只发布 `config.yaml.sample`。

## 快速启动

```bash
cp config.yaml.sample config.yaml
cargo run
```

访问：

```text
http://127.0.0.1:9000/admin
http://127.0.0.1:9000/v1/models
```

## Linux 构建

```bash
chmod +x scripts/build-linux.sh
./scripts/build-linux.sh
```

产物：

```text
dist/free-agent-gateway-linux-<arch>.tar.gz
```
