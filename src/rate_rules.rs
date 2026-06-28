use std::sync::Arc;

use crate::keyhub::{KeyHub, key_fingerprint};

const OPENROUTER_PROVIDER: &str = "openrouter";
const OPENROUTER_KEY_URL: &str = "https://openrouter.ai/api/v1/key";
const OPENROUTER_FREE_MODEL_RPM: u32 = 20;
const OPENROUTER_FREE_TIER_RPD: u32 = 50;
const OPENROUTER_TOPPED_UP_FREE_MODEL_RPD: u32 = 1000;
const SYNC_INTERVAL_SECONDS: u64 = 6 * 60 * 60;

pub fn start_openrouter_key_rule_sync(
    keyhub: Arc<KeyHub>,
    client: reqwest::Client,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            sync_openrouter_key_rules_once(keyhub.clone(), &client).await;
            tokio::time::sleep(std::time::Duration::from_secs(SYNC_INTERVAL_SECONDS)).await;
        }
    })
}

pub async fn sync_openrouter_key_rules_once(keyhub: Arc<KeyHub>, client: &reqwest::Client) {
    let keys = keyhub.provider_keys(OPENROUTER_PROVIDER);
    if keys.is_empty() {
        return;
    }

    for key in keys {
        match fetch_openrouter_key_rule(client, &key).await {
            Ok(rule) => {
                let rpd_limit = if rule.is_free_tier {
                    OPENROUTER_FREE_TIER_RPD
                } else {
                    OPENROUTER_TOPPED_UP_FREE_MODEL_RPD
                };
                if keyhub.apply_request_limits(
                    OPENROUTER_PROVIDER,
                    &key,
                    Some(OPENROUTER_FREE_MODEL_RPM),
                    Some(rpd_limit),
                    "official_api",
                ) {
                    tracing::info!(
                        provider = OPENROUTER_PROVIDER,
                        key_id = %key_fingerprint(&key),
                        is_free_tier = rule.is_free_tier,
                        rpm_limit = OPENROUTER_FREE_MODEL_RPM,
                        rpd_limit,
                        stage = "rate_rule_sync",
                        "OpenRouter key rule synced"
                    );
                }
            }
            Err(error) => {
                tracing::warn!(
                    provider = OPENROUTER_PROVIDER,
                    key_id = %key_fingerprint(&key),
                    stage = "rate_rule_sync",
                    error = %error,
                    "Failed to sync OpenRouter key rule"
                );
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct OpenRouterKeyRule {
    is_free_tier: bool,
}

async fn fetch_openrouter_key_rule(
    client: &reqwest::Client,
    key: &str,
) -> anyhow::Result<OpenRouterKeyRule> {
    let response = client
        .get(OPENROUTER_KEY_URL)
        .bearer_auth(key)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("OpenRouter key endpoint returned {status}");
    }

    let body: serde_json::Value = response.json().await?;
    let data = body.get("data").unwrap_or(&body);
    let is_free_tier = data
        .get("is_free_tier")
        .and_then(|value| value.as_bool())
        .unwrap_or(true);

    Ok(OpenRouterKeyRule { is_free_tier })
}
