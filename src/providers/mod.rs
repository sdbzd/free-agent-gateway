/// Provider abstraction and implementations.
///
/// Each provider (GitHub Models, NVIDIA NIM, OpenAI-compatible, Ollama)
/// implements the `Provider` trait.
pub mod cloudflare;
pub mod github_models;
pub mod nvidia;
pub mod ollama;
pub mod openai_compatible;
pub mod traits;

pub use traits::{BoxedProvider, Provider};

use crate::config::ProviderConfig;
use crate::error::{GatewayError, GatewayResult};

/// Build an HTTP client for SSE/streaming responses.
///
/// Do not set reqwest's total request timeout here: for streaming responses it
/// covers the whole response body and can abort normal long generations. Keep a
/// bounded connect timeout and a per-read idle timeout instead.
pub(crate) fn streaming_client(timeout_seconds: u64) -> GatewayResult<reqwest::Client> {
    streaming_client_with_proxy(timeout_seconds, None)
}

pub(crate) fn http_client(
    timeout_seconds: u64,
    proxy_url: Option<&str>,
) -> GatewayResult<reqwest::Client> {
    let mut builder =
        reqwest::Client::builder().timeout(std::time::Duration::from_secs(timeout_seconds));
    if let Some(proxy_url) = proxy_url.filter(|value| !value.trim().is_empty()) {
        builder = builder.proxy(reqwest::Proxy::all(proxy_url)?);
    }
    Ok(builder.build()?)
}

pub(crate) fn streaming_client_with_proxy(
    timeout_seconds: u64,
    proxy_url: Option<&str>,
) -> GatewayResult<reqwest::Client> {
    let connect_timeout_s = timeout_seconds.clamp(5, 30);
    let read_timeout_s = timeout_seconds.max(120);
    let mut builder = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(connect_timeout_s))
        .read_timeout(std::time::Duration::from_secs(read_timeout_s));
    if let Some(proxy_url) = proxy_url.filter(|value| !value.trim().is_empty()) {
        builder = builder.proxy(reqwest::Proxy::all(proxy_url)?);
    }
    Ok(builder.build()?)
}

/// Send a streaming request but bound only the wait for the upstream response
/// headers. Once headers arrive, the response body can stream for a long time
/// without being killed by a total request timeout.
pub(crate) async fn send_stream_request(
    request: reqwest::RequestBuilder,
    timeout_seconds: u64,
) -> GatewayResult<reqwest::Response> {
    let header_timeout_s = timeout_seconds.clamp(5, 60);
    match tokio::time::timeout(
        std::time::Duration::from_secs(header_timeout_s),
        request.send(),
    )
    .await
    {
        Ok(result) => Ok(result?),
        Err(_) => Err(GatewayError::Timeout(format!(
            "upstream stream response headers timed out after {header_timeout_s}s"
        ))),
    }
}

/// Factory: create a provider instance from configuration.
pub fn create_provider(
    name: &str,
    config: &ProviderConfig,
) -> crate::error::GatewayResult<BoxedProvider> {
    match config.provider_type {
        crate::config::ProviderType::GithubModels
        | crate::config::ProviderType::Gemini
        | crate::config::ProviderType::HuggingFace
        | crate::config::ProviderType::Nvidia
        | crate::config::ProviderType::OpenaiCompatible => Ok(Box::new(
            openai_compatible::OpenAiCompatibleProvider::new(name, config),
        )),
        crate::config::ProviderType::Ollama => {
            Ok(Box::new(ollama::OllamaProvider::new(name, config)))
        }
        crate::config::ProviderType::Cloudflare => {
            Ok(Box::new(cloudflare::CloudflareProvider::new(name, config)))
        }
    }
}
