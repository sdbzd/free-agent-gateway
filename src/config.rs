use std::collections::{HashMap, HashSet};
use std::env;

use serde::{Deserialize, Serialize};

use crate::error::{GatewayError, GatewayResult};

// ─── Top-level config ───────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub server: ServerConfig,
    pub routing: RoutingConfig,
    #[serde(default)]
    pub fallback: Vec<String>,
    #[serde(default)]
    pub agents: HashMap<String, AgentConfig>,
    #[serde(default)]
    pub models: HashMap<String, ModelAlias>,
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub watcher: WatcherConfig,
    #[serde(default)]
    pub state: StateConfig,
    #[serde(default)]
    pub cors: CorsConfig,
}

// ─── Server ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_timeout")]
    pub request_timeout: u64,
    #[serde(default = "default_sse_keepalive")]
    pub sse_keepalive: u64,
}

fn default_host() -> String {
    "127.0.0.1".into()
}
fn default_port() -> u16 {
    9000
}
fn default_log_level() -> String {
    "info".into()
}
fn default_timeout() -> u64 {
    120
}
fn default_sse_keepalive() -> u64 {
    15
}

// ─── Routing ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RoutingStrategy {
    RoundRobin,
    Random,
    LeastFailed,
    LeastRate,
    Priority,
}

impl std::str::FromStr for RoutingStrategy {
    type Err = GatewayError;
    fn from_str(s: &str) -> GatewayResult<Self> {
        match s {
            "round_robin" => Ok(Self::RoundRobin),
            "random" => Ok(Self::Random),
            "least_failed" => Ok(Self::LeastFailed),
            "least_rate" => Ok(Self::LeastRate),
            "priority" => Ok(Self::Priority),
            _ => Err(GatewayError::Config(format!(
                "Unknown routing strategy: {s}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RoutingConfig {
    #[serde(default = "default_routing_strategy")]
    pub strategy: RoutingStrategy,
    #[serde(default = "default_fail_threshold")]
    pub fail_threshold: u32,
    #[serde(default = "default_cooldown")]
    pub cooldown_seconds: u64,
    #[serde(default = "default_auto_discover")]
    pub auto_discover: bool,
}

fn default_routing_strategy() -> RoutingStrategy {
    RoutingStrategy::LeastFailed
}
fn default_fail_threshold() -> u32 {
    3
}
fn default_cooldown() -> u64 {
    600
}
fn default_auto_discover() -> bool {
    true
}

// ─── Agent ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentConfig {
    pub default_model: String,
}

// ─── Model Alias ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelAlias {
    #[serde(default)]
    pub provider: String,
    pub model: String,
}

// ─── Provider Config ─────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
    GithubModels,
    Nvidia,
    OpenaiCompatible,
    Ollama,
}

impl std::fmt::Display for ProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GithubModels => write!(f, "github_models"),
            Self::Nvidia => write!(f, "nvidia"),
            Self::OpenaiCompatible => write!(f, "openai_compatible"),
            Self::Ollama => write!(f, "ollama"),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum KeyTier {
    Free,
    Paid,
    #[default]
    Unknown,
}

impl std::fmt::Display for KeyTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Free => write!(f, "free"),
            Self::Paid => write!(f, "paid"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum KeyConfig {
    Legacy(String),
    Detailed {
        value: String,
        #[serde(default)]
        tier: KeyTier,
        /// Max requests per minute (None = unlimited).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rpm_limit: Option<u32>,
        /// Max requests per day (None = unlimited).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rpd_limit: Option<u32>,
        /// Max prompt tokens per minute (None = unlimited).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tpm_limit: Option<u32>,
        /// Max tokens per day (None = unlimited).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tpd_limit: Option<u32>,
    },
}

impl KeyConfig {
    pub fn value(&self) -> &str {
        match self {
            Self::Legacy(value) | Self::Detailed { value, .. } => value,
        }
    }

    pub fn tier(&self) -> KeyTier {
        match self {
            Self::Legacy(_) => KeyTier::Unknown,
            Self::Detailed { tier, .. } => *tier,
        }
    }

    pub fn rpm_limit(&self) -> Option<u32> {
        match self {
            Self::Legacy(_) => None,
            Self::Detailed { rpm_limit, .. } => *rpm_limit,
        }
    }

    pub fn rpd_limit(&self) -> Option<u32> {
        match self {
            Self::Legacy(_) => None,
            Self::Detailed { rpd_limit, .. } => *rpd_limit,
        }
    }

    pub fn tpm_limit(&self) -> Option<u32> {
        match self {
            Self::Legacy(_) => None,
            Self::Detailed { tpm_limit, .. } => *tpm_limit,
        }
    }

    pub fn tpd_limit(&self) -> Option<u32> {
        match self {
            Self::Legacy(_) => None,
            Self::Detailed { tpd_limit, .. } => *tpd_limit,
        }
    }

    /// Create a Detailed key config with all rate limits unset.
    pub fn detailed(value: impl Into<String>, tier: KeyTier) -> Self {
        Self::Detailed {
            value: value.into(),
            tier,
            rpm_limit: None,
            rpd_limit: None,
            tpm_limit: None,
            tpd_limit: None,
        }
    }
}

impl From<String> for KeyConfig {
    fn from(value: String) -> Self {
        Self::Legacy(value)
    }
}

impl From<&str> for KeyConfig {
    fn from(value: &str) -> Self {
        Self::Legacy(value.to_string())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderConfig {
    #[serde(rename = "type")]
    pub provider_type: ProviderType,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub base_url: String,
    pub keys: Vec<KeyConfig>,
    #[serde(default)]
    pub health_check_model: String,
    #[serde(default = "default_provider_timeout")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub priority: u32,
}

fn default_true() -> bool {
    true
}
fn default_provider_timeout() -> u64 {
    30
}

// ─── Watcher ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct WatcherConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_watcher_interval")]
    pub interval_seconds: u64,
    #[serde(default = "default_watcher_timeout")]
    pub check_timeout_seconds: u64,
}

fn default_watcher_interval() -> u64 {
    60
}
fn default_watcher_timeout() -> u64 {
    10
}

// ─── State ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct StateConfig {
    #[serde(default = "default_save_interval")]
    pub save_interval_seconds: u64,
    #[serde(default = "default_state_file")]
    pub state_file: String,
    #[serde(default = "default_models_cache_file")]
    pub models_cache_file: String,
}

fn default_save_interval() -> u64 {
    30
}
fn default_state_file() -> String {
    "state.json".into()
}
fn default_models_cache_file() -> String {
    "models.cache".into()
}

// ─── CORS ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct CorsConfig {
    #[serde(default = "default_cors_origins")]
    pub allowed_origins: Vec<String>,
    #[serde(default = "default_cors_methods")]
    pub allowed_methods: Vec<String>,
    #[serde(default = "default_cors_headers")]
    pub allowed_headers: Vec<String>,
}

fn default_cors_origins() -> Vec<String> {
    vec!["*".into()]
}
fn default_cors_methods() -> Vec<String> {
    vec!["GET".into(), "POST".into(), "OPTIONS".into()]
}
fn default_cors_headers() -> Vec<String> {
    vec![
        "Authorization".into(),
        "Content-Type".into(),
        "X-Request-Id".into(),
        "X-Agent-Name".into(),
    ]
}

// ─── Load helpers ────────────────────────────────────────────────────

/// Expand `${VAR_NAME}` patterns in a string using environment variables.
fn expand_env_vars(s: &str) -> String {
    let mut result = s.to_string();
    // Simple regex-free replacement: scan for ${...}
    while let Some(start) = result.find("${") {
        if let Some(end) = result[start..].find('}') {
            let var_name = &result[start + 2..start + end];
            let replacement = env::var(var_name).unwrap_or_default();
            result.replace_range(start..start + end + 1, &replacement);
        } else {
            break;
        }
    }
    result
}

impl Config {
    /// Validate cross-field configuration invariants after YAML parsing.
    pub fn validate(&self) -> GatewayResult<()> {
        if self.server.port == 0 {
            return Err(GatewayError::Config(
                "server.port must be greater than 0".into(),
            ));
        }
        if self.server.request_timeout == 0 {
            return Err(GatewayError::Config(
                "server.request_timeout must be greater than 0".into(),
            ));
        }
        if self.server.sse_keepalive == 0 {
            return Err(GatewayError::Config(
                "server.sse_keepalive must be greater than 0".into(),
            ));
        }
        if self.providers.is_empty() {
            return Err(GatewayError::Config(
                "at least one provider must be configured".into(),
            ));
        }

        for provider in &self.fallback {
            if !self.providers.contains_key(provider) {
                return Err(GatewayError::Config(format!(
                    "fallback provider '{provider}' is not configured"
                )));
            }
        }

        for (alias_name, alias) in &self.models {
            if alias.model.trim().is_empty() {
                return Err(GatewayError::Config(format!(
                    "model alias '{alias_name}' must specify a model"
                )));
            }
            if !alias.provider.trim().is_empty() && !self.providers.contains_key(&alias.provider) {
                return Err(GatewayError::Config(format!(
                    "model alias '{alias_name}' references unknown provider '{}'",
                    alias.provider
                )));
            }
        }

        for (agent_name, agent) in &self.agents {
            if agent.default_model.trim().is_empty() {
                return Err(GatewayError::Config(format!(
                    "agent '{agent_name}' must specify default_model"
                )));
            }
        }

        for (name, provider) in &self.providers {
            if provider.base_url.trim().is_empty() {
                return Err(GatewayError::Config(format!(
                    "provider '{name}' must specify base_url"
                )));
            }
            if provider.timeout_seconds == 0 {
                return Err(GatewayError::Config(format!(
                    "provider '{name}' timeout_seconds must be greater than 0"
                )));
            }

            if !provider.enabled {
                continue;
            }
            if provider.keys.is_empty() {
                return Err(GatewayError::Config(format!(
                    "enabled provider '{name}' must configure at least one key"
                )));
            }

            let mut seen_keys = HashSet::new();
            for key in &provider.keys {
                let value = key.value();
                if value.trim().is_empty() {
                    return Err(GatewayError::Config(format!(
                        "provider '{name}' contains an empty key"
                    )));
                }
                if !seen_keys.insert(value) {
                    return Err(GatewayError::Config(format!(
                        "provider '{name}' contains duplicate keys"
                    )));
                }
            }
        }

        Ok(())
    }

    /// Load configuration from a YAML file, expanding environment variables.
    pub fn load(path: &str) -> GatewayResult<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| GatewayError::Config(format!("Cannot read config {path}: {e}")))?;
        let expanded = expand_env_vars(&content);
        let config: Config = serde_yaml::from_str(&expanded)
            .map_err(|e| GatewayError::Config(format!("YAML parse error: {e}")))?;
        config.validate()?;
        Ok(config)
    }

    /// Load configuration from a YAML string, expanding environment variables.
    pub fn from_str_yaml(content: &str) -> GatewayResult<Self> {
        let expanded = expand_env_vars(content);
        let config: Config = serde_yaml::from_str(&expanded)
            .map_err(|e| GatewayError::Config(format!("YAML parse error: {e}")))?;
        config.validate()?;
        Ok(config)
    }
}
