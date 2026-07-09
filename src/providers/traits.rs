/// Provider trait definition.
///
/// Every upstream AI service implements this trait.
use async_trait::async_trait;
use bytes::Bytes;

use crate::error::{GatewayError, GatewayResult, parse_retry_after_value};
use crate::models::ChatCompletionRequest;

/// Rate-limit information extracted from upstream provider response headers.
/// Used to tighten per-key limits without waiting for 429 errors.
#[derive(Debug, Clone, Default)]
pub struct RateLimitInfo {
    /// Max requests per minute (None = unknown).
    pub rpm_limit: Option<u32>,
    /// Remaining requests in current minute window (None = unknown).
    pub rpm_remaining: Option<u32>,
    /// Max requests per day (None = unknown).
    pub rpd_limit: Option<u32>,
    /// Remaining requests in current day window (None = unknown).
    pub rpd_remaining: Option<u32>,
    /// Max prompt+completion tokens per minute (None = unknown).
    pub tpm_limit: Option<u32>,
    /// Remaining tokens in current minute window (None = unknown).
    pub tpm_remaining: Option<u32>,
    /// Max tokens per day (None = unknown).
    pub tpd_limit: Option<u32>,
    /// Remaining tokens in current day window (None = unknown).
    pub tpd_remaining: Option<u32>,
}

/// Parse common X-RateLimit-* response headers into structured info.
/// Handles multiple naming conventions:
///   - Groq:   x-ratelimit-limit-requests, x-ratelimit-remaining-requests
///   - Cerebras: x-ratelimit-requests-limit, x-ratelimit-requests-remaining,
///               x-ratelimit-tokens-limit, x-ratelimit-tokens-remaining
///   - NVIDIA/OpenRouter: x-ratelimit-limit, x-ratelimit-remaining
///   - Generic: x-rate-limit-limit, x-rate-limit-remaining
pub fn extract_rate_limit_headers(headers: &reqwest::header::HeaderMap) -> RateLimitInfo {
    fn get_header(h: &reqwest::header::HeaderMap, names: &[&str]) -> Option<u32> {
        for name in names {
            if let Some(v) = h
                .get(*name)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u32>().ok())
            {
                return Some(v);
            }
        }
        None
    }

    let rpm_limit = get_header(
        headers,
        &[
            "x-ratelimit-limit-requests",
            "x-ratelimit-requests-limit",
            "x-ratelimit-limit",
            "x-rate-limit-limit",
        ],
    );
    let rpm_remaining = get_header(
        headers,
        &[
            "x-ratelimit-remaining-requests",
            "x-ratelimit-requests-remaining",
            "x-ratelimit-remaining",
            "x-rate-limit-remaining",
        ],
    );
    // Token limits — Cerebras-style
    let tpm_limit = get_header(headers, &["x-ratelimit-tokens-limit"]);
    let tpm_remaining = get_header(headers, &["x-ratelimit-tokens-remaining"]);

    // No standard per-day headers, but we can infer if we see a high limit
    // that's clearly daily (e.g. 50000 tokens) vs per-minute (e.g. 30 req).
    let (rpd_limit, rpd_remaining) = (None, None);

    RateLimitInfo {
        rpm_limit,
        rpm_remaining,
        rpd_limit,
        rpd_remaining,
        tpm_limit,
        tpm_remaining,
        tpd_limit: None,
        tpd_remaining: None,
    }
}

/// Streaming response from a provider.
pub type StreamResponse = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<Bytes, crate::error::GatewayError>> + Send>,
>;

/// Result of a non-streaming chat completion.
#[derive(Debug)]
pub struct ChatResponse {
    pub body: serde_json::Value,
    pub status: u16,
    /// Rate-limit information extracted from upstream response headers.
    /// None if not collected (legacy providers).
    pub rate_limits: Option<RateLimitInfo>,
}

impl ChatResponse {
    pub fn new(body: serde_json::Value, status: u16, rate_limits: Option<RateLimitInfo>) -> Self {
        Self {
            body,
            status,
            rate_limits,
        }
    }
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

        let rate_limits = extract_rate_limit_headers(resp.headers());
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

        Ok(ChatResponse::new(response_body, status, Some(rate_limits)))
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
