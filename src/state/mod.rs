/// State persistence: saves and loads gateway state to/from `state.json`.
///
/// No database — just a JSON file.
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{GatewayError, GatewayResult};
use crate::models::KeyState;

/// Persistable state for all keys across providers.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PersistedState {
    pub version: u32,
    pub providers: std::collections::HashMap<String, ProviderKeyState>,
    /// Per-provider set of disabled model IDs.
    #[serde(default)]
    pub disabled_models: std::collections::HashMap<String, Vec<String>>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderKeyState {
    pub keys: Vec<KeyState>,
}

impl PersistedState {
    /// Create a new empty persisted state.
    pub fn new() -> Self {
        Self {
            version: 1,
            providers: std::collections::HashMap::new(),
            disabled_models: std::collections::HashMap::new(),
            updated_at: chrono::Utc::now().timestamp(),
        }
    }

    /// Save state to a JSON file.
    pub fn save(&self, path: &str) -> GatewayResult<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| GatewayError::StateError(format!("Serialize error: {e}")))?;

        // Write atomically: write to temp file then rename
        let tmp_path = format!("{path}.tmp");
        fs::write(&tmp_path, &json)
            .map_err(|e| GatewayError::StateError(format!("Write error: {e}")))?;

        if let Err(e) = fs::rename(&tmp_path, path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(GatewayError::StateError(format!("Rename error: {e}")));
        }

        tracing::debug!(path = %path, "State saved");
        Ok(())
    }

    /// Load state from a JSON file. Returns default if file doesn't exist.
    pub fn load(path: &str) -> GatewayResult<Self> {
        if !Path::new(path).exists() {
            tracing::info!(path = %path, "State file not found, starting fresh");
            return Ok(Self::new());
        }

        let content = fs::read_to_string(path)
            .map_err(|e| GatewayError::StateError(format!("Read error: {e}")))?;

        let state: PersistedState = serde_json::from_str(&content)
            .map_err(|e| GatewayError::StateError(format!("Parse error: {e}")))?;

        tracing::info!(path = %path, "State loaded");
        Ok(state)
    }
}
