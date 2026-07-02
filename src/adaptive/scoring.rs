use super::profile::TaskProfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateTier {
    Free,
    Paid,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct AdaptiveCandidate {
    pub provider: String,
    pub model: String,
    pub api_key: Option<String>,
    pub tier: CandidateTier,
    pub supports_vision: Option<bool>,
    pub supports_tools: Option<bool>,
    pub supports_reasoning: Option<bool>,
    pub context_window: Option<i64>,
    pub recent_successes: i64,
    pub recent_errors: i64,
    pub recent_429s: i64,
    pub recent_timeouts: i64,
    pub quota_headroom: u32,
    pub provider_priority: i32,
    pub prompt_price: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct RouteConstraints {
    pub provider: Option<String>,
    pub provider_group: Option<Vec<String>>,
    pub allow_paid: bool,
}

#[derive(Debug, Clone)]
pub struct ScoreBreakdown {
    pub capability_match: i32,
    pub reliability: i32,
    pub quota: i32,
    pub provider_priority: i32,
    pub cost: i32,
    pub penalty: i32,
}

impl ScoreBreakdown {
    pub fn total(&self) -> i32 {
        self.capability_match + self.reliability + self.quota + self.provider_priority + self.cost
            - self.penalty
    }
}

#[derive(Debug, Clone)]
pub struct ScoredCandidate {
    pub candidate: AdaptiveCandidate,
    pub score: i32,
    pub breakdown: ScoreBreakdown,
    pub rejection_reasons: Vec<String>,
}

pub fn score_candidates(
    profile: &TaskProfile,
    candidates: Vec<AdaptiveCandidate>,
    constraints: &RouteConstraints,
) -> Vec<ScoredCandidate> {
    let mut scored: Vec<ScoredCandidate> = candidates
        .into_iter()
        .filter(|candidate| route_allows(candidate, constraints))
        .filter(|candidate| constraints.allow_paid || candidate.tier == CandidateTier::Free)
        .filter(|candidate| hard_capabilities_allow(profile, candidate))
        .map(|candidate| {
            let breakdown = score_breakdown(profile, &candidate);
            let score = breakdown.total();
            ScoredCandidate {
                candidate,
                score,
                breakdown,
                rejection_reasons: Vec::new(),
            }
        })
        .collect();

    scored.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then(left.candidate.provider.cmp(&right.candidate.provider))
            .then(left.candidate.model.cmp(&right.candidate.model))
    });
    scored
}

fn route_allows(candidate: &AdaptiveCandidate, constraints: &RouteConstraints) -> bool {
    if let Some(provider) = &constraints.provider
        && &candidate.provider != provider
    {
        return false;
    }
    if let Some(group) = &constraints.provider_group
        && !group.iter().any(|provider| provider == &candidate.provider)
    {
        return false;
    }
    true
}

fn hard_capabilities_allow(profile: &TaskProfile, candidate: &AdaptiveCandidate) -> bool {
    if profile.needs_vision && candidate.supports_vision == Some(false) {
        return false;
    }
    if profile.needs_tools && candidate.supports_tools == Some(false) {
        return false;
    }
    if profile.needs_long_context
        && let Some(context_window) = candidate.context_window
        && context_window < profile.estimated_prompt_tokens as i64
    {
        return false;
    }
    true
}

fn score_breakdown(profile: &TaskProfile, candidate: &AdaptiveCandidate) -> ScoreBreakdown {
    let mut capability_match = 20;
    capability_match += capability_score(profile.needs_vision, candidate.supports_vision);
    capability_match += capability_score(profile.needs_tools, candidate.supports_tools);
    capability_match += capability_score(profile.needs_reasoning, candidate.supports_reasoning);
    if profile.needs_coding && model_name_suggests_coding(&candidate.model) {
        capability_match += 12;
    }
    if profile.needs_long_context
        && candidate
            .context_window
            .is_some_and(|window| window >= profile.estimated_prompt_tokens as i64)
    {
        capability_match += 10;
    }

    let total = candidate.recent_successes + candidate.recent_errors;
    let reliability = if total > 0 {
        ((candidate.recent_successes * 15) / total) as i32
    } else {
        8
    };
    let quota = (candidate.quota_headroom.min(100) / 10) as i32;
    let provider_priority = 5 - candidate.provider_priority.clamp(0, 5);
    let cost = candidate
        .prompt_price
        .map(|price| if price <= 0.0 { 5 } else { 1 })
        .unwrap_or(3);
    let penalty = (candidate.recent_errors as i32 * 2)
        + (candidate.recent_429s as i32 * 5)
        + (candidate.recent_timeouts as i32 * 5);

    ScoreBreakdown {
        capability_match,
        reliability,
        quota,
        provider_priority,
        cost,
        penalty,
    }
}

fn capability_score(required: bool, support: Option<bool>) -> i32 {
    if !required {
        return 0;
    }
    match support {
        Some(true) => 20,
        Some(false) => -100,
        None => 2,
    }
}

fn model_name_suggests_coding(model: &str) -> bool {
    let lower = model.to_lowercase();
    lower.contains("coder") || lower.contains("code") || lower.contains("qwen3")
}
