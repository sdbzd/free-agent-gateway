/// Runtime learning — extract model metadata from provider responses.
///
/// Three learning channels:
/// 1. **Model list responses**: Parse `/v1/models` responses for capabilities
/// 2. **429 error parsing**: Extract rate limits from error response bodies
/// 3. **Usage recording**: Track request success/failure and token consumption
use crate::metadata::ModelMetaStore;

impl ModelMetaStore {
    /// Learn from a provider's `/v1/models` response.
    ///
    /// OpenAI-compatible endpoints return model objects with `id`, `owned_by`,
    /// and sometimes extended metadata. Parse whatever we can get.
    pub fn learn_from_models_response(&self, provider: &str, body: &str) {
        // Try to parse as OpenAI-compatible model list: { object: "list", data: [...] }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
            let data = if let Some(arr) = v.get("data").and_then(|d| d.as_array()) {
                arr
            } else if let Some(arr) = v.as_array() {
                arr
            } else {
                return;
            };

            for item in data {
                let model_id = match item.get("id").and_then(|id| id.as_str()) {
                    Some(id) => id,
                    None => continue,
                };

                // Parse context_length / context_window
                let context_window = item
                    .get("context_length")
                    .or_else(|| item.get("max_context"))
                    .or_else(|| {
                        item.get("top_provider")
                            .and_then(|tp| tp.get("context_length"))
                    })
                    .and_then(|v| v.as_i64());

                let max_completion_tokens = item
                    .get("max_completion_tokens")
                    .or_else(|| {
                        item.get("top_provider")
                            .and_then(|tp| tp.get("max_completion_tokens"))
                    })
                    .and_then(|v| v.as_i64());

                // Vision support from architecture.modality or input_modalities
                let supports_vision = item
                    .get("architecture")
                    .and_then(|a| {
                        a.get("input_modalities")
                            .and_then(|m| m.as_array())
                            .map(|arr| arr.iter().any(|v| v.as_str() == Some("image")))
                    })
                    .or_else(|| {
                        item.get("architecture")
                            .and_then(|a| a.get("modality").and_then(|m| m.as_str()))
                            .map(|m| m.contains("image"))
                    });

                // Tool support from supported_parameters
                let supports_tools = item
                    .get("supported_parameters")
                    .and_then(|p| p.as_array())
                    .map(|arr| {
                        arr.iter().any(|v| {
                            v.as_str() == Some("tools") || v.as_str() == Some("tool_choice")
                        })
                    });

                let supports_reasoning = item
                    .get("supported_parameters")
                    .and_then(|p| p.as_array())
                    .map(|arr| arr.iter().any(|v| v.as_str() == Some("reasoning")));

                // Pricing
                let pricing_prompt = item
                    .get("pricing")
                    .and_then(|p| p.get("prompt"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<f64>().ok());

                let pricing_completion = item
                    .get("pricing")
                    .and_then(|p| p.get("completion"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<f64>().ok());

                // Display name
                let display_name = item
                    .get("name")
                    .or_else(|| item.get("display_name"))
                    .and_then(|v| v.as_str());

                let architecture_modality = item
                    .get("architecture")
                    .and_then(|a| a.get("modality"))
                    .and_then(|v| v.as_str());

                // Rate limits from per_request_limits or top_provider
                let rpm_limit = item.get("per_request_limits").and_then(|v| v.as_i64());

                let rpd_limit = item
                    .get("per_request_limits")
                    .and_then(|v| if v.is_null() { None } else { Some(20i64) });

                if let Err(e) = self.upsert_model(
                    provider,
                    model_id,
                    display_name,
                    context_window,
                    max_completion_tokens,
                    supports_vision,
                    supports_tools,
                    supports_reasoning,
                    pricing_prompt,
                    pricing_completion,
                    architecture_modality,
                    rpm_limit,
                    rpd_limit,
                    None, // tpm_limit
                    None, // tpd_limit
                    "model_list",
                ) {
                    tracing::warn!("Failed to upsert model {provider}/{model_id}: {e}");
                }
            }
        }
    }

    /// Parse a 429 error response for rate limit information.
    ///
    /// Common 429 body patterns:
    /// ```json
    /// {"error": {"message": "Rate limit exceeded. Limit 30000, Requested 33476"}}
    /// {"error": {"message": "Limit 30 RPM, exceeded"}}
    /// ```
    pub fn learn_from_rate_limit(&self, provider: &str, model_id: &str, response_body: &str) {
        let text = response_body.to_lowercase();

        // Try to extract "Limit X, Requested Y" or similar generic patterns.
        if let Some(limit) =
            first_capture_i64(r"(?i)limit\s+(\d+)\s*,?\s*requested\s+\d+", response_body)
        {
            let limit_type = if text.contains("token") || text.contains("tpm") {
                "tpm"
            } else if text.contains("day") || text.contains("daily") || text.contains("rpd") {
                "rpd"
            } else {
                "rpm"
            };
            self.record_learned_limit(provider, model_id, limit_type, limit);
        }

        let typed_patterns = [
            (
                "rpm",
                r"(?i)(\d+)\s*(?:rpm|req/min|requests per minute|requests/minute)",
            ),
            (
                "rpd",
                r"(?i)(\d+)\s*(?:rpd|req/day|requests per day|requests/day)",
            ),
            (
                "rpd",
                r"(?i)(?:daily|per day|day).{0,30}?(?:limit|quota).{0,20}?(\d+)",
            ),
            (
                "tpm",
                r"(?i)(\d+)\s*(?:tpm|tok/min|tokens per minute|tokens/minute)",
            ),
            (
                "tpd",
                r"(?i)(\d+)\s*(?:tpd|tok/day|tokens per day|tokens/day)",
            ),
            (
                "tpd",
                r"(?i)(?:daily|per day|day).{0,30}?(?:token|tokens).{0,30}?(\d+)",
            ),
        ];
        for (limit_type, pattern) in typed_patterns {
            if let Some(limit) = first_capture_i64(pattern, response_body) {
                self.record_learned_limit(provider, model_id, limit_type, limit);
            }
        }

        // Even when a provider does not disclose quota, it often tells us when to retry.
        // Store that as an observed cooldown hint for future adaptive routing.
        if let Some(cooldown) = parse_retry_after_seconds(response_body) {
            self.record_learned_limit(provider, model_id, "cooldown_seconds", cooldown);
        }
    }

    /// Record successful (or failed) request usage.
    pub fn learn_from_request(
        &self,
        provider: &str,
        model_id: &str,
        success: bool,
        prompt_tokens: Option<i64>,
        completion_tokens: Option<i64>,
    ) {
        if let Err(e) = self.record_usage(
            provider,
            model_id,
            success,
            prompt_tokens,
            completion_tokens,
        ) {
            tracing::warn!("Failed to record usage for {provider}/{model_id}: {e}");
        }
    }

    /// Record a failed request with error context.
    ///
    /// Use this from the router whenever a request fails, instead of just
    /// calling `learn_from_request(success=false)`.
    pub fn learn_from_failure(
        &self,
        provider: &str,
        model_id: &str,
        error_msg: &str,
        http_status: u16,
    ) {
        let category = Self::classify_error(error_msg, http_status);
        if let Err(e) = self.record_model_error(provider, model_id, category) {
            tracing::warn!("Failed to record error category for {provider}/{model_id}: {e}");
        }
        if http_status == 429 {
            self.learn_from_rate_limit(provider, model_id, error_msg);
        }
        // Also record the usage failure
        self.learn_from_request(provider, model_id, false, None, None);
    }

    fn record_learned_limit(&self, provider: &str, model_id: &str, limit_type: &str, limit: i64) {
        if limit <= 0 {
            return;
        }
        if let Err(e) = self.learn_rate_limit(provider, model_id, limit_type, limit) {
            tracing::warn!(
                "Failed to learn rate limit {provider}/{model_id} {limit_type}={limit}: {e}"
            );
        }
    }
}

fn first_capture_i64(pattern: &str, text: &str) -> Option<i64> {
    regex::Regex::new(pattern)
        .ok()?
        .captures(text)?
        .get(1)?
        .as_str()
        .parse::<i64>()
        .ok()
}

fn parse_retry_after_seconds(text: &str) -> Option<i64> {
    let re = regex::Regex::new(
        r"(?i)(?:retry|try again|reset|available).{0,40}?(\d+)\s*(second|seconds|sec|s|minute|minutes|min|m|hour|hours|hr|h|day|days|d)\b",
    )
    .ok()?;
    let caps = re.captures(text)?;
    let value = caps.get(1)?.as_str().parse::<i64>().ok()?;
    let unit = caps.get(2)?.as_str().to_lowercase();
    let multiplier = match unit.as_str() {
        "second" | "seconds" | "sec" | "s" => 1,
        "minute" | "minutes" | "min" | "m" => 60,
        "hour" | "hours" | "hr" | "h" => 3600,
        "day" | "days" | "d" => 86400,
        _ => return None,
    };
    Some(value.saturating_mul(multiplier))
}
