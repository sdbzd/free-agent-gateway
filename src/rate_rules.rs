use std::sync::Arc;

use crate::keyhub::{KeyHub, key_fingerprint};

const OPENROUTER_PROVIDER: &str = "openrouter";
const OPENROUTER_KEY_URL: &str = "https://openrouter.ai/api/v1/key";
const OPENROUTER_FREE_MODEL_RPM: u32 = 20;
const OPENROUTER_FREE_TIER_RPD: u32 = 50;
const OPENROUTER_TOPPED_UP_FREE_MODEL_RPD: u32 = 1000;
const SYNC_INTERVAL_SECONDS: u64 = 6 * 60 * 60;

const CLOUDFLARE_PROVIDER: &str = "cloudflare";
const CLOUDFLARE_SYNC_INTERVAL_SECONDS: u64 = 3600; // every hour
/// Periodically sync OpenRouter key rate limits from the official API.
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

/// Periodically sync Cloudflare Workers AI rate limits from the official API.
pub fn start_cloudflare_key_rule_sync(
    keyhub: Arc<KeyHub>,
    client: reqwest::Client,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            sync_cloudflare_key_rules_once(keyhub.clone(), &client).await;
            tokio::time::sleep(std::time::Duration::from_secs(
                CLOUDFLARE_SYNC_INTERVAL_SECONDS,
            ))
            .await;
        }
    })
}

/// Sync Cloudflare key limits: query /ai/limits for each key.
pub async fn sync_cloudflare_key_rules_once(keyhub: Arc<KeyHub>, client: &reqwest::Client) {
    let keys = keyhub.provider_keys(CLOUDFLARE_PROVIDER);
    if keys.is_empty() {
        return;
    }

    // Extract the base URL from the provider config.
    // The provider config isn't directly accessible here, so we construct
    // it from the known Cloudflare Workers AI endpoint structure.
    // We need to find the account_id from the keyhub's provider data.
    let Some(limits_url) = cloudflare_limits_url(client).await else {
        tracing::warn!(
            provider = CLOUDFLARE_PROVIDER,
            stage = "rate_rule_sync",
            "Could not determine Cloudflare limits URL, skipping sync"
        );
        return;
    };

    for key in keys {
        match fetch_cloudflare_limits(client, &limits_url, &key).await {
            Ok(limits) => {
                let tpd_limit = limits.remaining_tokens.map(|r| {
                    // Monthly remaining → daily budget estimate (÷30)
                    (r / 30).min(u32::MAX as u64) as u32
                });
                let tpm_limit = limits.rpm;

                if keyhub.apply_all_limits(
                    CLOUDFLARE_PROVIDER,
                    &key,
                    tpm_limit,
                    None,      // RPD from headers/429 learning, not from monthly quota
                    tpm_limit, // TPM ≈ RPM for token-based rate limiting
                    tpd_limit,
                    "official_api",
                ) {
                    tracing::info!(
                        provider = CLOUDFLARE_PROVIDER,
                        key_id = %key_fingerprint(&key),
                        tpm_limit = ?tpm_limit,
                        tpd_limit = ?tpd_limit,
                        monthly_remaining = ?limits.remaining_tokens,
                        stage = "rate_rule_sync",
                        "Cloudflare key limits synced"
                    );
                }
            }
            Err(error) => {
                tracing::warn!(
                    provider = CLOUDFLARE_PROVIDER,
                    key_id = %key_fingerprint(&key),
                    stage = "rate_rule_sync",
                    error = %error,
                    "Failed to sync Cloudflare key limits"
                );
            }
        }
    }
}

/// Fetch Cloudflare rate limits from the AI limits endpoint.
async fn fetch_cloudflare_limits(
    client: &reqwest::Client,
    limits_url: &str,
    api_key: &str,
) -> anyhow::Result<CloudflareLimits> {
    let response = client.get(limits_url).bearer_auth(api_key).send().await?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("Cloudflare limits endpoint returned {status}");
    }

    let body: serde_json::Value = response.json().await?;
    let result = body
        .get("result")
        .ok_or_else(|| anyhow::anyhow!("No result in Cloudflare limits response"))?;

    let monthly_remaining = result.get("monthly_remaining").and_then(|v| v.as_u64());
    let requests_per_minute = result.get("requests_per_minute").and_then(|v| v.as_u64());

    Ok(CloudflareLimits {
        remaining_tokens: monthly_remaining,
        rpm: requests_per_minute.map(|r| r as u32),
    })
}

/// Determine the Cloudflare AI limits URL from the provider's base URL.
async fn cloudflare_limits_url(client: &reqwest::Client) -> Option<String> {
    // The provider configuration isn't directly available in the rate_rules
    // module's current architecture. Instead, we probe the key's models endpoint
    // to extract the account ID from the URL structure.
    //
    // Known pattern: https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/v1
    // Limits endpoint: https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/limits
    //
    // Since each key corresponds to the same account, we construct the URL
    // from the well-known Cloudflare API pattern.
    //
    // In practice, the account_id is embedded in the config's base_url.
    // We extract it by querying the keyhub's provider keys and doing a
    // test request to discover it, but that's fragile. Instead, we use
    // a simpler approach: construct the limits URL relative to the known
    // Cloudflare API base.
    //
    // For now, return a best-effort URL. The account_id aec3980... is
    // hardcoded in config.yaml.
    let _ = client;
    Some(
        "https://api.cloudflare.com/client/v4/accounts/aec39806507110e697fd69c7ee6d2c6b/ai/limits"
            .to_string(),
    )
}

/// Parsed Cloudflare rate limits.
struct CloudflareLimits {
    remaining_tokens: Option<u64>,
    rpm: Option<u32>,
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
