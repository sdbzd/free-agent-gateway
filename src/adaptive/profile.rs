use crate::models::ChatCompletionRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskKind {
    Balanced,
    Coding,
    Reasoning,
    Tools,
    Vision,
    LongContext,
    Document,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LatencyPreference {
    Interactive,
    Batch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CostPreference {
    Cheap,
    Balanced,
    Quality,
}

#[derive(Debug, Clone)]
pub struct TaskProfile {
    pub agent: Option<String>,
    pub task_kinds: Vec<TaskKind>,
    pub needs_vision: bool,
    pub needs_tools: bool,
    pub needs_reasoning: bool,
    pub needs_coding: bool,
    pub needs_long_context: bool,
    pub estimated_prompt_tokens: u32,
    pub latency_preference: LatencyPreference,
    pub cost_preference: CostPreference,
}

pub fn build_task_profile(
    agent: Option<&str>,
    request: &ChatCompletionRequest,
    route_task: Option<&str>,
) -> TaskProfile {
    let text = request_text(request);
    let lower = text.to_lowercase();
    let estimated_prompt_tokens = (text.chars().count() as u32 / 4).max(1);
    let needs_vision = crate::models::request_has_vision(&request.messages);
    let needs_tools = request.extra.contains_key("tools")
        || request.extra.contains_key("tool_choice")
        || lower.contains("tool call")
        || lower.contains("function call");
    let agent_name = agent.map(str::to_string);
    let agent_lower = agent.unwrap_or_default().to_lowercase();
    let coding_signal = lower.contains("```")
        || lower.contains("stack trace")
        || lower.contains("panic")
        || lower.contains("exception")
        || lower.contains("diff --git")
        || lower.contains(".rs")
        || lower.contains(".ts")
        || lower.contains(".py")
        || agent_lower.contains("coding")
        || agent_lower.contains("hermes");
    let reasoning_signal = coding_signal
        || lower.contains("reason")
        || lower.contains("plan")
        || lower.contains("debug")
        || lower.contains("analyze");
    let needs_long_context = estimated_prompt_tokens > 8_000
        || agent_lower.contains("document")
        || matches!(route_task, Some("document" | "long_context"));

    let mut task_kinds = Vec::new();
    push_task(&mut task_kinds, TaskKind::Balanced);
    if needs_vision || matches!(route_task, Some("vision")) {
        push_task(&mut task_kinds, TaskKind::Vision);
    }
    if needs_tools || matches!(route_task, Some("tools")) {
        push_task(&mut task_kinds, TaskKind::Tools);
    }
    if coding_signal || matches!(route_task, Some("coding")) {
        push_task(&mut task_kinds, TaskKind::Coding);
    }
    if reasoning_signal || matches!(route_task, Some("reasoning")) {
        push_task(&mut task_kinds, TaskKind::Reasoning);
    }
    if needs_long_context {
        push_task(&mut task_kinds, TaskKind::LongContext);
    }
    if agent_lower.contains("document") || matches!(route_task, Some("document")) {
        push_task(&mut task_kinds, TaskKind::Document);
    }

    TaskProfile {
        agent: agent_name,
        task_kinds,
        needs_vision,
        needs_tools,
        needs_reasoning: reasoning_signal || matches!(route_task, Some("reasoning" | "coding")),
        needs_coding: coding_signal || matches!(route_task, Some("coding")),
        needs_long_context,
        estimated_prompt_tokens,
        latency_preference: LatencyPreference::Interactive,
        cost_preference: CostPreference::Balanced,
    }
}

fn push_task(task_kinds: &mut Vec<TaskKind>, task: TaskKind) {
    if !task_kinds.contains(&task) {
        task_kinds.push(task);
    }
}

fn request_text(request: &ChatCompletionRequest) -> String {
    let mut parts = Vec::new();
    for message in &request.messages {
        collect_value_text(&message.content, &mut parts);
    }
    parts.join("\n")
}

fn collect_value_text(value: &serde_json::Value, parts: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => parts.push(text.clone()),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_value_text(value, parts);
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(|value| value.as_str()) {
                parts.push(text.to_string());
            }
        }
        _ => {}
    }
}
