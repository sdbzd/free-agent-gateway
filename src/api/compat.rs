use axum::{
    Json,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::GatewayError;
use crate::models::{ChatCompletionRequest, ChatMessage, StopToken};

pub async fn responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let request_id = request_id(&headers);
    match responses_to_chat_request(body) {
        Ok(request) if request.stream.unwrap_or(false) => {
            GatewayError::InvalidRequest("Responses streaming is not implemented yet".into())
                .into_response()
        }
        Ok(mut request) => {
            request.request_id = Some(request_id.clone());
            match state.router.chat(&request).await {
                Ok(response) => json_response(
                    StatusCode::from_u16(response.status).unwrap_or(StatusCode::OK),
                    &request_id,
                    chat_to_response_body(&response.body),
                ),
                Err(error) => error.into_response(),
            }
        }
        Err(error) => error.into_response(),
    }
}

pub async fn completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let request_id = request_id(&headers);
    if body.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        return GatewayError::InvalidRequest("Completions streaming is not implemented yet".into())
            .into_response();
    }
    match completions_to_chat_request(body) {
        Ok(mut request) => {
            request.request_id = Some(request_id.clone());
            match state.router.chat(&request).await {
                Ok(response) => json_response(
                    StatusCode::from_u16(response.status).unwrap_or(StatusCode::OK),
                    &request_id,
                    chat_to_completion_body(&response.body),
                ),
                Err(error) => error.into_response(),
            }
        }
        Err(error) => error.into_response(),
    }
}

pub async fn embeddings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let request_id = request_id(&headers);
    let Some(model) = body
        .get("model")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        return GatewayError::InvalidRequest("Embeddings request missing model".into())
            .into_response();
    };

    match state
        .router
        .post_openai_json(&model, "embeddings", body, None)
        .await
    {
        Ok(response) => json_response(
            StatusCode::from_u16(response.status).unwrap_or(StatusCode::OK),
            &request_id,
            response.body,
        ),
        Err(error) => error.into_response(),
    }
}

fn request_id(headers: &HeaderMap) -> String {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .unwrap_or_else(|| format!("req-{}", uuid::Uuid::new_v4()))
}

fn json_response(status: StatusCode, request_id: &str, body: Value) -> Response {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .header("X-Request-Id", request_id)
        .body(Body::from(serde_json::to_string(&body).unwrap_or_default()))
        .unwrap()
}

fn responses_to_chat_request(body: Value) -> Result<ChatCompletionRequest, GatewayError> {
    let model = body
        .get("model")
        .and_then(|value| value.as_str())
        .ok_or_else(|| GatewayError::InvalidRequest("Responses request missing model".into()))?;
    let mut messages = Vec::new();
    if let Some(instructions) = body.get("instructions").and_then(|value| value.as_str())
        && !instructions.trim().is_empty()
    {
        messages.push(chat_message(
            "system",
            Value::String(instructions.to_string()),
        ));
    }
    let input = body
        .get("input")
        .ok_or_else(|| GatewayError::InvalidRequest("Responses request missing input".into()))?;
    messages.extend(response_input_to_messages(input));
    if messages.is_empty() {
        return Err(GatewayError::InvalidRequest(
            "Responses request input produced no messages".into(),
        ));
    }

    Ok(ChatCompletionRequest {
        model: model.to_string(),
        messages,
        temperature: body.get("temperature").and_then(Value::as_f64),
        top_p: body.get("top_p").and_then(Value::as_f64),
        n: None,
        stream: body.get("stream").and_then(Value::as_bool),
        stop: parse_stop(body.get("stop")),
        max_tokens: body
            .get("max_output_tokens")
            .or_else(|| body.get("max_tokens"))
            .and_then(Value::as_u64)
            .map(|value| value as u32),
        presence_penalty: None,
        frequency_penalty: None,
        user: body.get("user").and_then(Value::as_str).map(str::to_string),
        request_id: None,
        agent_name: None,
        extra: serde_json::Map::new(),
    })
}

fn completions_to_chat_request(body: Value) -> Result<ChatCompletionRequest, GatewayError> {
    let model = body
        .get("model")
        .and_then(|value| value.as_str())
        .ok_or_else(|| GatewayError::InvalidRequest("Completions request missing model".into()))?;
    let prompt = body
        .get("prompt")
        .ok_or_else(|| GatewayError::InvalidRequest("Completions request missing prompt".into()))?;
    let prompt_text = match prompt {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|part| {
                part.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| part.to_string())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    };
    Ok(ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![chat_message("user", Value::String(prompt_text))],
        temperature: body.get("temperature").and_then(Value::as_f64),
        top_p: body.get("top_p").and_then(Value::as_f64),
        n: body
            .get("n")
            .and_then(Value::as_u64)
            .map(|value| value as u32),
        stream: Some(false),
        stop: parse_stop(body.get("stop")),
        max_tokens: body
            .get("max_tokens")
            .and_then(Value::as_u64)
            .map(|value| value as u32),
        presence_penalty: body.get("presence_penalty").and_then(Value::as_f64),
        frequency_penalty: body.get("frequency_penalty").and_then(Value::as_f64),
        user: body.get("user").and_then(Value::as_str).map(str::to_string),
        request_id: None,
        agent_name: None,
        extra: serde_json::Map::new(),
    })
}

fn response_input_to_messages(input: &Value) -> Vec<ChatMessage> {
    match input {
        Value::String(text) => vec![chat_message("user", Value::String(text.clone()))],
        Value::Array(items) => items
            .iter()
            .map(|item| {
                let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
                let content = item
                    .get("content")
                    .cloned()
                    .or_else(|| item.get("text").cloned())
                    .unwrap_or_else(|| Value::String(item.to_string()));
                chat_message(role, normalize_response_content(content))
            })
            .collect(),
        other => vec![chat_message("user", Value::String(other.to_string()))],
    }
}

fn normalize_response_content(content: Value) -> Value {
    match content {
        Value::Array(parts) => Value::Array(
            parts
                .into_iter()
                .map(|part| {
                    if part.get("type").and_then(Value::as_str) == Some("input_text") {
                        json!({
                            "type": "text",
                            "text": part.get("text").cloned().unwrap_or(Value::String(String::new()))
                        })
                    } else {
                        part
                    }
                })
                .collect(),
        ),
        other => other,
    }
}

fn chat_message(role: &str, content: Value) -> ChatMessage {
    ChatMessage {
        role: role.to_string(),
        content,
        name: None,
        tool_calls: None,
        tool_call_id: None,
        extra: serde_json::Map::new(),
    }
}

fn parse_stop(value: Option<&Value>) -> Option<StopToken> {
    match value {
        Some(Value::String(text)) => Some(StopToken::Single(text.clone())),
        Some(Value::Array(items)) => Some(StopToken::Multiple(
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect(),
        )),
        _ => None,
    }
}

fn chat_to_response_body(chat: &Value) -> Value {
    let text = first_assistant_text(chat);
    json!({
        "id": format!("resp-{}", uuid::Uuid::new_v4()),
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "status": "completed",
        "model": chat.get("model").cloned().unwrap_or(Value::Null),
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": text
            }]
        }],
        "output_text": text,
        "usage": chat.get("usage").cloned().unwrap_or(Value::Null)
    })
}

fn chat_to_completion_body(chat: &Value) -> Value {
    let text = first_assistant_text(chat);
    json!({
        "id": format!("cmpl-{}", uuid::Uuid::new_v4()),
        "object": "text_completion",
        "created": chrono::Utc::now().timestamp(),
        "model": chat.get("model").cloned().unwrap_or(Value::Null),
        "choices": [{
            "text": text,
            "index": 0,
            "logprobs": Value::Null,
            "finish_reason": chat["choices"][0].get("finish_reason").cloned().unwrap_or(Value::Null)
        }],
        "usage": chat.get("usage").cloned().unwrap_or(Value::Null)
    })
}

fn first_assistant_text(chat: &Value) -> String {
    let content = &chat["choices"][0]["message"]["content"];
    crate::models::content_to_text(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_input_string_maps_to_chat_request() {
        let request = responses_to_chat_request(json!({
            "model": "auto",
            "instructions": "be concise",
            "input": "hello",
            "max_output_tokens": 64
        }))
        .unwrap();

        assert_eq!(request.model, "auto");
        assert_eq!(request.messages[0].role, "system");
        assert_eq!(request.messages[1].role, "user");
        assert_eq!(request.messages[1].content, "hello");
        assert_eq!(request.max_tokens, Some(64));
    }

    #[test]
    fn completions_prompt_maps_to_chat_request() {
        let request = completions_to_chat_request(json!({
            "model": "text-model",
            "prompt": ["a", "b"],
            "max_tokens": 12
        }))
        .unwrap();

        assert_eq!(request.model, "text-model");
        assert_eq!(request.messages[0].content, "a\nb");
        assert_eq!(request.max_tokens, Some(12));
    }

    #[test]
    fn chat_response_maps_to_responses_shape() {
        let body = chat_to_response_body(&json!({
            "model": "m",
            "choices": [{"message": {"role": "assistant", "content": "done"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        }));

        assert_eq!(body["object"], "response");
        assert_eq!(body["output_text"], "done");
        assert_eq!(body["output"][0]["content"][0]["type"], "output_text");
    }
}
