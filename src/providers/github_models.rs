use async_trait::async_trait;

use crate::config::ProviderConfig;
use crate::error::{GatewayError, GatewayResult};
use crate::models::ChatCompletionRequest;
use crate::providers::traits::{ChatResponse, Provider, StreamResponse};

/// GitHub Models provider (models.inference.ai.azure.com).
#[derive(Debug)]
pub struct GithubModelsProvider {
    name: String,
    base_url: String,
    health_check_model: String,
    timeout_seconds: u64,
    priority: u32,
}

impl GithubModelsProvider {
    pub fn new(name: &str, config: &ProviderConfig) -> Self {
        Self {
            name: name.to_string(),
            base_url: config.base_url.clone(),
            health_check_model: if config.health_check_model.is_empty() {
                "openai/gpt-4.1-mini".into()
            } else {
                config.health_check_model.clone()
            },
            timeout_seconds: config.timeout_seconds,
            priority: config.priority,
        }
    }
}

#[async_trait]
impl Provider for GithubModelsProvider {
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
        let client = reqwest::Client::new();
        let resp = client
            .get(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .timeout(std::time::Duration::from_secs(self.timeout_seconds))
            .send()
            .await?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::HttpError {
                status,
                message: body,
            });
        }

        let json: serde_json::Value = resp.json().await?;
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
        request: ChatCompletionRequest,
    ) -> GatewayResult<ChatResponse> {
        let url = format!("{}/chat/completions", self.base_url);
        let client = reqwest::Client::new();
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
        let body: serde_json::Value = resp.json().await?;
        if !is_success {
            let msg = body["error"]["message"]
                .as_str()
                .unwrap_or(&body.to_string())
                .to_string();
            return Err(GatewayError::HttpError {
                status,
                message: msg,
            });
        }

        Ok(ChatResponse { body, status })
    }

    async fn chat_stream(
        &self,
        api_key: &str,
        request: ChatCompletionRequest,
    ) -> GatewayResult<StreamResponse> {
        let mut stream_request = request.clone();
        stream_request.stream = Some(true);

        let url = format!("{}/chat/completions", self.base_url);
        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(self.timeout_seconds))
            .json(&stream_request)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::HttpError {
                status,
                message: body,
            });
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
                "No models returned from GitHub Models".into(),
            ));
        }

        Ok(elapsed)
    }
}
