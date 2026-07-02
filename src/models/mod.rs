/// Data models for the free-agent-gateway.
///
/// Includes: OpenAI-compatible request/response types, health state, key state, etc.
use serde::{Deserialize, Serialize};

use crate::config::KeyTier;

// ─── OpenAI-compatible Chat Completion Request ───────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<StopToken>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Internal field set by the gateway for request tracking. Never sent upstream.
    #[serde(default, skip)]
    pub request_id: Option<String>,
    /// Internal field set by the gateway to track agent context.
    #[serde(skip)]
    pub agent_name: Option<String>,
    /// All other OpenAI-compatible fields (tools, tool_choice, response_format, seed, logprobs, etc.) are captured here and forwarded verbatim to the upstream provider. This keeps the gateway a faithful pass-through: agent frameworks rely on these fields, and dropping them would silently break function calling and structured output.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum StopToken {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Forward any other message fields verbatim (e.g. refusal, annotations, audio).
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

// ─── Vision / Content Parts ────────────────────────────────────────

/// A single content part in a message (text or image_url).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum ContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrl },
}

/// Image URL details for vision requests.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ImageUrl {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Check if a chat message contains image content (vision input).
pub fn message_has_vision(msg: &ChatMessage) -> bool {
    match &msg.content {
        serde_json::Value::Array(parts) => parts.iter().any(|part| {
            part.get("type")
                .and_then(|t| t.as_str())
                .map(|t| t == "image_url")
                .unwrap_or(false)
        }),
        _ => false,
    }
}

/// Check if any message in a request contains image content.
pub fn request_has_vision(messages: &[ChatMessage]) -> bool {
    messages.iter().any(message_has_vision)
}

/// Extract total token counts from a chat completion response body.
pub fn extract_usage(body: &serde_json::Value) -> (Option<u32>, Option<u32>) {
    let usage = body.get("usage");
    let prompt = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);
    let completion = usage
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);
    (prompt, completion)
}

pub fn estimate_text_tokens(text: &str) -> u32 {
    let chars = text.chars().count() as u32;
    if chars == 0 { 0 } else { chars.div_ceil(4) }
}

pub fn content_to_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                if let Some(text) = part.get("text").and_then(|value| value.as_str()) {
                    Some(text.to_string())
                } else if part.get("type").and_then(|value| value.as_str()) == Some("input_text") {
                    part.get("input_text")
                        .and_then(|value| value.as_str())
                        .map(str::to_string)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub fn estimate_message_tokens(messages: &[ChatMessage]) -> u32 {
    messages
        .iter()
        .map(|message| {
            let mut total = estimate_text_tokens(&content_to_text(&message.content));
            if let Some(tool_calls) = &message.tool_calls {
                total += estimate_text_tokens(&tool_calls.to_string());
            }
            if let Some(tool_call_id) = &message.tool_call_id {
                total += estimate_text_tokens(tool_call_id);
            }
            total
        })
        .sum()
}

pub fn compress_large_tool_context<F>(
    mut request: ChatCompletionRequest,
    min_message_tokens: u32,
    mut compressor: F,
) -> ChatCompletionRequest
where
    F: FnMut(&str) -> Option<String>,
{
    for message in &mut request.messages {
        if message.role != "tool" {
            continue;
        }
        let text = content_to_text(&message.content);
        if estimate_text_tokens(&text) < min_message_tokens {
            continue;
        }
        let Some(compacted) = compressor(&text).filter(|value| !value.trim().is_empty()) else {
            continue;
        };
        message.content =
            serde_json::Value::String(format!("[Compressed tool output]\n{compacted}"));
    }
    request
}

pub fn response_has_useful_output(body: &serde_json::Value) -> bool {
    body.get("choices")
        .and_then(|choices| choices.as_array())
        .map(|choices| {
            choices.iter().any(|choice| {
                let Some(message) = choice.get("message") else {
                    return false;
                };
                message
                    .get("content")
                    .map(content_to_text)
                    .map(|text| !text.trim().is_empty())
                    .unwrap_or(false)
                    || message
                        .get("tool_calls")
                        .and_then(|value| value.as_array())
                        .map(|calls| calls.iter().any(tool_call_is_useful))
                        .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn tool_call_is_useful(call: &serde_json::Value) -> bool {
    call.get("function")
        .and_then(|function| function.get("name"))
        .and_then(|name| name.as_str())
        .map(|name| !name.trim().is_empty())
        .unwrap_or(false)
}

pub fn repair_tool_call_arguments(body: &mut serde_json::Value) {
    let Some(choices) = body
        .get_mut("choices")
        .and_then(|value| value.as_array_mut())
    else {
        return;
    };
    for choice in choices {
        let Some(tool_calls) = choice
            .get_mut("message")
            .and_then(|message| message.get_mut("tool_calls"))
            .and_then(|value| value.as_array_mut())
        else {
            continue;
        };
        for call in tool_calls {
            let Some(arguments) = call
                .get_mut("function")
                .and_then(|function| function.get_mut("arguments"))
            else {
                continue;
            };
            if arguments.is_string() {
                continue;
            }
            let repaired = if arguments.is_null() {
                "{}".to_string()
            } else {
                arguments.to_string()
            };
            *arguments = serde_json::Value::String(repaired);
        }
    }
}

pub fn extract_usage_or_estimate(
    body: &serde_json::Value,
    prompt_estimate: u32,
) -> (Option<u32>, Option<u32>, bool) {
    let (prompt, completion) = extract_usage(body);
    if prompt.is_some() || completion.is_some() {
        return (prompt, completion, true);
    }

    let completion_estimate = body
        .get("choices")
        .and_then(|choices| choices.as_array())
        .map(|choices| {
            choices
                .iter()
                .map(|choice| {
                    let Some(message) = choice.get("message") else {
                        return 0;
                    };
                    let mut total = message
                        .get("content")
                        .map(content_to_text)
                        .map(|text| estimate_text_tokens(&text))
                        .unwrap_or(0);
                    if let Some(tool_calls) = message.get("tool_calls") {
                        total += estimate_text_tokens(&tool_calls.to_string());
                    }
                    total
                })
                .sum()
        })
        .unwrap_or(0);

    (Some(prompt_estimate), Some(completion_estimate), false)
}

// ─── OpenAI-compatible Chat Completion Response ─────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub system_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatChoice {
    pub index: usize,
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

// ─── SSE Stream Chunk ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChunkChoice>,
    pub system_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChunkChoice {
    pub index: usize,
    pub delta: ChunkDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChunkDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

// ─── Models List Response (OpenAI-compatible) ────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ModelsResponse {
    pub object: String,
    pub data: Vec<ModelInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub owned_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Context window (token limit) from metadata DB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<i64>,
    /// Whether the model supports vision/image inputs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_vision: Option<bool>,
    /// Whether the model supports tool/function calling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_tools: Option<bool>,
    /// Whether the model supports reasoning (e.g. chain-of-thought).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_reasoning: Option<bool>,
    /// Prompt price per 1M tokens (USD).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing_prompt: Option<f64>,
    /// Completion price per 1M tokens (USD).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing_completion: Option<f64>,
}

// ─── Key Status ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyStatus {
    Available,
    Probing,
    Cooldown,
    RateLimited,
    Disabled,
}

impl std::fmt::Display for KeyStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Available => write!(f, "available"),
            Self::Probing => write!(f, "probing"),
            Self::Cooldown => write!(f, "cooldown"),
            Self::RateLimited => write!(f, "rate_limited"),
            Self::Disabled => write!(f, "disabled"),
        }
    }
}

/// Tracking state for a single API key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyState {
    /// The key value (masked for logging).
    #[serde(skip_serializing, default)]
    pub key: String,
    /// Stable identifier used to match persisted metadata to configured keys.
    #[serde(default)]
    pub key_id: String,
    #[serde(default)]
    pub tier: KeyTier,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub models_updated_at: Option<i64>,
    #[serde(default)]
    pub models_last_error: String,
    /// Current status.
    pub status: KeyStatus,
    /// Number of consecutive failures.
    pub fail_count: u32,
    /// Timestamp (epoch seconds) when cooldown expires. None if not in cooldown.
    pub cooldown_until: Option<u64>,
    /// Last successful request time (epoch seconds).
    #[serde(default)]
    pub last_success_at: Option<u64>,
    /// Last failed request time (epoch seconds).
    #[serde(default)]
    pub last_error_at: Option<u64>,
    /// HTTP status for the last failed request.
    #[serde(default)]
    pub last_error_status: Option<u16>,
    /// Last time this key's status changed.
    #[serde(default)]
    pub status_updated_at: Option<u64>,
    /// Last time this key automatically recovered from cooldown/rate-limit.
    #[serde(default)]
    pub last_recovered_at: Option<u64>,
    /// Total successful requests.
    pub success_count: u64,
    /// Total failed requests.
    pub total_fail_count: u64,

    // ─── Rate tracking ──────────────────────────────────────────────
    /// Max requests per minute (None = unlimited).
    #[serde(default)]
    pub rpm_limit: Option<u32>,
    /// Max requests per day (None = unlimited).
    #[serde(default)]
    pub rpd_limit: Option<u32>,
    /// Source that last set the RPM limit.
    #[serde(default)]
    pub rpm_limit_source: Option<String>,
    /// Source that last set the RPD limit.
    #[serde(default)]
    pub rpd_limit_source: Option<String>,
    /// Max prompt tokens per minute (None = unlimited).
    #[serde(default)]
    pub tpm_limit: Option<u32>,
    /// Max tokens per day (None = unlimited).
    #[serde(default)]
    pub tpd_limit: Option<u32>,

    /// Request count in current minute window.
    #[serde(default)]
    pub rpm_count: u32,
    /// Request count in current day window.
    #[serde(default)]
    pub rpd_count: u32,
    /// Prompt tokens used in current minute window.
    #[serde(default)]
    pub tpm_prompt_count: u32,
    /// Completion tokens used in current minute window.
    #[serde(default)]
    pub tpm_completion_count: u32,
    /// Prompt tokens used today.
    #[serde(default)]
    pub tpd_prompt_count: u32,
    /// Completion tokens used today.
    #[serde(default)]
    pub tpd_completion_count: u32,

    /// Epoch second when the current RPM window started.
    #[serde(default)]
    pub rpm_window_start: u64,
    /// Epoch day (seconds / 86400) when the current RPD window started.
    #[serde(default)]
    pub rpd_window_start: u64,
}

impl KeyState {
    /// Check if this key has exceeded any rate limit based on current time.
    pub fn is_rate_limited(&self, now_secs: u64) -> bool {
        let now_day = now_secs / 86400;

        let now_min = now_secs / 60;

        // Check RPM
        if let Some(limit) = self.rpm_limit
            && self.rpm_window_start == now_min
            && self.rpm_count >= limit
        {
            return true;
        }

        // Check RPD
        if let Some(limit) = self.rpd_limit
            && self.rpd_window_start == now_day
            && self.rpd_count >= limit
        {
            return true;
        }

        // Check TPM (tokens per minute)
        if let Some(limit) = self.tpm_limit
            && self.rpm_window_start == now_min
            && (self.tpm_prompt_count + self.tpm_completion_count) >= limit
        {
            return true;
        }

        // Check TPD (tokens per day)
        if let Some(limit) = self.tpd_limit
            && self.rpd_window_start == now_day
            && (self.tpd_prompt_count + self.tpd_completion_count) >= limit
        {
            return true;
        }

        false
    }

    /// Reset rate counters if the window has expired.
    pub fn reset_rate_windows(&mut self, now_secs: u64) {
        let now_min = now_secs / 60;
        let now_day = now_secs / 86400;

        if self.rpm_window_start != now_min {
            self.rpm_window_start = now_min;
            self.rpm_count = 0;
            self.tpm_prompt_count = 0;
            self.tpm_completion_count = 0;
        }

        if self.rpd_window_start != now_day {
            self.rpd_window_start = now_day;
            self.rpd_count = 0;
            self.tpd_prompt_count = 0;
            self.tpd_completion_count = 0;
        }
    }
}

impl KeyState {
    pub fn new(key: String) -> Self {
        Self::with_tier(key, KeyTier::Unknown)
    }

    pub fn with_tier(key: String, tier: KeyTier) -> Self {
        let key_id = crate::keyhub::key_fingerprint(&key);
        let now = chrono::Utc::now().timestamp() as u64;
        Self {
            key,
            key_id,
            tier,
            models: Vec::new(),
            models_updated_at: None,
            models_last_error: String::new(),
            status: KeyStatus::Available,
            fail_count: 0,
            cooldown_until: None,
            last_success_at: None,
            last_error_at: None,
            last_error_status: None,
            status_updated_at: Some(now),
            last_recovered_at: None,
            success_count: 0,
            total_fail_count: 0,
            rpm_limit: None,
            rpd_limit: None,
            rpm_limit_source: None,
            rpd_limit_source: None,
            tpm_limit: None,
            tpd_limit: None,
            rpm_count: 0,
            rpd_count: 0,
            tpm_prompt_count: 0,
            tpm_completion_count: 0,
            tpd_prompt_count: 0,
            tpd_completion_count: 0,
            rpm_window_start: now / 60,
            rpd_window_start: now / 86400,
        }
    }

    /// Mask the key for safe display.
    pub fn masked_key(&self) -> String {
        if self.key.len() <= 8 {
            return "****".into();
        }
        let len = self.key.len();
        format!("{}...{}", &self.key[..4], &self.key[len - 4..])
    }
}

// ─── Health State ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthState {
    pub provider: String,
    pub status: String,
    pub latency_ms: u64,
    pub success_count: u64,
    pub fail_count: u64,
    pub last_error: String,
    pub cooldown_until: Option<u64>,
    pub models_count: usize,
    pub available_keys: usize,
    pub total_keys: usize,
}

// ─── Provider Model Entry (for auto-discovery cache) ─────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderModels {
    pub provider: String,
    pub models: Vec<String>,
    pub updated_at: i64,
}

// ─── Gateway Status ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct GatewayStatus {
    pub version: String,
    pub uptime_seconds: u64,
    pub providers: Vec<HealthState>,
    pub total_requests: u64,
    pub total_errors: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tools_field_roundtrip() {
        let input = r#"{
            "model": "test-model",
            "messages": [{"role": "user", "content": "hello"}],
            "tools": [{"type": "function", "function": {"name": "get_weather", "description": "Get weather", "parameters": {"type": "object"}}}],
            "tool_choice": "auto"
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(input).expect("deserialize");
        assert!(
            req.extra.contains_key("tools"),
            "tools should be captured in extra"
        );
        assert!(
            req.extra.contains_key("tool_choice"),
            "tool_choice should be captured in extra"
        );

        let output = serde_json::to_value(&req).expect("serialize");
        assert!(
            output.get("tools").is_some(),
            "tools should survive serialization"
        );
        assert!(
            output.get("tool_choice").is_some(),
            "tool_choice should survive serialization"
        );
        assert!(
            output.get("tool_choice").and_then(|v| v.as_str()) == Some("auto"),
            "tool_choice value should be preserved"
        );
    }

    #[test]
    fn test_tool_call_argument_objects_are_repaired_to_strings() {
        let mut body = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "lookup",
                            "arguments": {"city": "Shanghai"}
                        }
                    }]
                }
            }]
        });

        repair_tool_call_arguments(&mut body);

        let args = body["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap();
        assert_eq!(args, r#"{"city":"Shanghai"}"#);
        assert!(response_has_useful_output(&body));
    }

    #[test]
    fn test_null_tool_call_arguments_are_repaired_to_empty_object_string() {
        let mut body = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "lookup",
                            "arguments": null
                        }
                    }]
                }
            }]
        });

        repair_tool_call_arguments(&mut body);

        assert_eq!(
            body["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{}"
        );
        assert!(response_has_useful_output(&body));
    }

    #[test]
    fn test_empty_tool_call_shell_is_not_useful_output() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{}]
                }
            }]
        });

        assert!(!response_has_useful_output(&body));
    }

    #[test]
    fn test_usage_estimation_marks_tokens_as_estimated() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "12345678"
                }
            }]
        });

        let (prompt, completion, reported) = extract_usage_or_estimate(&body, 7);

        assert_eq!(prompt, Some(7));
        assert_eq!(completion, Some(2));
        assert!(!reported);
    }

    #[test]
    fn compress_large_tool_context_keeps_user_messages_unchanged() {
        let request = ChatCompletionRequest {
            model: "auto".to_string(),
            messages: vec![
                ChatMessage {
                    role: "user".to_string(),
                    content: serde_json::Value::String("x".repeat(80)),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    extra: serde_json::Map::new(),
                },
                ChatMessage {
                    role: "tool".to_string(),
                    content: serde_json::Value::String("log line\n".repeat(20)),
                    name: None,
                    tool_calls: None,
                    tool_call_id: Some("call-1".to_string()),
                    extra: serde_json::Map::new(),
                },
            ],
            temperature: None,
            top_p: None,
            n: None,
            stream: None,
            stop: None,
            max_tokens: None,
            presence_penalty: None,
            frequency_penalty: None,
            user: None,
            request_id: None,
            agent_name: None,
            extra: serde_json::Map::new(),
        };

        let compressed = compress_large_tool_context(request, 10, |text| {
            assert!(text.contains("log line"));
            Some("compact log".to_string())
        });

        assert_eq!(
            compressed.messages[0].content,
            serde_json::Value::String("x".repeat(80))
        );
        assert_eq!(
            compressed.messages[1].content,
            serde_json::Value::String("[Compressed tool output]\ncompact log".to_string())
        );
    }

    #[test]
    fn compress_large_tool_context_uses_original_when_compressor_fails() {
        let request = ChatCompletionRequest {
            model: "auto".to_string(),
            messages: vec![ChatMessage {
                role: "tool".to_string(),
                content: serde_json::Value::String("log line\n".repeat(20)),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                extra: serde_json::Map::new(),
            }],
            temperature: None,
            top_p: None,
            n: None,
            stream: None,
            stop: None,
            max_tokens: None,
            presence_penalty: None,
            frequency_penalty: None,
            user: None,
            request_id: None,
            agent_name: None,
            extra: serde_json::Map::new(),
        };

        let compressed = compress_large_tool_context(request, 10, |_text| None);

        assert_eq!(
            compressed.messages[0].content,
            serde_json::Value::String("log line\n".repeat(20))
        );
    }
}
