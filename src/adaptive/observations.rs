use super::profile::TaskProfile;
use crate::AppState;

pub fn record_profile_observations(
    state: &AppState,
    provider: &str,
    model: &str,
    profile: &TaskProfile,
    outcome: &str,
) {
    let Some(meta) = state.model_meta.as_ref() else {
        return;
    };

    for capability in required_capabilities(profile) {
        let _ = meta.record_capability_observation(provider, model, capability, outcome);
    }
}

fn required_capabilities(profile: &TaskProfile) -> Vec<&'static str> {
    let mut capabilities = Vec::new();
    if profile.needs_vision {
        capabilities.push("vision");
    }
    if profile.needs_tools {
        capabilities.push("tools");
    }
    if profile.needs_reasoning {
        capabilities.push("reasoning");
    }
    if profile.needs_coding {
        capabilities.push("coding");
    }
    if profile.needs_long_context {
        capabilities.push("long_context");
    }
    capabilities
}
