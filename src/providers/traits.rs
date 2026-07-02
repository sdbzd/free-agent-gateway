/// Provider trait definition.
///
/// Every upstream AI service implements this trait.
use async_trait::async_trait;
use bytes::Bytes;

use crate::error::{GatewayError, GatewayResult, parse_retry_after_value};
use crate::models::ChatCompletionRequest;

/// Streaming response from a provider.
pub type StreamResponse = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<Bytes, crate::error::GatewayError>> + Send>,
>;

/// Result of a non-streaming chat completion.
#[derive(Debug)]
pub struct ChatResponse {
    pub body: serde_json::Value,
    pub status: u16,
}

/// Abstract provider trait.
#[async_trait]
pub trait Provider: Send + Sync + std::fmt::Debug {
    /// Provider name (e.g. "github", "nvidia").
    fn name(&self) -> &str;

    /// Provider base URL.
    fn base_url(&self) -> &str;

    /// List available models from this provider.
    async fn list_models(&self, api_key: &str) -> GatewayResult<Vec<String>>;

    /// Send a non-streaming chat completion request.
    async fn chat(
        &self,
        api_key: &str,
        request: ChatCompletionRequest,
    ) -> GatewayResult<ChatResponse>;

    /// Send a streaming chat completion request.
    async fn chat_stream(
        &self,
        api_key: &str,
        request: ChatCompletionRequest,
    ) -> GatewayResult<StreamResponse>;

    /// Send a generic OpenAI-compatible JSON request to an upstream endpoint
    /// such as `/embeddings`.
    async fn post_json(
        &self,
        api_key: &str,
        endpoint: &str,
        body: serde_json::Value,
    ) -> GatewayResult<ChatResponse> {
        let endpoint = endpoint.trim_start_matches('/');
        let url = format!("{}/{}", self.base_url().trim_end_matches('/'), endpoint);
        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(self.timeout_seconds()))
            .json(&body)
            .send()
            .await?;

        let status = resp.status().as_u16();
        let is_success = resp.status().is_success();
        let retry_after_seconds = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_retry_after_value);
        let response_body: serde_json::Value = resp.json().await?;
        if !is_success {
            let msg = response_body["error"]["message"]
                .as_str()
                .unwrap_or(&response_body.to_string())
                .to_string();
            return Err(GatewayError::http_error(status, msg, retry_after_seconds));
        }

        Ok(ChatResponse {
            body: response_body,
            status,
        })
    }

    /// Health check: send a minimal request to verify the provider is alive.
    async fn health_check(&self, api_key: &str) -> GatewayResult<u64>;

    /// The model name to use for health checks.
    fn health_check_model(&self) -> &str;

    /// Request timeout in seconds.
    fn timeout_seconds(&self) -> u64;

    /// Priority for routing (lower = higher priority).
    fn priority(&self) -> u32;
}

/// Type-erased provider handle.
pub type BoxedProvider = Box<dyn Provider + 'static>;
