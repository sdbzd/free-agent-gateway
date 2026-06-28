/// Provider abstraction and implementations.
///
/// Each provider (GitHub Models, NVIDIA NIM, OpenAI-compatible, Ollama)
/// implements the `Provider` trait.
pub mod github_models;
pub mod nvidia;
pub mod ollama;
pub mod openai_compatible;
pub mod traits;

pub use traits::{BoxedProvider, Provider};

use crate::config::ProviderConfig;

/// Factory: create a provider instance from configuration.
pub fn create_provider(
    name: &str,
    config: &ProviderConfig,
) -> crate::error::GatewayResult<BoxedProvider> {
    match config.provider_type {
        crate::config::ProviderType::GithubModels
        | crate::config::ProviderType::Nvidia
        | crate::config::ProviderType::OpenaiCompatible => Ok(Box::new(
            openai_compatible::OpenAiCompatibleProvider::new(name, config),
        )),
        crate::config::ProviderType::Ollama => {
            Ok(Box::new(ollama::OllamaProvider::new(name, config)))
        }
    }
}
