/// Models list handler: GET /v1/models
///
/// Returns OpenAI-compatible model list combining aliases and discovered models.
use std::collections::{HashMap, HashSet};

use axum::{extract::State, response::Json};
use serde_json::json;

use crate::AppState;
use crate::metadata::ModelMetaRow;
use crate::models::{ModelInfo, ModelsResponse};

/// GET /v1/models
pub async fn list_models(State(state): State<AppState>) -> Json<ModelsResponse> {
    let disabled_map = state.disabled_models.read();
    let mut model_list = merged_visible_models(&state.config, &state.keyhub, &disabled_map);

    enrich_model_metadata(&mut model_list, state.model_meta.as_ref());

    Json(ModelsResponse {
        object: "list".into(),
        data: model_list,
    })
}

/// GET /admin/models/families
///
/// Browser/admin-only grouped model view. This does not change the
/// OpenAI-compatible `/v1/models` response or chat routing semantics.
pub async fn admin_model_families(State(state): State<AppState>) -> Json<serde_json::Value> {
    let disabled_map = state.disabled_models.read();
    let mut model_list = visible_models(&state.config, &state.keyhub, &disabled_map);
    enrich_model_metadata(&mut model_list, state.model_meta.as_ref());
    Json(model_families_json(&model_list))
}

fn enrich_model_metadata(
    model_list: &mut [ModelInfo],
    model_meta: Option<&crate::metadata::ModelMetaStore>,
) {
    let Some(meta) = model_meta else {
        return;
    };
    let Ok(rows) = meta.list_models(None) else {
        return;
    };
    let meta_lookup: HashMap<(String, String), ModelMetaRow> = rows
        .into_iter()
        .map(|row| ((row.provider.clone(), row.model_id.clone()), row))
        .collect();
    for m in model_list {
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

fn model_families_json(models: &[ModelInfo]) -> serde_json::Value {
    let mut grouped: HashMap<String, Vec<&ModelInfo>> = HashMap::new();
    for model in models {
        let (family_id, _) = model_family_id(&model.id);
        grouped.entry(family_id).or_default().push(model);
    }

    let mut families: Vec<serde_json::Value> = grouped
        .into_iter()
        .map(|(family_id, mut variants)| {
            variants.sort_by(|left, right| {
                let left_provider = left.provider.as_deref().unwrap_or(&left.owned_by);
                let right_provider = right.provider.as_deref().unwrap_or(&right.owned_by);
                left_provider
                    .cmp(right_provider)
                    .then(left.id.cmp(&right.id))
            });
            let providers: HashSet<String> = variants
                .iter()
                .map(|model| {
                    model
                        .provider
                        .clone()
                        .unwrap_or_else(|| model.owned_by.clone())
                })
                .collect();
            let tiers: HashSet<String> = variants
                .iter()
                .map(|model| model_family_id(&model.id).1)
                .collect();
            let max_context_window = variants
                .iter()
                .filter_map(|model| model.context_window)
                .max();
            let capability = |f: fn(&ModelInfo) -> Option<bool>| {
                let known: Vec<bool> = variants.iter().filter_map(|model| f(model)).collect();
                if known.iter().any(|value| *value) {
                    Some(true)
                } else if !known.is_empty() {
                    Some(false)
                } else {
                    None
                }
            };
            json!({
                "id": family_id,
                "object": "model_family",
                "variant_count": variants.len(),
                "provider_count": providers.len(),
                "providers": sorted_strings(providers),
                "tiers": sorted_strings(tiers),
                "max_context_window": max_context_window,
                "supports_vision": capability(|model| model.supports_vision),
                "supports_tools": capability(|model| model.supports_tools),
                "supports_reasoning": capability(|model| model.supports_reasoning),
                "variants": variants.into_iter().map(model_variant_json).collect::<Vec<_>>(),
            })
        })
        .collect();
    families.sort_by(|left, right| {
        right["variant_count"]
            .as_u64()
            .cmp(&left["variant_count"].as_u64())
            .then_with(|| {
                left["id"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(right["id"].as_str().unwrap_or_default())
            })
    });
    let multi_variant_count = families
        .iter()
        .filter(|family| family["variant_count"].as_u64().unwrap_or(0) > 1)
        .count();
    let cross_provider_count = families
        .iter()
        .filter(|family| family["provider_count"].as_u64().unwrap_or(0) > 1)
        .count();
    json!({
        "object": "list",
        "summary": {
            "total_models": models.len(),
            "total_families": families.len(),
            "multi_variant_families": multi_variant_count,
            "cross_provider_families": cross_provider_count,
            "grouping_rule": "strip_openrouter_pricing_suffix_only",
        },
        "families": families,
    })
}

fn model_variant_json(model: &ModelInfo) -> serde_json::Value {
    let provider = model
        .provider
        .clone()
        .unwrap_or_else(|| model.owned_by.clone());
    let (_, tier) = model_family_id(&model.id);
    json!({
        "id": model.id,
        "object": model.object,
        "provider": provider,
        "owned_by": model.owned_by,
        "tier": tier,
        "context_window": model.context_window,
        "supports_vision": model.supports_vision,
        "supports_tools": model.supports_tools,
        "supports_reasoning": model.supports_reasoning,
        "pricing_prompt": model.pricing_prompt,
        "pricing_completion": model.pricing_completion,
    })
}

fn model_family_id(model_id: &str) -> (String, String) {
    for suffix in [":free", ":paid", ":extended"] {
        if let Some(base) = model_id.strip_suffix(suffix) {
            return (base.to_string(), suffix.trim_start_matches(':').to_string());
        }
    }
    (model_id.to_string(), "default".to_string())
}

fn sorted_strings(values: HashSet<String>) -> Vec<String> {
    let mut values: Vec<String> = values.into_iter().collect();
    values.sort();
    values
}

fn merged_visible_models(
    config: &crate::config::Config,
    keyhub: &crate::keyhub::KeyHub,
    disabled_models: &std::collections::HashMap<String, HashSet<String>>,
) -> Vec<ModelInfo> {
    let models = visible_models(config, keyhub, disabled_models);
    merge_model_families(models)
}

pub(crate) fn merge_model_families(models: Vec<ModelInfo>) -> Vec<ModelInfo> {
    let mut grouped: HashMap<String, Vec<ModelInfo>> = HashMap::new();
    for model in models {
        let (family_id, _) = model_family_id(&model.id);
        grouped.entry(family_id).or_default().push(model);
    }

    let mut merged: Vec<ModelInfo> = grouped
        .into_iter()
        .map(|(family_id, mut variants)| {
            variants.sort_by(|left, right| {
                model_variant_rank(left)
                    .cmp(&model_variant_rank(right))
                    .then(left.id.cmp(&right.id))
                    .then(left.owned_by.cmp(&right.owned_by))
            });
            let mut chosen = variants.remove(0);
            chosen.id = family_id;
            chosen
        })
        .collect();
    merged.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then(left.owned_by.cmp(&right.owned_by))
    });
    merged.dedup_by(|left, right| left.id == right.id);
    merged
}

fn model_variant_rank(model: &ModelInfo) -> (u8, String) {
    let (_, tier) = model_family_id(&model.id);
    let provider = model
        .provider
        .clone()
        .unwrap_or_else(|| model.owned_by.clone());
    let tier_rank = match tier.as_str() {
        "default" => 0,
        "free" => 1,
        "extended" => 2,
        "paid" => 3,
        _ => 4,
    };
    (tier_rank, provider)
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::time::Instant;

    use async_trait::async_trait;
    use axum::extract::State;
    use dashmap::DashMap;
    use parking_lot::Mutex as ParkingMutex;

    use super::list_models;
    use super::merge_model_families;
    use super::model_families_json;
    use super::model_family_id;
    use super::visible_models;
    use crate::AppState;
    use crate::config::{
        Config, CorsConfig, KeyConfig, KeyTier, ModelAlias, ProviderConfig, ProviderType,
        RoutingConfig, RoutingStrategy, ServerConfig, StateConfig, WatcherConfig,
    };
    use crate::error::GatewayResult;
    use crate::health::HealthRegistry;
    use crate::keyhub::KeyHub;
    use crate::models::ChatCompletionRequest;
    use crate::models::ModelInfo;
    use crate::providers::traits::{ChatResponse, Provider, StreamResponse};
    use crate::router::Router;
    use crate::state::PersistedState;

    #[derive(Debug)]
    struct CountingProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Provider for CountingProvider {
        fn name(&self) -> &str {
            "cached"
        }

        fn base_url(&self) -> &str {
            "http://cached"
        }

        async fn list_models(&self, _api_key: &str) -> GatewayResult<Vec<String>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(vec!["fresh-model".into()])
        }

        async fn chat(
            &self,
            _api_key: &str,
            _request: ChatCompletionRequest,
        ) -> GatewayResult<ChatResponse> {
            unreachable!()
        }

        async fn chat_stream(
            &self,
            _api_key: &str,
            _request: ChatCompletionRequest,
        ) -> GatewayResult<StreamResponse> {
            unreachable!()
        }

        async fn health_check(&self, _api_key: &str) -> GatewayResult<u64> {
            Ok(1)
        }

        fn health_check_model(&self) -> &str {
            "cached-model"
        }

        fn timeout_seconds(&self) -> u64 {
            1
        }

        fn priority(&self) -> u32 {
            0
        }
    }

    fn base_config() -> Config {
        Config {
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
            fallback: vec!["cached".into()],
            agents: HashMap::new(),
            models: HashMap::new(),
            model_fallbacks: HashMap::new(),
            providers: HashMap::from([(
                "cached".into(),
                ProviderConfig {
                    provider_type: ProviderType::OpenaiCompatible,
                    enabled: true,
                    base_url: "http://cached".into(),
                    proxy_url: None,
                    keys: vec![KeyConfig::detailed("cached-key", KeyTier::Free)],
                    health_check_model: "cached-model".into(),
                    timeout_seconds: 5,
                    priority: 0,
                },
            )]),
            watcher: WatcherConfig::default(),
            state: StateConfig::default(),
            cors: CorsConfig::default(),
            adaptive_routing: Default::default(),
            context_compression: Default::default(),
            logging: Default::default(),
        }
    }

    #[tokio::test]
    async fn list_models_uses_cached_inventory_without_provider_discovery() {
        let config = Arc::new(base_config());
        let providers = Arc::new(DashMap::new());
        let calls = Arc::new(AtomicUsize::new(0));
        providers.insert(
            "cached".into(),
            Box::new(CountingProvider {
                calls: calls.clone(),
            }) as crate::providers::BoxedProvider,
        );
        let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
        keyhub.register_provider("cached", config.providers["cached"].keys.clone());
        keyhub.update_models("cached", "cached-key", vec!["cached-model".into()]);
        let disabled_models = Arc::new(parking_lot::RwLock::new(
            HashMap::<String, HashSet<String>>::new(),
        ));
        let router = Arc::new(Router::new(
            config.clone(),
            providers.clone(),
            keyhub.clone(),
            disabled_models.clone(),
            None,
        ));
        let (sse_tx, _) = tokio::sync::broadcast::channel::<String>(16);
        let state = AppState {
            config,
            state: Arc::new(parking_lot::RwLock::new(PersistedState::new())),
            http_client: reqwest::Client::new(),
            providers,
            keyhub,
            router,
            health_registry: Arc::new(HealthRegistry::new()),
            request_counter: Arc::new(AtomicU64::new(0)),
            error_counter: Arc::new(AtomicU64::new(0)),
            start_time: Instant::now(),
            sse_tx,
            disabled_models,
            model_meta: None,
            _sync_handle: Arc::new(ParkingMutex::new(None)),
        };

        let response = list_models(State(state)).await;

        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(response.data.len(), 1);
        assert_eq!(response.data[0].id, "cached-model");
    }

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
            model_fallbacks: HashMap::new(),
            providers: HashMap::from([(
                "shared".into(),
                ProviderConfig {
                    provider_type: ProviderType::OpenaiCompatible,
                    enabled: true,
                    base_url: "http://example".into(),
                    proxy_url: None,
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
            adaptive_routing: Default::default(),
            context_compression: Default::default(),
            logging: Default::default(),
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
            model_fallbacks: HashMap::new(),
            providers: HashMap::from([
                (
                    "first".into(),
                    ProviderConfig {
                        provider_type: ProviderType::OpenaiCompatible,
                        enabled: true,
                        base_url: "http://first.example".into(),
                        proxy_url: None,
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
                        proxy_url: None,
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
            adaptive_routing: Default::default(),
            context_compression: Default::default(),
            logging: Default::default(),
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

    #[test]
    fn model_families_group_openrouter_suffix_without_collapsing_variants() {
        let models = vec![
            ModelInfo {
                id: "nvidia/nemotron-3-super-120b-a12b".into(),
                object: "model".into(),
                created: 1,
                owned_by: "nvidia".into(),
                provider: Some("nvidia".into()),
                context_window: None,
                supports_vision: None,
                supports_tools: None,
                supports_reasoning: None,
                pricing_prompt: None,
                pricing_completion: None,
            },
            ModelInfo {
                id: "nvidia/nemotron-3-super-120b-a12b:free".into(),
                object: "model".into(),
                created: 1,
                owned_by: "openrouter".into(),
                provider: Some("openrouter".into()),
                context_window: Some(1_000_000),
                supports_vision: Some(false),
                supports_tools: Some(true),
                supports_reasoning: Some(true),
                pricing_prompt: Some(0.0),
                pricing_completion: Some(0.0),
            },
        ];

        let grouped = model_families_json(&models);
        assert_eq!(grouped["summary"]["total_models"], 2);
        assert_eq!(grouped["summary"]["total_families"], 1);
        assert_eq!(grouped["summary"]["cross_provider_families"], 1);
        let family = &grouped["families"][0];
        assert_eq!(family["id"], "nvidia/nemotron-3-super-120b-a12b");
        assert_eq!(family["variant_count"], 2);
        assert_eq!(family["provider_count"], 2);
        assert_eq!(family["tiers"][0], "default");
        assert_eq!(family["tiers"][1], "free");
        assert_eq!(
            family["variants"][1]["id"],
            "nvidia/nemotron-3-super-120b-a12b:free"
        );
    }

    #[test]
    fn merge_model_families_collapses_pricing_suffixes_for_model_list() {
        let models = vec![
            ModelInfo {
                id: "nvidia/nemotron-3-super-120b-a12b:free".into(),
                object: "model".into(),
                created: 1,
                owned_by: "openrouter".into(),
                provider: Some("openrouter".into()),
                context_window: Some(1_000_000),
                supports_vision: None,
                supports_tools: Some(true),
                supports_reasoning: Some(true),
                pricing_prompt: Some(0.0),
                pricing_completion: Some(0.0),
            },
            ModelInfo {
                id: "nvidia/nemotron-3-super-120b-a12b".into(),
                object: "model".into(),
                created: 1,
                owned_by: "nvidia".into(),
                provider: Some("nvidia".into()),
                context_window: None,
                supports_vision: None,
                supports_tools: None,
                supports_reasoning: None,
                pricing_prompt: None,
                pricing_completion: None,
            },
        ];

        let merged = merge_model_families(models);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].id, "nvidia/nemotron-3-super-120b-a12b");
        assert_eq!(merged[0].provider.as_deref(), Some("nvidia"));
    }

    #[test]
    fn model_family_id_only_strips_openrouter_pricing_suffixes() {
        assert_eq!(
            model_family_id("qwen/qwen3-next-80b-a3b-instruct:free"),
            (
                "qwen/qwen3-next-80b-a3b-instruct".to_string(),
                "free".to_string()
            )
        );
        assert_eq!(
            model_family_id("deepseek-v4-flash-free"),
            ("deepseek-v4-flash-free".to_string(), "default".to_string())
        );
    }
}
