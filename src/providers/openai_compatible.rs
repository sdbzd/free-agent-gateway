use async_trait::async_trait;

use crate::config::ProviderConfig;
use crate::error::{GatewayError, GatewayResult, parse_retry_after_value};
use crate::models::ChatCompletionRequest;
use crate::providers::traits::{ChatResponse, Provider, StreamResponse};
use crate::providers::{http_client, send_stream_request, streaming_client_with_proxy};

/// Fields that the OpenAI spec does not define and that some OpenAI-compatible
/// providers (e.g. Cerebras) reject as unsupported parameters.
const UNSUPPORTED_EXTRA_FIELDS: &[&str] = &["provider"];

/// Remove known unsupported fields from the request's `extra` map before
/// serializing and sending to the upstream provider. This prevents errors
/// from strict OpenAI-compatible providers that reject unknown fields.
fn strip_unsupported_extra_fields(request: &mut ChatCompletionRequest) {
    for field in UNSUPPORTED_EXTRA_FIELDS {
        request.extra.remove(*field);
    }
}

/// Generic OpenAI-compatible provider.
///
/// Works with any service that implements the OpenAI chat completions API.
#[derive(Debug)]
pub struct OpenAiCompatibleProvider {
    name: String,
    base_url: String,
    proxy_url: Option<String>,
    health_check_model: String,
    timeout_seconds: u64,
    priority: u32,
}

impl OpenAiCompatibleProvider {
    pub fn new(name: &str, config: &ProviderConfig) -> Self {
        Self {
            name: name.to_string(),
            base_url: config.base_url.clone(),
            proxy_url: config.proxy_url.clone(),
            health_check_model: config.health_check_model.clone(),
            timeout_seconds: config.timeout_seconds,
            priority: config.priority,
        }
    }
}

async fn parse_json_body(
    resp: reqwest::Response,
    provider_name: &str,
    endpoint: &str,
) -> GatewayResult<(u16, bool, Option<u64>, serde_json::Value)> {
    let status = resp.status().as_u16();
    let is_success = resp.status().is_success();
    let retry_after_seconds = retry_after_seconds(resp.headers());
    let body = resp.text().await?;
    match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(json) => Ok((status, is_success, retry_after_seconds, json)),
        Err(error) if is_success => Err(GatewayError::UpstreamError(format!(
            "non-JSON response from upstream provider={provider_name} endpoint={endpoint} status={status}: {error}; body_preview={}",
            preview_body(&body)
        ))),
        Err(_) => Ok((
            status,
            is_success,
            retry_after_seconds,
            serde_json::json!({ "error": { "message": preview_body(&body) } }),
        )),
    }
}

fn preview_body(body: &str) -> String {
    const LIMIT: usize = 240;
    let compact = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.len() > LIMIT {
        format!("{}...", &compact[..LIMIT])
    } else {
        compact
    }
}

#[async_trait]
impl Provider for OpenAiCompatibleProvider {
    fn name(&self) -> &str {
        &self.name
    }
    fn base_url(&self) -> &str {
        &self.base_url
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

    async fn list_models(&self, api_key: &str) -> GatewayResult<Vec<String>> {
        let url = format!("{}/models", self.base_url);
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
            if matches!(status, 404 | 405)
                && !self.health_check_model.trim().is_empty()
                && body.contains("GET not supported")
            {
                if let Some(models_url) = cloudflare_models_search_url(&self.base_url) {
                    match fetch_cloudflare_models(
                        &client,
                        &models_url,
                        api_key,
                        self.timeout_seconds,
                    )
                    .await
                    {
                        Ok(models) if !models.is_empty() => {
                            tracing::info!(
                                provider = %self.name,
                                models = models.len(),
                                "Discovered Cloudflare Workers AI models via models/search"
                            );
                            return Ok(models);
                        }
                        Ok(_) => {
                            tracing::warn!(
                                provider = %self.name,
                                "Cloudflare models/search returned no models"
                            );
                        }
                        Err(err) => {
                            tracing::warn!(
                                provider = %self.name,
                                error = %err,
                                "Cloudflare models/search failed"
                            );
                        }
                    }
                }
                tracing::warn!(
                    provider = %self.name,
                    status,
                    "Provider does not support GET /models; using health_check_model as model inventory"
                );
                return Ok(vec![self.health_check_model.clone()]);
            }
            return Err(GatewayError::http_error(status, body, retry_after_seconds));
        }

        let (_, _, _, json) = parse_json_body(resp, &self.name, "models").await?;
        let models = json["data"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m["id"].as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        Ok(models)
    }

    async fn chat(
        &self,
        api_key: &str,
        mut request: ChatCompletionRequest,
    ) -> GatewayResult<ChatResponse> {
        strip_unsupported_extra_fields(&mut request);
        let url = format!("{}/chat/completions", self.base_url);
        let client = http_client(self.timeout_seconds, self.proxy_url.as_deref())?;
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(self.timeout_seconds))
            .json(&request)
            .send()
            .await?;

        let (status, is_success, retry_after_seconds, body) =
            parse_json_body(resp, &self.name, "chat/completions").await?;
        if !is_success {
            // Extract error message, preferring nested details from OpenRouter-style errors
            let msg = body["error"]["metadata"]["raw"]
                .as_str()
                .and_then(|raw| {
                    serde_json::from_str::<serde_json::Value>(raw)
                        .ok()
                        .and_then(|v| v["error"]["message"].as_str().map(|s| s.to_string()))
                })
                .or_else(|| body["error"]["message"].as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| body.to_string());
            return Err(GatewayError::http_error(status, msg, retry_after_seconds));
        }

        Ok(ChatResponse { body, status })
    }

    async fn chat_stream(
        &self,
        api_key: &str,
        mut request: ChatCompletionRequest,
    ) -> GatewayResult<StreamResponse> {
        strip_unsupported_extra_fields(&mut request);
        let mut stream_request = request.clone();
        stream_request.stream = Some(true);

        let url = format!("{}/chat/completions", self.base_url);
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
            return Err(GatewayError::UpstreamError("No models returned".into()));
        }

        Ok(elapsed)
    }

    async fn post_json(
        &self,
        api_key: &str,
        endpoint: &str,
        body: serde_json::Value,
    ) -> GatewayResult<ChatResponse> {
        let endpoint = endpoint.trim_start_matches('/');
        let url = format!("{}/{}", self.base_url.trim_end_matches('/'), endpoint);
        let client = http_client(self.timeout_seconds, self.proxy_url.as_deref())?;
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(self.timeout_seconds))
            .json(&body)
            .send()
            .await?;

        let (status, is_success, retry_after_seconds, body) =
            parse_json_body(resp, &self.name, endpoint).await?;
        if !is_success {
            let msg = body["error"]["metadata"]["raw"]
                .as_str()
                .and_then(|raw| {
                    serde_json::from_str::<serde_json::Value>(raw)
                        .ok()
                        .and_then(|v| v["error"]["message"].as_str().map(|s| s.to_string()))
                })
                .or_else(|| body["error"]["message"].as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| body.to_string());
            return Err(GatewayError::http_error(status, msg, retry_after_seconds));
        }

        Ok(ChatResponse { body, status })
    }
}

fn retry_after_seconds(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after_value)
}

fn cloudflare_models_search_url(base_url: &str) -> Option<String> {
    if !base_url.contains("/client/v4/accounts/") {
        return None;
    }

    base_url
        .strip_suffix("/ai/v1")
        .map(|prefix| format!("{prefix}/ai/models/search"))
}

async fn fetch_cloudflare_models(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    timeout_seconds: u64,
) -> GatewayResult<Vec<String>> {
    let resp = client
        .get(url)
        .header("Authorization", format!("Bearer {api_key}"))
        .timeout(std::time::Duration::from_secs(timeout_seconds))
        .send()
        .await?;

    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        let retry_after_seconds = retry_after_seconds(resp.headers());
        let body = resp.text().await.unwrap_or_default();
        return Err(GatewayError::http_error(status, body, retry_after_seconds));
    }

    let json: serde_json::Value = resp.json().await?;
    Ok(extract_cloudflare_model_ids(&json))
}

fn extract_cloudflare_model_ids(json: &serde_json::Value) -> Vec<String> {
    let entries = match json["result"]
        .as_array()
        .or_else(|| json["data"].as_array())
    {
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
        .filter_map(cloudflare_callable_model_name)
        .collect();

    if models.is_empty() {
        models = entries
            .iter()
            .filter_map(cloudflare_callable_model_name)
            .collect();
    }

    models.sort();
    models.dedup();
    models
}

fn cloudflare_callable_model_name(model: &serde_json::Value) -> Option<String> {
    ["name", "model", "id"]
        .iter()
        .filter_map(|field| model[*field].as_str())
        .find(|candidate| candidate.starts_with("@cf/"))
        .map(|candidate| candidate.to_string())
}
