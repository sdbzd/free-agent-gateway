use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};

use crate::AppState;
use crate::adaptive::{self, AdaptiveScope};
use crate::models::{ChatCompletionRequest, ModelsResponse};

pub async fn adaptive_chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut request): Json<ChatCompletionRequest>,
) -> Response {
    apply_request_headers(&headers, &mut request);
    adaptive_chat_response(&state, AdaptiveScope::Auto, request).await
}

pub async fn adaptive_agent_chat_completions(
    State(state): State<AppState>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    Json(mut request): Json<ChatCompletionRequest>,
) -> Response {
    apply_request_headers(&headers, &mut request);
    request.agent_name = Some(agent.clone());
    adaptive_chat_response(&state, AdaptiveScope::Agent(agent), request).await
}

pub async fn adaptive_provider_chat_completions(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    Json(mut request): Json<ChatCompletionRequest>,
) -> Response {
    apply_request_headers(&headers, &mut request);
    adaptive_chat_response(&state, AdaptiveScope::Provider(provider), request).await
}

pub async fn adaptive_provider_group_chat_completions(
    State(state): State<AppState>,
    Path(group): Path<String>,
    headers: HeaderMap,
    Json(mut request): Json<ChatCompletionRequest>,
) -> Response {
    apply_request_headers(&headers, &mut request);
    adaptive_chat_response(&state, AdaptiveScope::ProviderGroup(group), request).await
}

pub async fn adaptive_models(State(state): State<AppState>) -> Response {
    adaptive_models_response(&state, AdaptiveScope::Auto)
}

pub async fn adaptive_agent_models(
    State(state): State<AppState>,
    Path(agent): Path<String>,
) -> Response {
    adaptive_models_response(&state, AdaptiveScope::Agent(agent))
}

pub async fn adaptive_provider_models(
    State(state): State<AppState>,
    Path(provider): Path<String>,
) -> Response {
    adaptive_models_response(&state, AdaptiveScope::Provider(provider))
}

pub async fn adaptive_provider_group_models(
    State(state): State<AppState>,
    Path(group): Path<String>,
) -> Response {
    adaptive_models_response(&state, AdaptiveScope::ProviderGroup(group))
}

async fn adaptive_chat_response(
    state: &AppState,
    scope: AdaptiveScope,
    request: ChatCompletionRequest,
) -> Response {
    match adaptive::chat(state, scope, request).await {
        Ok(response) => Response::builder()
            .status(StatusCode::from_u16(response.status).unwrap_or(StatusCode::OK))
            .header("Content-Type", "application/json")
            .body(Body::from(
                serde_json::to_string(&response.body).unwrap_or_default(),
            ))
            .unwrap(),
        Err(error) => error.into_response(),
    }
}

fn adaptive_models_response(state: &AppState, scope: AdaptiveScope) -> Response {
    match adaptive::scoped_models(state, &scope) {
        Ok(data) => Json(ModelsResponse {
            object: "list".into(),
            data,
        })
        .into_response(),
        Err(error) => error.into_response(),
    }
}

fn apply_request_headers(headers: &HeaderMap, request: &mut ChatCompletionRequest) {
    if request.agent_name.is_none() {
        request.agent_name = headers
            .get("x-agent-name")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
    }
    request.request_id = headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .or_else(|| Some(format!("req-{}", uuid::Uuid::new_v4())));
}
