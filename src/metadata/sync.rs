/// Public catalog sync — fetch model metadata from public internet sources.
///
/// Sources:
/// - OpenRouter `/api/v1/models` — rich metadata (context window, pricing, capabilities)
///
/// Similar to freellmapi's `catalog-sync.ts` but fetching from public APIs
/// instead of a signed private catalog.
use std::sync::Arc;
use std::time::Duration;

use crate::metadata::ModelMetaStore;

/// Sync scheduler that periodically fetches public model catalogs.
pub struct SyncScheduler {
    store: ModelMetaStore,
    http_client: reqwest::Client,
}

impl SyncScheduler {
    pub fn new(store: ModelMetaStore, http_client: reqwest::Client) -> Self {
        Self { store, http_client }
    }

    /// Run a full sync cycle: fetch from all known public sources.
    pub async fn sync_all(&self) {
        tracing::info!("🔄 Starting public model catalog sync...");
        self.sync_openrouter().await;
        // Future: sync_nvidia().await, sync_github_models().await, etc.
        tracing::info!("✅ Public model catalog sync complete");
    }

    /// Fetch models from OpenRouter's public API.
    async fn sync_openrouter(&self) {
        let source = "openrouter";
        tracing::info!("  Syncing from OpenRouter API...");

        let resp = match self
            .http_client
            .get("https://openrouter.ai/api/v1/models")
            .timeout(Duration::from_secs(30))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("  OpenRouter sync failed (HTTP): {e}");
                let _ = self.store.record_sync_error(source, &e.to_string());
                return;
            }
        };

        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("  OpenRouter sync failed (body read): {e}");
                let _ = self.store.record_sync_error(source, &e.to_string());
                return;
            }
        };

        // Parse the response
        let models = match serde_json::from_str::<OpenRouterResponse>(&body) {
            Ok(r) => r.data,
            Err(e) => {
                tracing::warn!("  OpenRouter sync failed (JSON parse): {e}");
                let _ = self.store.record_sync_error(source, &e.to_string());
                return;
            }
        };

        let total = models.len();
        let mut updated = 0i64;

        for model in &models {
            let supports_vision = model
                .architecture
                .as_ref()
                .and_then(|a| a.input_modalities.as_ref())
                .map(|mods| mods.iter().any(|m| m == "image"));

            let supports_tools = model
                .supported_parameters
                .as_ref()
                .map(|params| params.iter().any(|p| p == "tools" || p == "tool_choice"));

            let supports_reasoning = model
                .supported_parameters
                .as_ref()
                .map(|params| params.iter().any(|p| p == "reasoning"));

            let architecture_modality = model
                .architecture
                .as_ref()
                .and_then(|a| a.modality.as_deref());

            let pricing_prompt = model
                .pricing
                .as_ref()
                .and_then(|p| p.prompt.as_deref())
                .and_then(|s| s.parse::<f64>().ok());

            let pricing_completion = model
                .pricing
                .as_ref()
                .and_then(|p| p.completion.as_deref())
                .and_then(|s| s.parse::<f64>().ok());

            // First seen context_length and max_completion_tokens from top_provider
            let context_window = model
                .context_length
                .or_else(|| model.top_provider.as_ref().and_then(|tp| tp.context_length));

            let max_completion_tokens = model
                .top_provider
                .as_ref()
                .and_then(|tp| tp.max_completion_tokens);

            if let Err(e) = self.store.upsert_model(
                "openrouter", // we tag all of these as openrouter source
                &model.id,
                model.name.as_deref(),
                context_window,
                max_completion_tokens,
                supports_vision,
                supports_tools,
                supports_reasoning,
                pricing_prompt,
                pricing_completion,
                architecture_modality,
                model.per_request_limits,
                None, // rpd_limit
                None, // tpm_limit
                None, // tpd_limit
                "public_sync",
            ) {
                tracing::warn!("  Failed to upsert {}: {e}", model.id);
            } else {
                updated += 1;
            }
        }

        tracing::info!("  OpenRouter sync: {total} models found, {updated} upserted");
        let _ = self.store.record_sync(source, total as i64, updated);

        // Also learn from the raw response body for any extra metadata
        self.store.learn_from_models_response("openrouter", &body);
    }

    /// Start the periodic sync loop (runs on a background tokio task).
    pub fn start_background_sync(self: Arc<Self>) {
        tokio::spawn(async move {
            // Initial sync after a short delay (let server settle)
            tokio::time::sleep(Duration::from_secs(10)).await;
            self.sync_all().await;

            // Then every 6 hours
            loop {
                tokio::time::sleep(Duration::from_secs(6 * 60 * 60)).await;
                self.sync_all().await;
            }
        });
    }
}

// ─── OpenRouter API response types (subset) ─────────────────────────

#[derive(serde::Deserialize)]
struct OpenRouterResponse {
    data: Vec<OpenRouterModel>,
}

#[derive(serde::Deserialize)]
struct OpenRouterModel {
    id: String,
    name: Option<String>,
    context_length: Option<i64>,
    per_request_limits: Option<i64>,
    architecture: Option<Architecture>,
    pricing: Option<Pricing>,
    top_provider: Option<TopProvider>,
    supported_parameters: Option<Vec<String>>,
}

#[derive(serde::Deserialize)]
struct Architecture {
    modality: Option<String>,
    input_modalities: Option<Vec<String>>,
}

#[derive(serde::Deserialize)]
struct Pricing {
    prompt: Option<String>,
    completion: Option<String>,
}

#[derive(serde::Deserialize)]
struct TopProvider {
    context_length: Option<i64>,
    max_completion_tokens: Option<i64>,
}
