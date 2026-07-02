/// Chat completions handler: POST /v1/chat/completions
///
/// Supports both streaming (SSE) and non-streaming responses.
/// Routes through the router with automatic fallback.
use axum::{
    Json,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use futures::StreamExt;
use std::time::Instant;

use crate::AppState;
use crate::api::SECURITY_REDACT_HEADERS;
use crate::error::GatewayError;
use crate::models::ChatCompletionRequest;

/// POST /v1/chat/completions
pub async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ChatCompletionRequest>,
) -> Response {
    // Extract agent name from header if present
    let agent_name = headers
        .get("x-agent-name")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let request_id = headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("req-{}", uuid::Uuid::new_v4()));

    let mut request = request;
    request.agent_name = agent_name.clone();
    request.request_id = Some(request_id.clone());
    let started = Instant::now();

    // Log request (with sensitive headers redacted)
    let safe_headers = sanitize_headers(&headers);
    tracing::info!(
        request_id = %request_id,
        model = %request.model,
        agent = ?request.agent_name.as_deref().unwrap_or("default"),
        stream = request.stream.unwrap_or(false),
        "Chat completion request"
    );
    tracing::debug!(
        request_id = %request_id,
        headers = ?safe_headers,
        "Request headers (sanitized)"
    );

    // Increment request counter
    state
        .request_counter
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let is_stream = request.stream.unwrap_or(false);

    let result = if is_stream {
        handle_stream(&state, request, &request_id).await
    } else {
        handle_non_stream(&state, request, &request_id).await
    };

    if result.is_err() {
        state
            .error_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    match result {
        Ok(response) => {
            tracing::info!(
                request_id = %request_id,
                stage = "request_complete",
                elapsed_ms = started.elapsed().as_millis() as u64,
                stream = is_stream,
                "Chat completion request accepted"
            );
            response
        }
        Err(e) => {
            tracing::error!(
                request_id = %request_id,
                stage = "request_complete",
                elapsed_ms = started.elapsed().as_millis() as u64,
                stream = is_stream,
                error_category = e.category(),
                error = %e,
                "Chat completion failed"
            );
            e.into_response()
        }
    }
}

/// Handle a non-streaming chat completion.
async fn handle_non_stream(
    state: &AppState,
    request: ChatCompletionRequest,
    request_id: &str,
) -> Result<Response, GatewayError> {
    let deadline = std::time::Duration::from_secs(state.config.server.request_timeout);
    let result = tokio::time::timeout(deadline, state.router.chat(&request))
        .await
        .map_err(|_| GatewayError::Timeout("Gateway request deadline exceeded".into()))?;

    match result {
        Ok(response) => {
            tracing::info!(request_id = %request_id, "Chat completion completed successfully");
            let resp = Response::builder()
                .status(StatusCode::from_u16(response.status).unwrap_or(StatusCode::OK))
                .header("Content-Type", "application/json")
                .header("X-Request-Id", request_id)
                .body(Body::from(
                    serde_json::to_string(&response.body).unwrap_or_default(),
                ))
                .unwrap();
            Ok(resp)
        }
        Err(e) => Err(e),
    }
}

/// Handle a streaming (SSE) chat completion.
async fn handle_stream(
    state: &AppState,
    request: ChatCompletionRequest,
    request_id: &str,
) -> Result<Response, GatewayError> {
    let stream = state.router.chat_stream(&request).await?;

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(256);

    let req_id = request_id.to_string();
    let error_counter = state.error_counter.clone();
    let keepalive = std::time::Duration::from_secs(state.config.server.sse_keepalive.max(1));
    tokio::spawn(async move {
        let mut stream = stream;
        let mut first = true;
        let mut total_chunks: u64 = 0;

        loop {
            let result = match tokio::time::timeout(keepalive, stream.next()).await {
                Ok(result) => result,
                Err(_) => {
                    if tx
                        .send(Ok(Bytes::from_static(b": keep-alive\n\n")))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
            };

            let Some(result) = result else {
                break;
            };

            match result {
                Ok(bytes) => {
                    total_chunks += 1;
                    if tx.send(Ok(bytes)).await.is_err() {
                        return;
                    }
                    first = false;
                }
                Err(e) => {
                    error_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    tracing::error!(request_id = %req_id, error = %e, "Stream error");
                    let error_body = serde_json::json!({
                        "error": {
                            "message": sanitize_error(&e.to_string()),
                            "type": e.category(),
                            "param": serde_json::Value::Null,
                            "code": "stream_error"
                        }
                    });
                    let err_payload =
                        Bytes::from(format!("data: {error_body}\n\ndata: [DONE]\n\n"));
                    let _ = tx.send(Ok(err_payload)).await;
                    break;
                }
            }
        }

        // If we never sent data, send an error
        if first {
            error_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let _ = tx
                .send(Ok(Bytes::from_static(
                    b"data: {\"error\":{\"message\":\"Empty response from provider\",\"type\":\"upstream_error\",\"param\":null,\"code\":\"empty_stream\"}}\n\ndata: [DONE]\n\n",
                )))
                .await;
        }

        tracing::debug!(request_id = %req_id, chunks = total_chunks, "Stream completed");
    });

    let body = Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));

    let response = Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .header("X-Accel-Buffering", "no")
        .header("X-Request-Id", request_id)
        .body(body)
        .unwrap();

    Ok(response)
}

/// Sanitize headers for logging: remove sensitive values.
fn sanitize_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            let name_str = name.as_str().to_lowercase();
            if SECURITY_REDACT_HEADERS.contains(&name_str.as_str()) {
                (name_str, "[REDACTED]".into())
            } else {
                (name_str, value.to_str().unwrap_or("[binary]").to_string())
            }
        })
        .collect()
}

/// Sanitize error messages that might contain keys or tokens.
fn sanitize_error(msg: &str) -> String {
    crate::error::sanitize_diagnostic(msg)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::time::{Duration, Instant};

    use async_trait::async_trait;
    use dashmap::DashMap;
    use parking_lot::Mutex as ParkingMutex;

    use super::handle_non_stream;
    use crate::AppState;
    use crate::config::{
        Config, CorsConfig, KeyConfig, KeyTier, ModelAlias, ProviderConfig, ProviderType,
        RoutingConfig, RoutingStrategy, ServerConfig, StateConfig, WatcherConfig,
    };
    use crate::error::{GatewayError, GatewayResult};
    use crate::health::HealthRegistry;
    use crate::keyhub::KeyHub;
    use crate::models::{ChatCompletionRequest, ChatMessage};
    use crate::providers::traits::{ChatResponse, Provider, StreamResponse};
    use crate::router::Router;
    use crate::state::PersistedState;
    use std::collections::HashSet;

    #[derive(Debug)]
    struct SlowProvider;

    #[async_trait]
    impl Provider for SlowProvider {
        fn name(&self) -> &str {
            "slow"
        }

        fn base_url(&self) -> &str {
            "http://slow"
        }

        async fn list_models(&self, _api_key: &str) -> GatewayResult<Vec<String>> {
            Ok(vec!["slow-model".into()])
        }

        async fn chat(
            &self,
            _api_key: &str,
            _request: ChatCompletionRequest,
        ) -> GatewayResult<ChatResponse> {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(ChatResponse {
                body: serde_json::json!({"id": "too-late"}),
                status: 200,
            })
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
            "slow-model"
        }

        fn timeout_seconds(&self) -> u64 {
            1
        }

        fn priority(&self) -> u32 {
            0
        }
    }

    #[tokio::test]
    async fn non_stream_request_honors_gateway_timeout() {
        let provider_config = ProviderConfig {
            provider_type: ProviderType::OpenaiCompatible,
            enabled: true,
            base_url: "http://slow".into(),
            proxy_url: None,
            keys: vec![KeyConfig::detailed("slow-key", KeyTier::Free)],
            health_check_model: "slow-model".into(),
            timeout_seconds: 1,
            priority: 0,
        };
        let config = Arc::new(Config {
            server: ServerConfig {
                host: "127.0.0.1".into(),
                port: 9000,
                log_level: "info".into(),
                request_timeout: 0,
                sse_keepalive: 15,
            },
            routing: RoutingConfig {
                strategy: RoutingStrategy::LeastFailed,
                fail_threshold: 3,
                cooldown_seconds: 60,
                auto_discover: true,
            },
            fallback: vec!["slow".into()],
            agents: HashMap::new(),
            models: HashMap::from([(
                "slow".into(),
                ModelAlias {
                    provider: "slow".into(),
                    model: "slow-model".into(),
                },
            )]),
            providers: HashMap::from([("slow".into(), provider_config)]),
            watcher: WatcherConfig::default(),
            state: StateConfig::default(),
            cors: CorsConfig::default(),
            adaptive_routing: Default::default(),
            context_compression: Default::default(),
        });
        let providers = Arc::new(DashMap::new());
        providers.insert(
            "slow".into(),
            Box::new(SlowProvider) as crate::providers::BoxedProvider,
        );
        let keyhub = Arc::new(KeyHub::new(config.routing.clone()));
        keyhub.register_provider("slow", config.providers["slow"].keys.clone());
        keyhub.update_models("slow", "slow-key", vec!["slow-model".into()]);
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
        let (sse_tx, _) = tokio::sync::broadcast::channel::<String>(256);
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
        let request = ChatCompletionRequest {
            model: "slow".into(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: serde_json::json!("hello"),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                extra: serde_json::Map::new(),
            }],
            temperature: None,
            top_p: None,
            n: None,
            stream: Some(false),
            stop: None,
            max_tokens: None,
            presence_penalty: None,
            frequency_penalty: None,
            user: None,
            request_id: Some("timeout-test".into()),
            agent_name: None,
            extra: serde_json::Map::new(),
        };

        let result = handle_non_stream(&state, request, "timeout-test").await;

        assert!(matches!(result, Err(GatewayError::Timeout(_))));
    }
}
