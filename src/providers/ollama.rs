use async_trait::async_trait;
use bytes::Bytes;

use crate::config::ProviderConfig;
use crate::error::{GatewayError, GatewayResult, parse_retry_after_value};
use crate::models::ChatCompletionRequest;
use crate::providers::traits::{ChatResponse, Provider, StreamResponse};

/// Ollama provider (local LLM inference).
///
/// Ollama doesn't use Bearer tokens, but we accept a placeholder key.
#[derive(Debug)]
pub struct OllamaProvider {
    name: String,
    base_url: String,
    health_check_model: String,
    timeout_seconds: u64,
    priority: u32,
}

impl OllamaProvider {
    pub fn new(name: &str, config: &ProviderConfig) -> Self {
        Self {
            name: name.to_string(),
            base_url: config.base_url.trim_end_matches('/').to_string(),
            health_check_model: if config.health_check_model.is_empty() {
                "qwen2.5:7b".into()
            } else {
                config.health_check_model.clone()
            },
            timeout_seconds: config.timeout_seconds,
            priority: if config.priority == 0 {
                100
            } else {
                config.priority
            },
        }
    }
}

/// Ollama native request body.
#[derive(Debug, Clone, serde::Serialize)]
struct OllamaChatRequest {
    model: String,
    messages: Vec<serde_json::Value>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<serde_json::Value>,
}

/// Convert OpenAI-format messages to Ollama format.
fn convert_messages(messages: &[crate::models::ChatMessage]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "role": m.role,
                "content": m.content,
            })
        })
        .collect()
}

#[async_trait]
impl Provider for OllamaProvider {
    fn name(&self) -> &str {
        &self.name
    }
    fn base_url(&self) -> &str {
        &self.base_url
    }
    fn health_check_model(&self) -> &str {
        &self.health_check_model
    }
    fn timeout_seconds(&self) -> u64 {
        self.timeout_seconds
    }
    fn priority(&self) -> u32 {
        self.priority
    }

    async fn list_models(&self, _api_key: &str) -> GatewayResult<Vec<String>> {
        let url = format!("{}/api/tags", self.base_url);
        let client = reqwest::Client::new();
        let resp = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(self.timeout_seconds))
            .send()
            .await?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let retry_after_seconds = retry_after_seconds(resp.headers());
            let body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::http_error(status, body, retry_after_seconds));
        }

        let json: serde_json::Value = resp.json().await?;
        let models = json["models"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m["name"].as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        Ok(models)
    }

    async fn chat(
        &self,
        _api_key: &str,
        request: ChatCompletionRequest,
    ) -> GatewayResult<ChatResponse> {
        let ollama_req = OllamaChatRequest {
            model: request.model.clone(),
            messages: convert_messages(&request.messages),
            stream: false,
            options: build_ollama_options(&request),
        };

        let url = format!("{}/api/chat", self.base_url);
        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(self.timeout_seconds))
            .json(&ollama_req)
            .send()
            .await?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let retry_after_seconds = retry_after_seconds(resp.headers());
            let body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::http_error(status, body, retry_after_seconds));
        }

        // Convert Ollama response to OpenAI format
        let ollama_body: serde_json::Value = resp.json().await?;
        let openai_body = convert_ollama_to_openai(&ollama_body, &request.model);

        Ok(ChatResponse {
            body: openai_body,
            status: 200,
        })
    }

    async fn chat_stream(
        &self,
        _api_key: &str,
        request: ChatCompletionRequest,
    ) -> GatewayResult<StreamResponse> {
        let ollama_req = OllamaChatRequest {
            model: request.model.clone(),
            messages: convert_messages(&request.messages),
            stream: true,
            options: build_ollama_options(&request),
        };

        let url = format!("{}/api/chat", self.base_url);
        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(self.timeout_seconds))
            .json(&ollama_req)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let retry_after_seconds = retry_after_seconds(resp.headers());
            let body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::http_error(status, body, retry_after_seconds));
        }

        // Convert Ollama streaming format to OpenAI SSE format
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, GatewayError>>(256);

        tokio::spawn(async move {
            use futures::StreamExt;
            let mut stream = resp.bytes_stream();
            let request_id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
            let created = chrono::Utc::now().timestamp();
            let mut sent_first = false;

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        // Ollama sends NDJSON; try to parse each line
                        for line in text.lines() {
                            let line = line.trim();
                            if line.is_empty() {
                                continue;
                            }
                            if let Ok(ollama_chunk) =
                                serde_json::from_str::<serde_json::Value>(line)
                            {
                                if let Some(content) = ollama_chunk["message"]["content"].as_str() {
                                    let chunk = crate::models::ChatCompletionChunk {
                                        id: request_id.clone(),
                                        object: "chat.completion.chunk".into(),
                                        created,
                                        model: request.model.clone(),
                                        choices: vec![crate::models::ChunkChoice {
                                            index: 0,
                                            delta: crate::models::ChunkDelta {
                                                role: if !sent_first {
                                                    Some("assistant".to_string())
                                                } else {
                                                    None
                                                },
                                                content: Some(content.to_string()),
                                            },
                                            finish_reason: None,
                                        }],
                                        system_fingerprint: None,
                                    };
                                    let sse = format!(
                                        "data: {}\n\n",
                                        serde_json::to_string(&chunk).unwrap_or_default()
                                    );
                                    if tx.send(Ok(Bytes::from(sse))).await.is_err() {
                                        return;
                                    }
                                    sent_first = true;
                                }
                                // Check if this is the final chunk
                                if ollama_chunk
                                    .get("done")
                                    .and_then(|d| d.as_bool())
                                    .unwrap_or(false)
                                {
                                    let final_chunk = crate::models::ChatCompletionChunk {
                                        id: request_id.clone(),
                                        object: "chat.completion.chunk".into(),
                                        created,
                                        model: request.model.clone(),
                                        choices: vec![crate::models::ChunkChoice {
                                            index: 0,
                                            delta: crate::models::ChunkDelta {
                                                role: None,
                                                content: None,
                                            },
                                            finish_reason: Some("stop".into()),
                                        }],
                                        system_fingerprint: None,
                                    };
                                    let sse = format!(
                                        "data: {}\n\ndata: [DONE]\n\n",
                                        serde_json::to_string(&final_chunk).unwrap_or_default()
                                    );
                                    let _ = tx.send(Ok(Bytes::from(sse))).await;
                                    return;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx
                            .send(Err(GatewayError::UpstreamError(e.to_string())))
                            .await;
                        return;
                    }
                }
            }
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        Ok(Box::pin(stream))
    }

    async fn health_check(&self, _api_key: &str) -> GatewayResult<u64> {
        let start = std::time::Instant::now();
        let models = self.list_models("").await?;
        let elapsed = start.elapsed().as_millis() as u64;

        // Ollama may have no models pulled yet; just check it's responsive
        tracing::info!("Ollama health check: {} models found", models.len());
        Ok(elapsed)
    }
}

/// Build Ollama options from OpenAI request parameters.
fn build_ollama_options(request: &ChatCompletionRequest) -> Option<serde_json::Value> {
    let mut opts = serde_json::Map::new();
    if let Some(t) = request.temperature {
        opts.insert("temperature".into(), serde_json::json!(t));
    }
    if let Some(p) = request.top_p {
        opts.insert("top_p".into(), serde_json::json!(p));
    }
    if let Some(n) = request.max_tokens {
        opts.insert("num_predict".into(), serde_json::json!(n));
    }
    if let Some(s) = &request.stop {
        match s {
            crate::models::StopToken::Single(v) => {
                opts.insert("stop".into(), serde_json::json!(vec![v]));
            }
            crate::models::StopToken::Multiple(v) => {
                opts.insert("stop".into(), serde_json::json!(v));
            }
        }
    }
    if opts.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(opts))
    }
}

/// Convert an Ollama response body to OpenAI-compatible format.
fn convert_ollama_to_openai(ollama: &serde_json::Value, model: &str) -> serde_json::Value {
    let content = ollama["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();

    let prompt_tokens = ollama["prompt_eval_count"].as_u64().unwrap_or(0) as u32;
    let completion_tokens = ollama["eval_count"].as_u64().unwrap_or(0) as u32;

    serde_json::json!({
        "id": format!("chatcmpl-{}", uuid::Uuid::new_v4()),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": content,
            },
            "finish_reason": "stop",
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        }
    })
}

fn retry_after_seconds(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after_value)
}
