use async_trait::async_trait;

use crate::config::ProviderConfig;
use crate::error::{GatewayError, GatewayResult, parse_retry_after_value};
use crate::models::ChatCompletionRequest;
use crate::providers::traits::{ChatResponse, Provider, StreamResponse};
use crate::providers::{http_client, send_stream_request, streaming_client_with_proxy};

/// Cloudflare Workers AI provider.
///
/// Uses the OpenAI-compatible endpoint (`/ai/v1/chat/completions`) which
/// returns standard OpenAI-formatted responses. Also includes a defensive
/// `result`-unwrapping fallback in case the response is wrapped
/// (as the native `/ai/run/{model}` endpoint does).
///
/// Model discovery uses `/ai/models/search` since standard `/v1/models`
/// is not supported by Cloudflare.
#[derive(Debug)]
pub struct CloudflareProvider {
    name: String,
    /// Account-level base URL (without /ai/v1 suffix).
    /// e.g. https://api.cloudflare.com/client/v4/accounts/{id}
    account_base: String,
    proxy_url: Option<String>,
    health_check_model: String,
    timeout_seconds: u64,
    priority: u32,
}

impl CloudflareProvider {
    pub fn new(name: &str, config: &ProviderConfig) -> Self {
        // Strip /ai/v1 suffix if present to get the account-level base
        let raw = config.base_url.trim_end_matches('/');
        let account_base = raw
            .strip_suffix("/ai/v1")
            .or_else(|| raw.strip_suffix("/ai"))
            .unwrap_or(raw)
            .to_string();

        Self {
            name: name.to_string(),
            account_base,
            proxy_url: config.proxy_url.clone(),
            health_check_model: if config.health_check_model.is_empty() {
                "@cf/meta/llama-3.2-3b-instruct".into()
            } else {
                config.health_check_model.clone()
            },
            timeout_seconds: config.timeout_seconds,
            priority: config.priority,
        }
    }

    /// Model discovery URL: /ai/models/search
    fn models_search_url(&self) -> String {
        format!("{}/ai/models/search", self.account_base)
    }

    /// OpenAI-compatible chat completions URL.
    /// Returns standard OpenAI responses (no `result` wrapper).
    fn chat_completions_url(&self) -> String {
        format!("{}/ai/v1/chat/completions", self.account_base)
    }

    /// Extract model IDs from Cloudflare models/search response.
    fn extract_model_ids(json: &serde_json::Value) -> Vec<String> {
        let entries = match json["result"].as_array() {
            Some(entries) => entries,
            None => return Vec::new(),
        };

        let mut models: Vec<String> = entries
            .iter()
            .filter(|m| {
                m["task"]["name"]
                    .as_str()
                    .is_none_or(|task| task.eq_ignore_ascii_case("Text Generation"))
            })
            .filter_map(Self::callable_model_name)
            .collect();

        if models.is_empty() {
            models = entries
                .iter()
                .filter_map(Self::callable_model_name)
                .collect();
        }

        models.sort();
        models.dedup();
        models
    }

    fn callable_model_name(model: &serde_json::Value) -> Option<String> {
        ["name", "model", "id"]
            .iter()
            .filter_map(|field| model[*field].as_str())
            .find(|candidate| candidate.starts_with("@cf/"))
            .map(|candidate| candidate.to_string())
    }

    /// Normalize a Cloudflare response to OpenAI-compatible format.
    /// Cloudflare's native `/ai/run/{model}` endpoint wraps the response
    /// in a `result` envelope; the OpenAI-compatible endpoint does not.
    /// This defensive function handles both cases.
    fn normalize_response(raw_body: &serde_json::Value) -> serde_json::Value {
        let mut body = raw_body["result"].clone();
        if body.is_null() {
            body = raw_body.clone();
        }

        // Some models return content: null with reasoning in the
        // `reasoning` field — promote to `content` so the gateway's
        // response_has_useful_output() can see it.
        if let Some(choices) = body.get_mut("choices").and_then(|c| c.as_array_mut()) {
            if let Some(first) = choices.first_mut() {
                if let Some(msg) = first.get_mut("message").and_then(|m| m.as_object_mut()) {
                    if msg.get("content").map_or(true, |c| c.is_null()) {
                        if let Some(reasoning) = msg.get("reasoning").and_then(|r| r.as_str()) {
                            if !reasoning.is_empty() {
                                msg.insert(
                                    "content".to_string(),
                                    serde_json::Value::String(reasoning.to_string()),
                                );
                            }
                        }
                    }
                }
            }
        }

        body
    }
}

#[async_trait]
impl Provider for CloudflareProvider {
    fn name(&self) -> &str {
        &self.name
    }
    fn base_url(&self) -> &str {
        &self.account_base
    }
    fn health_check_model(&self) -> &str {
        &self.health_check_model
    }
    fn timeout_seconds(&self) -> u64 {
        self.timeout_seconds
    }
    fn priority(&self) -> u32 {
        self.priority
    }

    /// Cloudflare doesn't have /v1/models; use /ai/models/search.
    async fn list_models(&self, api_key: &str) -> GatewayResult<Vec<String>> {
        let url = self.models_search_url();
        let client = http_client(self.timeout_seconds, self.proxy_url.as_deref())?;
        let resp = client
            .get(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .timeout(std::time::Duration::from_secs(self.timeout_seconds))
            .send()
            .await?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let retry_after_seconds = retry_after_seconds(resp.headers());
            let body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::http_error(status, body, retry_after_seconds));
        }

        let json: serde_json::Value = resp.json().await?;
        let models = Self::extract_model_ids(&json);
        Ok(models)
    }

    /// Cloudflare chat via OpenAI-compatible endpoint: POST /ai/v1/chat/completions.
    /// Normalizes responses defensively (handles both OpenAI and wrapped formats).
    async fn chat(
        &self,
        api_key: &str,
        request: ChatCompletionRequest,
    ) -> GatewayResult<ChatResponse> {
        let url = self.chat_completions_url();

        let client = http_client(self.timeout_seconds, self.proxy_url.as_deref())?;
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(self.timeout_seconds))
            .json(&request)
            .send()
            .await?;

        let status = resp.status().as_u16();
        let is_success = resp.status().is_success();
        let retry_after_seconds = retry_after_seconds(resp.headers());
        let raw_body: serde_json::Value = resp.json().await?;
        if !is_success {
            let msg = raw_body["errors"]
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|e| e["message"].as_str())
                .or_else(|| raw_body["error"]["message"].as_str())
                .unwrap_or(&raw_body.to_string())
                .to_string();
            return Err(GatewayError::http_error(status, msg, retry_after_seconds));
        }

        // Defensive: normalize Cloudflare's response to OpenAI format.
        // Via /ai/v1/chat/completions the response is already standard;
        // this handles any residual `result` wrapping from fallback code paths.
        let openai_body = Self::normalize_response(&raw_body);

        Ok(ChatResponse::new(openai_body, status, None))
    }

    /// Cloudflare streaming via OpenAI-compatible endpoint.
    async fn chat_stream(
        &self,
        api_key: &str,
        request: ChatCompletionRequest,
    ) -> GatewayResult<StreamResponse> {
        let url = self.chat_completions_url();
        let mut stream_request = request.clone();
        stream_request.stream = Some(true);

        let client = streaming_client_with_proxy(self.timeout_seconds, self.proxy_url.as_deref())?;
        let resp = send_stream_request(
            client
                .post(&url)
                .header("Authorization", format!("Bearer {api_key}"))
                .header("Content-Type", "application/json")
                .header("Accept-Encoding", "identity")
                .json(&stream_request),
            self.timeout_seconds,
        )
        .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let retry_after_seconds = retry_after_seconds(resp.headers());
            let body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::http_error(status, body, retry_after_seconds));
        }

        let stream = futures::StreamExt::map(resp.bytes_stream(), |item| {
            item.map_err(|e| GatewayError::UpstreamError(e.to_string()))
        });
        Ok(Box::pin(stream))
    }

    async fn health_check(&self, api_key: &str) -> GatewayResult<u64> {
        let start = std::time::Instant::now();
        let models = self.list_models(api_key).await?;
        let elapsed = start.elapsed().as_millis() as u64;

        if models.is_empty() {
            return Err(GatewayError::UpstreamError(
                "No models returned from Cloudflare Workers AI".into(),
            ));
        }

        Ok(elapsed)
    }
}

fn retry_after_seconds(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after_value)
}
