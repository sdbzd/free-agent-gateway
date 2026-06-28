/// Provider trait definition.
///
/// Every upstream AI service implements this trait.
use async_trait::async_trait;
use bytes::Bytes;

use crate::error::GatewayResult;
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
