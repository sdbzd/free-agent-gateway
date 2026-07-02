use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use thiserror::Error;

/// Unified error type for the gateway.
#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Provider not found: {0}")]
    ProviderNotFound(String),

    #[error("Model not found: {0}")]
    ModelNotFound(String),

    #[error("No available keys for provider: {0}")]
    NoAvailableKeys(String),

    #[error("All providers failed, including fallbacks")]
    AllProvidersFailed,

    #[error("Upstream request failed: {0}")]
    UpstreamError(String),

    #[error("HTTP error {status}: {message}")]
    HttpError {
        status: u16,
        message: String,
        #[allow(dead_code)]
        retry_after_seconds: Option<u64>,
    },

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("State persistence error: {0}")]
    StateError(String),

    #[error("Rate limited by upstream")]
    RateLimited,

    #[error("Authentication failed with upstream")]
    AuthFailed,

    #[error("Provider disabled: {0}")]
    ProviderDisabled(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("YAML error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("Database error: {0}")]
    Database(String),
}

impl From<rusqlite::Error> for GatewayError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Database(e.to_string())
    }
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        let (status, error_type, message) = match &self {
            Self::Config(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "config_error",
                msg.clone(),
            ),
            Self::ProviderNotFound(msg) => {
                (StatusCode::NOT_FOUND, "provider_not_found", msg.clone())
            }
            Self::ModelNotFound(msg) => (StatusCode::NOT_FOUND, "model_not_found", msg.clone()),
            Self::NoAvailableKeys(msg) => (StatusCode::SERVICE_UNAVAILABLE, "no_keys", msg.clone()),
            Self::AllProvidersFailed => (
                StatusCode::SERVICE_UNAVAILABLE,
                "all_providers_failed",
                "All providers and fallbacks exhausted".into(),
            ),
            Self::UpstreamError(_) => (
                StatusCode::BAD_GATEWAY,
                "upstream_error",
                "Upstream provider request failed".into(),
            ),
            Self::HttpError {
                status, message, ..
            } => (
                StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY),
                "http_error",
                format!("Upstream provider returned HTTP {status}: {message}"),
            ),
            Self::Timeout(msg) => (StatusCode::GATEWAY_TIMEOUT, "timeout", msg.clone()),
            Self::InvalidRequest(msg) => (StatusCode::BAD_REQUEST, "invalid_request", msg.clone()),
            Self::StateError(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "state_error",
                msg.clone(),
            ),
            Self::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "Rate limited by upstream provider".into(),
            ),
            Self::AuthFailed => (
                StatusCode::UNAUTHORIZED,
                "auth_failed",
                "Authentication failed with upstream provider".into(),
            ),
            Self::ProviderDisabled(msg) => {
                (StatusCode::FORBIDDEN, "provider_disabled", msg.clone())
            }
            Self::Serialization(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "serialization_error",
                msg.clone(),
            ),
            Self::Io(_) | Self::Reqwest(_) | Self::Json(_) | Self::Yaml(_) | Self::Database(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                self.to_string(),
            ),
        };

        let body = serde_json::json!({
            "error": {
                "type": error_type,
                "message": message,
                "param": serde_json::Value::Null,
                "code": error_type,
            }
        });

        (status, axum::Json(body)).into_response()
    }
}

impl GatewayError {
    pub fn http_error(
        status: u16,
        message: impl Into<String>,
        retry_after_seconds: Option<u64>,
    ) -> Self {
        Self::HttpError {
            status,
            message: message.into(),
            retry_after_seconds,
        }
    }

    pub fn http_status(&self) -> u16 {
        match self {
            Self::HttpError { status, .. } => *status,
            Self::RateLimited => 429,
            Self::AuthFailed => 401,
            Self::Timeout(_) => 504,
            _ => 500,
        }
    }

    pub fn retry_after_seconds(&self) -> Option<u64> {
        match self {
            Self::HttpError {
                retry_after_seconds: Some(seconds),
                ..
            } => Some(*seconds),
            Self::HttpError { message, .. } => parse_retry_after_seconds(message),
            _ => None,
        }
    }

    pub fn is_auth_failure(&self) -> bool {
        match self {
            Self::AuthFailed => true,
            Self::HttpError { status: 401, .. } => true,
            Self::HttpError {
                status: 403,
                message,
                ..
            } => !is_cloudflare_or_waf_block(message) && !is_model_or_region_forbidden(message),
            _ => false,
        }
    }

    pub fn is_key_attributable_failure(&self) -> bool {
        match self {
            Self::AuthFailed => true,
            Self::HttpError { status: 401, .. } => true,
            Self::HttpError {
                status: 403,
                message,
                ..
            } => {
                !is_cloudflare_or_waf_block(message)
                    && !is_model_or_region_forbidden(message)
                    && looks_like_auth_forbidden(message)
            }
            Self::HttpError { status: 429, .. } => true,
            Self::HttpError { status, .. } => *status >= 500,
            Self::Timeout(_) | Self::Reqwest(_) | Self::UpstreamError(_) => true,
            _ => false,
        }
    }

    pub fn category(&self) -> &'static str {
        match self {
            Self::Config(_) => "config_error",
            Self::ProviderNotFound(_) => "provider_not_found",
            Self::ModelNotFound(_) => "model_not_found",
            Self::NoAvailableKeys(_) => "no_keys",
            Self::AllProvidersFailed => "all_providers_failed",
            Self::UpstreamError(message) if is_empty_response_error(message) => "empty_response",
            Self::UpstreamError(message) if is_malformed_stream_error(message) => {
                "malformed_stream"
            }
            Self::UpstreamError(_) | Self::Reqwest(_) => "upstream_error",
            Self::HttpError { status: 429, .. } => "rate_limited",
            Self::HttpError { status: 401, .. } => "auth_failed",
            Self::HttpError {
                status: 403,
                message,
                ..
            } if is_cloudflare_or_waf_block(message) => "waf_blocked",
            Self::HttpError {
                status: 403,
                message,
                ..
            } if is_region_forbidden(message) => "region_forbidden",
            Self::HttpError {
                status: 403,
                message,
                ..
            } if is_model_forbidden(message) => "model_forbidden",
            Self::HttpError { status: 403, .. } => "auth_failed",
            Self::HttpError { status: 503, .. } => "upstream_error",
            Self::HttpError { .. } => "upstream_http_error",
            Self::Timeout(_) => "timeout",
            Self::InvalidRequest(_) => "invalid_request",
            Self::StateError(_) => "state_error",
            Self::RateLimited => "rate_limited",
            Self::AuthFailed => "auth_failed",
            Self::ProviderDisabled(_) => "provider_disabled",
            Self::Serialization(_) | Self::Json(_) | Self::Yaml(_) => "serialization_error",
            Self::Io(_) => "io_error",
            Self::Database(_) => "database_error",
        }
    }
}

pub fn is_cloudflare_or_waf_block(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("cloudflare")
        || lower.contains("error code: 1009")
        || lower.contains("error code 1009")
        || lower.contains("cf-error")
        || lower.contains("access denied")
        || lower.contains("the owner of this website has banned")
        || lower.contains("waf")
}

pub fn is_model_or_region_forbidden(message: &str) -> bool {
    is_region_forbidden(message) || is_model_forbidden(message)
}

pub fn is_region_forbidden(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("not available in your region")
        || lower.contains("region restricted")
        || lower.contains("region is not supported")
        || lower.contains("not supported in your region")
}

pub fn is_model_forbidden(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("do not have access to model")
        || lower.contains("model is not available")
        || lower.contains("model not available")
        || lower.contains("model is not accessible")
        || lower.contains("model_not_found")
        || lower.contains("does not have access to model")
        || lower.contains("not authorized for model")
}

pub fn is_malformed_stream_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("error decoding response body")
        || lower.contains("incomplete streaming tool call arguments")
        || lower.contains("invalid tool call")
        || lower.contains("malformed sse")
        || lower.contains("malformed stream")
        || lower.contains("invalid streaming")
}

pub fn is_empty_response_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("empty chat completion response")
        || lower.contains("empty streaming chat completion response")
        || lower.contains("empty upstream response")
}

fn looks_like_auth_forbidden(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("invalid api key")
        || lower.contains("invalid token")
        || lower.contains("unauthorized")
        || lower.contains("authentication")
        || lower.contains("permission denied")
        || lower.contains("forbidden")
        || lower.contains("insufficient")
        || lower.contains("quota")
}

/// Convenient Result alias.
pub type GatewayResult<T> = Result<T, GatewayError>;

pub fn parse_retry_after_value(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    if let Ok(seconds) = trimmed.parse::<u64>() {
        return Some(seconds);
    }

    let parsed = chrono::DateTime::parse_from_rfc2822(trimmed)
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(trimmed))
        .ok()?;
    let now = chrono::Utc::now();
    let seconds = parsed.with_timezone(&chrono::Utc) - now;
    Some(seconds.num_seconds().max(0) as u64)
}

pub fn parse_retry_after_seconds(text: &str) -> Option<u64> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text)
        && let Some(seconds) = retry_after_from_json(&value)
    {
        return Some(seconds);
    }

    let patterns = [
        r"(?i)(?:retry|try again|reset|available).{0,40}?(\d+)\s*(second|seconds|sec|s|minute|minutes|min|m|hour|hours|hr|h|day|days|d)\b",
        r"(?i)(\d+)\s*(second|seconds|sec|s|minute|minutes|min|m|hour|hours|hr|h|day|days|d).{0,40}?(?:retry|try again|reset|available)",
    ];

    for pattern in patterns {
        let Ok(regex) = regex::Regex::new(pattern) else {
            continue;
        };
        let Some(captures) = regex.captures(text) else {
            continue;
        };
        let amount = captures.get(1)?.as_str().parse::<u64>().ok()?;
        let unit = captures.get(2)?.as_str().to_ascii_lowercase();
        return Some(match unit.as_str() {
            "s" | "sec" | "second" | "seconds" => amount,
            "m" | "min" | "minute" | "minutes" => amount.saturating_mul(60),
            "h" | "hr" | "hour" | "hours" => amount.saturating_mul(3600),
            "d" | "day" | "days" => amount.saturating_mul(86400),
            _ => continue,
        });
    }

    None
}

fn retry_after_from_json(value: &serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::Object(map) => {
            for field in [
                "retry_after",
                "retry_after_seconds",
                "cooldown_seconds",
                "reset_after",
                "reset_after_seconds",
            ] {
                if let Some(seconds) = map.get(field).and_then(json_retry_after_value) {
                    return Some(seconds);
                }
            }
            map.values().find_map(retry_after_from_json)
        }
        serde_json::Value::Array(items) => items.iter().find_map(retry_after_from_json),
        _ => None,
    }
}

fn json_retry_after_value(value: &serde_json::Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(parse_retry_after_value))
}

pub fn sanitize_diagnostic(message: &str) -> String {
    ["Bearer ", "key=", "token="]
        .into_iter()
        .fold(message.to_string(), |value, marker| {
            redact_marker_value(&value, marker)
        })
}

fn redact_marker_value(input: &str, marker: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(position) = rest.find(marker) {
        let value_start = position + marker.len();
        output.push_str(&rest[..value_start]);
        let value = &rest[value_start..];
        let value_end = value
            .find(|character: char| {
                character.is_whitespace() || matches!(character, ',' | '&' | '"' | '\'' | '}' | ']')
            })
            .unwrap_or(value.len());
        output.push_str("[REDACTED]");
        rest = &value[value_end..];
    }

    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use axum::{body::to_bytes, response::IntoResponse};

    use super::GatewayError;
    use super::sanitize_diagnostic;

    #[tokio::test]
    async fn error_response_uses_openai_compatible_envelope() {
        let response = GatewayError::ModelNotFound("missing".into()).into_response();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["error"]["type"], "model_not_found");
        assert_eq!(json["error"]["code"], "model_not_found");
        assert!(json["error"]["param"].is_null());
        assert!(json["error"]["message"].is_string());
    }

    #[test]
    fn diagnostic_sanitizer_removes_sensitive_values() {
        let sanitized = sanitize_diagnostic(
            "Authorization: Bearer secret-token key=secret-key token=secret-token-2",
        );

        assert!(!sanitized.contains("secret-token"));
        assert!(!sanitized.contains("secret-key"));
        assert!(sanitized.contains("Bearer [REDACTED]"));
        assert!(sanitized.contains("key=[REDACTED]"));
        assert!(sanitized.contains("token=[REDACTED]"));
    }

    #[test]
    fn cloudflare_403_is_not_auth_failure() {
        let error = GatewayError::http_error(
            403,
            "Access denied | api.groq.com used Cloudflare to restrict access. Error code: 1009",
            None,
        );

        assert_eq!(error.category(), "waf_blocked");
        assert!(!error.is_auth_failure());
        assert!(!error.is_key_attributable_failure());
    }

    #[test]
    fn region_model_403_is_not_key_attributable() {
        let error =
            GatewayError::http_error(403, "This model is not available in your region.", None);

        assert_eq!(error.category(), "region_forbidden");
        assert!(!error.is_auth_failure());
        assert!(!error.is_key_attributable_failure());
    }

    #[test]
    fn model_forbidden_403_is_distinct_from_region_forbidden() {
        let error = GatewayError::http_error(403, "You do not have access to model foo.", None);

        assert_eq!(error.category(), "model_forbidden");
        assert!(!error.is_auth_failure());
        assert!(!error.is_key_attributable_failure());
    }

    #[test]
    fn malformed_stream_errors_have_specific_category() {
        let decode = GatewayError::UpstreamError("error decoding response body".into());
        let tool = GatewayError::UpstreamError("incomplete streaming tool call arguments".into());

        assert_eq!(decode.category(), "malformed_stream");
        assert_eq!(tool.category(), "malformed_stream");
    }

    #[test]
    fn empty_upstream_response_has_specific_category() {
        let error = GatewayError::UpstreamError("empty streaming chat completion response".into());

        assert_eq!(error.category(), "empty_response");
    }

    #[test]
    fn plain_403_remains_auth_failure() {
        let error = GatewayError::http_error(403, "invalid api key", None);

        assert_eq!(error.category(), "auth_failed");
        assert!(error.is_auth_failure());
        assert!(error.is_key_attributable_failure());
    }
}
