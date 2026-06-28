/// Models list handler: GET /v1/models
///
/// Returns OpenAI-compatible model list combining aliases and discovered models.
use std::collections::{HashMap, HashSet};

use axum::{extract::State, response::Json};

use crate::AppState;
use crate::error::sanitize_diagnostic;
use crate::metadata::ModelMetaRow;
use crate::models::{ModelInfo, ModelsResponse};

/// GET /v1/models
pub async fn list_models(State(state): State<AppState>) -> Json<ModelsResponse> {
    if state.config.routing.auto_discover {
        let provider_names: Vec<String> = state
            .providers
            .iter()
            .map(|entry| entry.key().clone())
            .collect();

        for provider_name in provider_names {
            let provider_ref = state.providers.get(&provider_name);
            if let Some(provider) = provider_ref {
                for (api_key, tier) in state.keyhub.discovery_keys(&provider_name) {
                    if !state.keyhub.reserve_key(&provider_name, &api_key) {
                        continue;
                    }
                    match provider.list_models(&api_key).await {
                        Ok(models) => {
                            state.keyhub.update_models(&provider_name, &api_key, models);
                            state.keyhub.report_reserved_success(
                                &provider_name,
                                &api_key,
                                None,
                                None,
                            );
                        }
                        Err(error) => {
                            let status = error.http_status();
                            if matches!(status, 401 | 403 | 429) {
                                state.keyhub.report_failure_with_retry_after(
                                    &provider_name,
                                    &api_key,
                                    status,
                                    error.retry_after_seconds(),
                                );
                            }
                            state.keyhub.record_model_error(
                                &provider_name,
                                &api_key,
                                &sanitize_diagnostic(&error.to_string()),
                            );
                            tracing::warn!(
                                provider = %provider_name,
                                key = %crate::keyhub::key_fingerprint(&api_key),
                                tier = %tier,
                                stage = "model_discovery",
                                http_status = status,
                                error_category = error.category(),
                                error = %sanitize_diagnostic(&error.to_string()),
                                "Provider model discovery failed"
                            );
                        }
                    }
                }
            }
        }
    }

    let disabled_map = state.disabled_models.read();
    let mut model_list = visible_models(&state.config, &state.keyhub, &disabled_map);

    // Enrich with metadata from the model knowledge DB if available
    if let Some(ref meta) = state.model_meta
        && let Ok(rows) = meta.list_models(None)
    {
        let meta_lookup: HashMap<(String, String), ModelMetaRow> = rows
            .into_iter()
            .map(|row| ((row.provider.clone(), row.model_id.clone()), row))
            .collect();
        for m in &mut model_list {
            let provider = m.provider.as_deref().unwrap_or(&m.owned_by);
            if let Some(row) = meta_lookup.get(&(provider.to_string(), m.id.clone())) {
                m.context_window = row.context_window;
                m.supports_vision = row.supports_vision;
                m.supports_tools = row.supports_tools;
                m.supports_reasoning = row.supports_reasoning;
                m.pricing_prompt = row.pricing_prompt;
                m.pricing_completion = row.pricing_completion;
            }
        }
    }

    Json(ModelsResponse {
        object: "list".into(),
        data: model_list,
    })
}

fn visible_models(
    config: &crate::config::Config,
    keyhub: &crate::keyhub::KeyHub,
    disabled_models: &std::collections::HashMap<String, HashSet<String>>,
) -> Vec<ModelInfo> {
    let available = keyhub.available_free_models();
    let mut model_list: Vec<ModelInfo> = available
        .iter()
        .filter(|(provider, model)| {
            disabled_models
                .get(provider)
                .map(|set| !set.contains(model))
                .unwrap_or(true)
        })
        .map(|(provider, model)| ModelInfo {
            id: model.clone(),
            object: "model".into(),
            created: chrono::Utc::now().timestamp(),
            owned_by: provider.clone(),
            provider: Some(provider.clone()),
            context_window: None,
            supports_vision: None,
            supports_tools: None,
            supports_reasoning: None,
            pricing_prompt: None,
            pricing_completion: None,
        })
        .collect();

    for (alias_name, alias) in &config.models {
        if let Some((provider, _)) = available.iter().find(|(provider, model)| {
            model == &alias.model && (alias.provider.is_empty() || provider == &alias.provider)
        }) {
            // Also skip alias if the underlying model is disabled
            let alias_disabled = disabled_models
                .get(provider)
                .map(|set| set.contains(&alias.model))
                .unwrap_or(false);
            if alias_disabled {
                continue;
            }
            model_list.push(ModelInfo {
                id: alias_name.clone(),
                object: "model".into(),
                created: chrono::Utc::now().timestamp(),
                owned_by: provider.clone(),
                provider: Some(provider.clone()),
                context_window: None,
                supports_vision: None,
                supports_tools: None,
                supports_reasoning: None,
                pricing_prompt: None,
                pricing_completion: None,
            });
        }
    }
    model_list.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then(left.owned_by.cmp(&right.owned_by))
    });
    model_list.dedup_by(|left, right| left.id == right.id && left.owned_by == right.owned_by);
    model_list
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::visible_models;
    use crate::config::{
        Config, CorsConfig, KeyConfig, KeyTier, ModelAlias, ProviderConfig, ProviderType,
        RoutingConfig, RoutingStrategy, ServerConfig, StateConfig, WatcherConfig,
    };
    use crate::keyhub::KeyHub;

    #[test]
    fn visible_models_exclude_paid_and_unknown_only_models() {
        let config = Config {
            server: ServerConfig {
                host: "127.0.0.1".into(),
                port: 9000,
                log_level: "info".into(),
                request_timeout: 30,
                sse_keepalive: 15,
            },
            routing: RoutingConfig {
                strategy: RoutingStrategy::LeastFailed,
                fail_threshold: 3,
                cooldown_seconds: 60,
                auto_discover: true,
            },
            fallback: vec!["shared".into()],
            agents: HashMap::new(),
            models: HashMap::from([
                (
                    "free-alias".into(),
                    ModelAlias {
                        provider: String::new(),
                        model: "free-model".into(),
                    },
                ),
                (
                    "paid-alias".into(),
                    ModelAlias {
                        provider: String::new(),
                        model: "paid-model".into(),
                    },
                ),
            ]),
            providers: HashMap::from([(
                "shared".into(),
                ProviderConfig {
                    provider_type: ProviderType::OpenaiCompatible,
                    enabled: true,
                    base_url: "http://example".into(),
                    keys: vec![
                        KeyConfig::detailed("free", KeyTier::Free),
                        KeyConfig::detailed("paid", KeyTier::Paid),
                        KeyConfig::Legacy("unknown".into()),
                    ],
                    health_check_model: String::new(),
                    timeout_seconds: 5,
                    priority: 0,
                },
            )]),
            watcher: WatcherConfig::default(),
            state: StateConfig::default(),
            cors: CorsConfig::default(),
        };
        let keyhub = KeyHub::new(config.routing.clone());
        keyhub.register_provider("shared", config.providers["shared"].keys.clone());
        keyhub.update_models("shared", "free", vec!["free-model".into()]);
        keyhub.update_models("shared", "paid", vec!["paid-model".into()]);
        keyhub.update_models("shared", "unknown", vec!["unknown-model".into()]);

        let disabled = HashMap::<String, HashSet<String>>::new();
        let ids: Vec<String> = visible_models(&config, &keyhub, &disabled)
            .into_iter()
            .map(|model| model.id)
            .collect();

        assert_eq!(ids, vec!["free-alias", "free-model"]);
    }

    #[test]
    fn visible_models_respect_alias_provider() {
        let config = Config {
            server: ServerConfig {
                host: "127.0.0.1".into(),
                port: 9000,
                log_level: "info".into(),
                request_timeout: 30,
                sse_keepalive: 15,
            },
            routing: RoutingConfig {
                strategy: RoutingStrategy::LeastFailed,
                fail_threshold: 3,
                cooldown_seconds: 60,
                auto_discover: true,
            },
            fallback: vec!["first".into(), "second".into()],
            agents: HashMap::new(),
            models: HashMap::from([(
                "second-alias".into(),
                ModelAlias {
                    provider: "second".into(),
                    model: "same-model".into(),
                },
            )]),
            providers: HashMap::from([
                (
                    "first".into(),
                    ProviderConfig {
                        provider_type: ProviderType::OpenaiCompatible,
                        enabled: true,
                        base_url: "http://first.example".into(),
                        keys: vec![KeyConfig::detailed("first-key", KeyTier::Free)],
                        health_check_model: String::new(),
                        timeout_seconds: 5,
                        priority: 0,
                    },
                ),
                (
                    "second".into(),
                    ProviderConfig {
                        provider_type: ProviderType::OpenaiCompatible,
                        enabled: true,
                        base_url: "http://second.example".into(),
                        keys: vec![KeyConfig::detailed("second-key", KeyTier::Free)],
                        health_check_model: String::new(),
                        timeout_seconds: 5,
                        priority: 0,
                    },
                ),
            ]),
            watcher: WatcherConfig::default(),
            state: StateConfig::default(),
            cors: CorsConfig::default(),
        };
        let keyhub = KeyHub::new(config.routing.clone());
        keyhub.register_provider("first", config.providers["first"].keys.clone());
        keyhub.register_provider("second", config.providers["second"].keys.clone());
        keyhub.update_models("first", "first-key", vec!["same-model".into()]);
        keyhub.update_models("second", "second-key", vec!["same-model".into()]);

        let disabled = HashMap::<String, HashSet<String>>::new();
        let alias = visible_models(&config, &keyhub, &disabled)
            .into_iter()
            .find(|model| model.id == "second-alias")
            .expect("alias should be visible");

        assert_eq!(alias.owned_by, "second");
        assert_eq!(alias.provider.as_deref(), Some("second"));
    }
}
