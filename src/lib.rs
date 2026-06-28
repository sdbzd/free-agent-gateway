/// free-agent-gateway - Unified AI entry for the Agent ecosystem.
///
/// This is the library root. The binary entry point is in `main.rs`.
pub mod config;
pub mod error;
pub mod metadata;
pub mod models;

pub mod api;
pub mod health;
pub mod keyhub;
pub mod providers;
pub mod rate_rules;
pub mod router;
pub mod state;
pub mod watcher;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;

use dashmap::DashMap;
use parking_lot::RwLock;
use tokio::sync::broadcast;

use config::Config;
use health::HealthRegistry;
use keyhub::KeyHub;
use metadata::ModelMetaStore;
use router::Router;
use state::PersistedState;

/// Application shared context, passed to every handler.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    /// Persisted state (key states, cooldowns, etc.)
    pub state: Arc<RwLock<PersistedState>>,
    /// Shared HTTP client
    pub http_client: reqwest::Client,
    /// Registered provider instances
    pub providers: Arc<DashMap<String, providers::BoxedProvider>>,
    /// Key management
    pub keyhub: Arc<KeyHub>,
    /// Request router
    pub router: Arc<Router>,
    /// Health registry
    pub health_registry: Arc<HealthRegistry>,
    /// Total request counter
    pub request_counter: Arc<AtomicU64>,
    /// Total error counter
    pub error_counter: Arc<AtomicU64>,
    /// Server start time
    pub start_time: Instant,
    /// SSE broadcast channel for real-time dashboard events
    pub sse_tx: broadcast::Sender<String>,
    /// Per-provider disabled model IDs (provider_name -> set of model_ids)
    pub disabled_models: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    /// Model metadata database (auto-learning from experience)
    pub model_meta: Option<ModelMetaStore>,
    /// Symbol used to track the sync scheduler handle.
    pub _sync_handle: Arc<parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>>,
}
