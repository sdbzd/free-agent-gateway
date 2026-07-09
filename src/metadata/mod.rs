/// Model metadata store — SQLite-backed auto-learning database.
///
/// Three data sources:
/// 1. **Public sync**: Fetch model catalogs from OpenRouter / NVIDIA APIs
/// 2. **Runtime learning**: Parse 429 errors, provider model lists, usage stats
/// 3. **HTTP middleware**: Intercept model responses to extract metadata
pub mod learner;
pub mod sync;

use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::{Connection, params};

use crate::error::GatewayResult;

/// Thread-safe wrapper around SQLite connection.
#[derive(Clone)]
pub struct ModelMetaStore {
    db: Arc<Mutex<Connection>>,
}

impl ModelMetaStore {
    /// Open (or create) the database at the given path and run migrations.
    pub fn open(path: impl AsRef<Path>) -> GatewayResult<Self> {
        let db = Connection::open(path)?;
        let store = Self {
            db: Arc::new(Mutex::new(db)),
        };
        store.migrate()?;
        Ok(store)
    }

    /// Run schema migrations.
    fn migrate(&self) -> GatewayResult<()> {
        let db = self.db.lock();
        db.execute_batch(
            "
            -- Core model metadata table
            CREATE TABLE IF NOT EXISTS model_meta (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                provider        TEXT NOT NULL,
                model_id        TEXT NOT NULL,
                display_name    TEXT,
                context_window  INTEGER,
                max_completion_tokens INTEGER,
                supports_vision INTEGER DEFAULT 0,
                supports_tools  INTEGER DEFAULT 0,
                supports_reasoning INTEGER DEFAULT 0,
                pricing_prompt  REAL,
                pricing_completion REAL,
                architecture_modality TEXT,
                rpm_limit       INTEGER,
                rpd_limit       INTEGER,
                tpm_limit       INTEGER,
                tpd_limit       INTEGER,
                first_seen_at   INTEGER NOT NULL,
                last_updated_at INTEGER NOT NULL,
                update_count    INTEGER DEFAULT 1,
                source          TEXT DEFAULT 'discovery',
                UNIQUE(provider, model_id)
            );

            -- Usage tracking per model per day
            CREATE TABLE IF NOT EXISTS model_usage (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                provider        TEXT NOT NULL,
                model_id        TEXT NOT NULL,
                date            TEXT NOT NULL,
                request_count   INTEGER DEFAULT 0,
                prompt_tokens   INTEGER DEFAULT 0,
                completion_tokens INTEGER DEFAULT 0,
                reported_prompt_tokens INTEGER DEFAULT 0,
                reported_completion_tokens INTEGER DEFAULT 0,
                estimated_prompt_tokens INTEGER DEFAULT 0,
                estimated_completion_tokens INTEGER DEFAULT 0,
                token_reported_requests INTEGER DEFAULT 0,
                success_count   INTEGER DEFAULT 0,
                error_count     INTEGER DEFAULT 0,
                last_used_at    INTEGER,
                UNIQUE(provider, model_id, date)
            );

            -- Usage tracking per model per hour. This survives raw request-log cleanup
            -- while preserving enough shape for daily/hourly dashboards.
            CREATE TABLE IF NOT EXISTS model_usage_hourly (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                provider        TEXT NOT NULL,
                model_id        TEXT NOT NULL,
                hour            TEXT NOT NULL,
                request_count   INTEGER DEFAULT 0,
                prompt_tokens   INTEGER DEFAULT 0,
                completion_tokens INTEGER DEFAULT 0,
                reported_prompt_tokens INTEGER DEFAULT 0,
                reported_completion_tokens INTEGER DEFAULT 0,
                estimated_prompt_tokens INTEGER DEFAULT 0,
                estimated_completion_tokens INTEGER DEFAULT 0,
                token_reported_requests INTEGER DEFAULT 0,
                success_count   INTEGER DEFAULT 0,
                error_count     INTEGER DEFAULT 0,
                last_used_at    INTEGER,
                UNIQUE(provider, model_id, hour)
            );

            -- Lifetime usage totals. This is intentionally append-only aggregate
            -- state so pruning detailed/daily rows cannot erase all-time totals.
            CREATE TABLE IF NOT EXISTS usage_lifetime (
                id              INTEGER PRIMARY KEY CHECK (id = 1),
                request_count   INTEGER DEFAULT 0,
                prompt_tokens   INTEGER DEFAULT 0,
                completion_tokens INTEGER DEFAULT 0,
                reported_prompt_tokens INTEGER DEFAULT 0,
                reported_completion_tokens INTEGER DEFAULT 0,
                estimated_prompt_tokens INTEGER DEFAULT 0,
                estimated_completion_tokens INTEGER DEFAULT 0,
                token_reported_requests INTEGER DEFAULT 0,
                success_count   INTEGER DEFAULT 0,
                error_count     INTEGER DEFAULT 0,
                first_used_at   INTEGER,
                last_used_at    INTEGER
            );

            -- Rate limits learned from 429 responses
            CREATE TABLE IF NOT EXISTS rate_limit_learned (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                provider        TEXT NOT NULL,
                model_id        TEXT NOT NULL,
                limit_type      TEXT NOT NULL,
                limit_value     INTEGER NOT NULL,
                learned_at      INTEGER NOT NULL,
                source          TEXT DEFAULT 'error_429'
            );

            -- Sync tracking (when we last synced from each public source)
            CREATE TABLE IF NOT EXISTS sync_state (
                source_name     TEXT PRIMARY KEY,
                last_sync_at    INTEGER NOT NULL,
                items_found     INTEGER DEFAULT 0,
                items_updated   INTEGER DEFAULT 0,
                error_message   TEXT
            );

            -- Error category breakdown per model per day
            CREATE TABLE IF NOT EXISTS model_errors (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                provider        TEXT NOT NULL,
                model_id        TEXT NOT NULL,
                date            TEXT NOT NULL,
                category        TEXT NOT NULL,
                count           INTEGER DEFAULT 0,
                last_error_at   INTEGER,
                UNIQUE(provider, model_id, date, category)
            );

            -- Task-aware adaptive routing performance by model, agent, and task.
            CREATE TABLE IF NOT EXISTS model_task_stats (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                provider        TEXT NOT NULL,
                model_id        TEXT NOT NULL,
                agent           TEXT,
                task_kind       TEXT NOT NULL,
                date            TEXT NOT NULL,
                request_count   INTEGER DEFAULT 0,
                success_count   INTEGER DEFAULT 0,
                error_count     INTEGER DEFAULT 0,
                total_latency_ms INTEGER DEFAULT 0,
                prompt_tokens   INTEGER DEFAULT 0,
                completion_tokens INTEGER DEFAULT 0,
                last_used_at    INTEGER,
                UNIQUE(provider, model_id, agent, task_kind, date)
            );

            -- Learned capability observations from runtime adaptive requests.
            CREATE TABLE IF NOT EXISTS model_capability_observations (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                provider        TEXT NOT NULL,
                model_id        TEXT NOT NULL,
                capability      TEXT NOT NULL,
                outcome         TEXT NOT NULL,
                count           INTEGER DEFAULT 0,
                last_observed_at INTEGER,
                UNIQUE(provider, model_id, capability, outcome)
            );

            -- Per-attempt routing trace. This is the source of truth for
            -- diagnosing failover, bad model aliases, and provider/key health.
            CREATE TABLE IF NOT EXISTS request_attempts (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                request_id      TEXT NOT NULL,
                attempt_index   INTEGER NOT NULL,
                provider        TEXT NOT NULL,
                model_id        TEXT NOT NULL,
                key_id          TEXT NOT NULL,
                success         INTEGER NOT NULL,
                error_category  TEXT,
                http_status     INTEGER,
                error_message   TEXT,
                cooldown_seconds INTEGER,
                fallback        INTEGER NOT NULL DEFAULT 0,
                created_at      INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_request_attempts_request
                ON request_attempts(request_id, attempt_index);
            CREATE INDEX IF NOT EXISTS idx_request_attempts_created
                ON request_attempts(created_at);

            -- Aggregated state for one concrete deployment: provider + model + key.
            -- Router decisions should move toward this table instead of relying on
            -- provider-wide optimistic health alone.
            CREATE TABLE IF NOT EXISTS deployment_state (
                provider        TEXT NOT NULL,
                model_id        TEXT NOT NULL,
                key_id          TEXT NOT NULL,
                success_count   INTEGER DEFAULT 0,
                error_count     INTEGER DEFAULT 0,
                consecutive_failures INTEGER DEFAULT 0,
                last_success_at INTEGER,
                last_error_at   INTEGER,
                last_error_category TEXT,
                last_http_status INTEGER,
                cooldown_until  INTEGER,
                updated_at      INTEGER NOT NULL,
                PRIMARY KEY(provider, model_id, key_id)
            );
            CREATE INDEX IF NOT EXISTS idx_deployment_state_cooldown
                ON deployment_state(cooldown_until);
            ",
        )?;
        let _ = db.execute(
            "ALTER TABLE model_usage ADD COLUMN token_reported_requests INTEGER DEFAULT 0",
            [],
        );
        for table in ["model_usage", "model_usage_hourly", "usage_lifetime"] {
            let _ = db.execute(
                &format!("ALTER TABLE {table} ADD COLUMN reported_prompt_tokens INTEGER DEFAULT 0"),
                [],
            );
            let _ = db.execute(
                &format!(
                    "ALTER TABLE {table} ADD COLUMN reported_completion_tokens INTEGER DEFAULT 0"
                ),
                [],
            );
            let _ = db.execute(
                &format!(
                    "ALTER TABLE {table} ADD COLUMN estimated_prompt_tokens INTEGER DEFAULT 0"
                ),
                [],
            );
            let _ = db.execute(
                &format!(
                    "ALTER TABLE {table} ADD COLUMN estimated_completion_tokens INTEGER DEFAULT 0"
                ),
                [],
            );
        }
        db.execute(
            "UPDATE model_usage
             SET token_reported_requests = request_count
             WHERE token_reported_requests = 0
               AND (prompt_tokens > 0 OR completion_tokens > 0)",
            [],
        )?;
        for table in ["model_usage", "model_usage_hourly", "usage_lifetime"] {
            db.execute(
                &format!(
                    "UPDATE {table}
                     SET reported_prompt_tokens = prompt_tokens,
                         reported_completion_tokens = completion_tokens
                     WHERE reported_prompt_tokens = 0
                       AND reported_completion_tokens = 0
                       AND token_reported_requests >= request_count
                       AND (prompt_tokens > 0 OR completion_tokens > 0)"
                ),
                [],
            )?;
            db.execute(
                &format!(
                    "UPDATE {table}
                     SET estimated_prompt_tokens = prompt_tokens,
                         estimated_completion_tokens = completion_tokens
                     WHERE estimated_prompt_tokens = 0
                       AND estimated_completion_tokens = 0
                       AND token_reported_requests = 0
                       AND (prompt_tokens > 0 OR completion_tokens > 0)"
                ),
                [],
            )?;
        }
        Ok(())
    }

    // ─── CRUD: model_meta ────────────────────────────────────────────

    /// Upsert model metadata from any learning source.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_model(
        &self,
        provider: &str,
        model_id: &str,
        display_name: Option<&str>,
        context_window: Option<i64>,
        max_completion_tokens: Option<i64>,
        supports_vision: Option<bool>,
        supports_tools: Option<bool>,
        supports_reasoning: Option<bool>,
        pricing_prompt: Option<f64>,
        pricing_completion: Option<f64>,
        architecture_modality: Option<&str>,
        rpm_limit: Option<i64>,
        rpd_limit: Option<i64>,
        tpm_limit: Option<i64>,
        tpd_limit: Option<i64>,
        source: &str,
    ) -> GatewayResult<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let db = self.db.lock();
        let mut stmt = db.prepare_cached(
            "INSERT INTO model_meta (provider, model_id, display_name, context_window,
                max_completion_tokens, supports_vision, supports_tools, supports_reasoning,
                pricing_prompt, pricing_completion, architecture_modality,
                rpm_limit, rpd_limit, tpm_limit, tpd_limit,
                first_seen_at, last_updated_at, update_count, source)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                    ?12, ?13, ?14, ?15,
                    ?16, ?16, 1, ?17)
            ON CONFLICT(provider, model_id) DO UPDATE SET
                display_name        = COALESCE(?3,  display_name),
                context_window      = COALESCE(?4,  context_window),
                max_completion_tokens = COALESCE(?5, max_completion_tokens),
                supports_vision     = COALESCE(?6,  supports_vision),
                supports_tools      = COALESCE(?7,  supports_tools),
                supports_reasoning  = COALESCE(?8,  supports_reasoning),
                pricing_prompt      = COALESCE(?9,  pricing_prompt),
                pricing_completion  = COALESCE(?10, pricing_completion),
                architecture_modality = COALESCE(?11, architecture_modality),
                rpm_limit           = COALESCE(?12, rpm_limit),
                rpd_limit           = COALESCE(?13, rpd_limit),
                tpm_limit           = COALESCE(?14, tpm_limit),
                tpd_limit           = COALESCE(?15, tpd_limit),
                last_updated_at     = ?16,
                update_count        = update_count + 1,
                source              = ?17",
        )?;

        stmt.execute(params![
            provider,
            model_id,
            display_name,
            context_window,
            max_completion_tokens,
            supports_vision.map(|v| v as i32),
            supports_tools.map(|v| v as i32),
            supports_reasoning.map(|v| v as i32),
            pricing_prompt,
            pricing_completion,
            architecture_modality,
            rpm_limit,
            rpd_limit,
            tpm_limit,
            tpd_limit,
            now,
            source,
        ])?;

        Ok(())
    }

    /// Query all known models with optional provider filter.
    pub fn list_models(&self, provider_filter: Option<&str>) -> GatewayResult<Vec<ModelMetaRow>> {
        let db = self.db.lock();
        let mut sql = String::from(
            "SELECT id, provider, model_id, display_name, context_window,
                    max_completion_tokens, supports_vision, supports_tools, supports_reasoning,
                    pricing_prompt, pricing_completion, architecture_modality,
                    rpm_limit, rpd_limit, tpm_limit, tpd_limit,
                    first_seen_at, last_updated_at, update_count, source
             FROM model_meta",
        );
        let params: Vec<Box<dyn rusqlite::types::ToSql>>;
        if let Some(filter) = provider_filter {
            sql.push_str(" WHERE provider = ?1");
            params = vec![Box::new(filter.to_string())];
        } else {
            params = vec![];
        }
        sql.push_str(" ORDER BY provider, model_id");

        let mut stmt = db.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
            |row| {
                Ok(ModelMetaRow {
                    id: row.get(0)?,
                    provider: row.get(1)?,
                    model_id: row.get(2)?,
                    display_name: row.get(3)?,
                    context_window: row.get(4)?,
                    max_completion_tokens: row.get(5)?,
                    supports_vision: row.get::<_, Option<i32>>(6)?.map(|v| v != 0),
                    supports_tools: row.get::<_, Option<i32>>(7)?.map(|v| v != 0),
                    supports_reasoning: row.get::<_, Option<i32>>(8)?.map(|v| v != 0),
                    pricing_prompt: row.get(9)?,
                    pricing_completion: row.get(10)?,
                    architecture_modality: row.get(11)?,
                    rpm_limit: row.get(12)?,
                    rpd_limit: row.get(13)?,
                    tpm_limit: row.get(14)?,
                    tpd_limit: row.get(15)?,
                    first_seen_at: row.get(16)?,
                    last_updated_at: row.get(17)?,
                    update_count: row.get(18)?,
                    source: row.get(19)?,
                })
            },
        )?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Get metadata for a specific provider+model.
    pub fn get_model(&self, provider: &str, model_id: &str) -> GatewayResult<Option<ModelMetaRow>> {
        let db = self.db.lock();
        let mut stmt = db.prepare_cached(
            "SELECT id, provider, model_id, display_name, context_window,
                    max_completion_tokens, supports_vision, supports_tools, supports_reasoning,
                    pricing_prompt, pricing_completion, architecture_modality,
                    rpm_limit, rpd_limit, tpm_limit, tpd_limit,
                    first_seen_at, last_updated_at, update_count, source
             FROM model_meta WHERE provider = ?1 AND model_id = ?2",
        )?;

        let mut rows = stmt.query_map(params![provider, model_id], |row| {
            Ok(ModelMetaRow {
                id: row.get(0)?,
                provider: row.get(1)?,
                model_id: row.get(2)?,
                display_name: row.get(3)?,
                context_window: row.get(4)?,
                max_completion_tokens: row.get(5)?,
                supports_vision: row.get::<_, Option<i32>>(6)?.map(|v| v != 0),
                supports_tools: row.get::<_, Option<i32>>(7)?.map(|v| v != 0),
                supports_reasoning: row.get::<_, Option<i32>>(8)?.map(|v| v != 0),
                pricing_prompt: row.get(9)?,
                pricing_completion: row.get(10)?,
                architecture_modality: row.get(11)?,
                rpm_limit: row.get(12)?,
                rpd_limit: row.get(13)?,
                tpm_limit: row.get(14)?,
                tpd_limit: row.get(15)?,
                first_seen_at: row.get(16)?,
                last_updated_at: row.get(17)?,
                update_count: row.get(18)?,
                source: row.get(19)?,
            })
        })?;

        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Try to get model metadata without waiting for long-running sync/write work.
    ///
    /// Adaptive routing uses this on the request path. Missing metadata is safer
    /// than letting a SQLite sync lock decide request latency.
    pub fn try_get_model(
        &self,
        provider: &str,
        model_id: &str,
    ) -> GatewayResult<Option<ModelMetaRow>> {
        let Some(db) = self.db.try_lock() else {
            return Ok(None);
        };
        let mut stmt = db.prepare_cached(
            "SELECT id, provider, model_id, display_name, context_window,
                    max_completion_tokens, supports_vision, supports_tools, supports_reasoning,
                    pricing_prompt, pricing_completion, architecture_modality,
                    rpm_limit, rpd_limit, tpm_limit, tpd_limit,
                    first_seen_at, last_updated_at, update_count, source
             FROM model_meta WHERE provider = ?1 AND model_id = ?2",
        )?;

        let mut rows = stmt.query_map(params![provider, model_id], |row| {
            Ok(ModelMetaRow {
                id: row.get(0)?,
                provider: row.get(1)?,
                model_id: row.get(2)?,
                display_name: row.get(3)?,
                context_window: row.get(4)?,
                max_completion_tokens: row.get(5)?,
                supports_vision: row.get::<_, Option<i32>>(6)?.map(|v| v != 0),
                supports_tools: row.get::<_, Option<i32>>(7)?.map(|v| v != 0),
                supports_reasoning: row.get::<_, Option<i32>>(8)?.map(|v| v != 0),
                pricing_prompt: row.get(9)?,
                pricing_completion: row.get(10)?,
                architecture_modality: row.get(11)?,
                rpm_limit: row.get(12)?,
                rpd_limit: row.get(13)?,
                tpm_limit: row.get(14)?,
                tpd_limit: row.get(15)?,
                first_seen_at: row.get(16)?,
                last_updated_at: row.get(17)?,
                update_count: row.get(18)?,
                source: row.get(19)?,
            })
        })?;

        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    // ─── Usage tracking ──────────────────────────────────────────────

    /// Record a request attempt for a model.
    pub fn record_usage(
        &self,
        provider: &str,
        model_id: &str,
        success: bool,
        prompt_tokens: Option<i64>,
        completion_tokens: Option<i64>,
        tokens_reported: bool,
    ) -> GatewayResult<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let date = chrono::Local::now().format("%Y-%m-%d").to_string();
        let hour = chrono::Local::now().format("%Y-%m-%d %H:00:00").to_string();
        let prompt_tokens = prompt_tokens.unwrap_or(0).max(0);
        let completion_tokens = completion_tokens.unwrap_or(0).max(0);
        let token_reported = if tokens_reported { 1 } else { 0 };
        let reported_prompt_tokens = if tokens_reported { prompt_tokens } else { 0 };
        let reported_completion_tokens = if tokens_reported {
            completion_tokens
        } else {
            0
        };
        let estimated_prompt_tokens = if tokens_reported { 0 } else { prompt_tokens };
        let estimated_completion_tokens = if tokens_reported {
            0
        } else {
            completion_tokens
        };
        let success_inc = if success { 1 } else { 0 };
        let error_inc = if success { 0 } else { 1 };

        let db = self.db.lock();
        let mut stmt = db.prepare_cached(
            "INSERT INTO model_usage (provider, model_id, date, request_count, prompt_tokens,
                completion_tokens, reported_prompt_tokens, reported_completion_tokens,
                estimated_prompt_tokens, estimated_completion_tokens,
                token_reported_requests, success_count, error_count, last_used_at)
            VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            ON CONFLICT(provider, model_id, date) DO UPDATE SET
                request_count     = request_count + 1,
                prompt_tokens     = prompt_tokens + COALESCE(?4, 0),
                completion_tokens = completion_tokens + COALESCE(?5, 0),
                reported_prompt_tokens = reported_prompt_tokens + ?6,
                reported_completion_tokens = reported_completion_tokens + ?7,
                estimated_prompt_tokens = estimated_prompt_tokens + ?8,
                estimated_completion_tokens = estimated_completion_tokens + ?9,
                token_reported_requests = token_reported_requests + ?10,
                success_count     = success_count + ?11,
                error_count       = error_count + ?12,
                last_used_at      = ?13",
        )?;

        stmt.execute(params![
            provider,
            model_id,
            date,
            prompt_tokens,
            completion_tokens,
            reported_prompt_tokens,
            reported_completion_tokens,
            estimated_prompt_tokens,
            estimated_completion_tokens,
            token_reported,
            success_inc,
            error_inc,
            now,
        ])?;

        let mut stmt = db.prepare_cached(
            "INSERT INTO model_usage_hourly (provider, model_id, hour, request_count, prompt_tokens,
                completion_tokens, reported_prompt_tokens, reported_completion_tokens,
                estimated_prompt_tokens, estimated_completion_tokens,
                token_reported_requests, success_count, error_count, last_used_at)
            VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            ON CONFLICT(provider, model_id, hour) DO UPDATE SET
                request_count     = request_count + 1,
                prompt_tokens     = prompt_tokens + ?4,
                completion_tokens = completion_tokens + ?5,
                reported_prompt_tokens = reported_prompt_tokens + ?6,
                reported_completion_tokens = reported_completion_tokens + ?7,
                estimated_prompt_tokens = estimated_prompt_tokens + ?8,
                estimated_completion_tokens = estimated_completion_tokens + ?9,
                token_reported_requests = token_reported_requests + ?10,
                success_count     = success_count + ?11,
                error_count       = error_count + ?12,
                last_used_at      = ?13",
        )?;
        stmt.execute(params![
            provider,
            model_id,
            hour,
            prompt_tokens,
            completion_tokens,
            reported_prompt_tokens,
            reported_completion_tokens,
            estimated_prompt_tokens,
            estimated_completion_tokens,
            token_reported,
            success_inc,
            error_inc,
            now,
        ])?;

        let mut stmt = db.prepare_cached(
            "INSERT INTO usage_lifetime (id, request_count, prompt_tokens, completion_tokens,
                reported_prompt_tokens, reported_completion_tokens,
                estimated_prompt_tokens, estimated_completion_tokens,
                token_reported_requests, success_count, error_count, first_used_at, last_used_at)
             VALUES (1, 1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
             ON CONFLICT(id) DO UPDATE SET
                request_count = request_count + 1,
                prompt_tokens = prompt_tokens + ?1,
                completion_tokens = completion_tokens + ?2,
                reported_prompt_tokens = reported_prompt_tokens + ?3,
                reported_completion_tokens = reported_completion_tokens + ?4,
                estimated_prompt_tokens = estimated_prompt_tokens + ?5,
                estimated_completion_tokens = estimated_completion_tokens + ?6,
                token_reported_requests = token_reported_requests + ?7,
                success_count = success_count + ?8,
                error_count = error_count + ?9,
                first_used_at = COALESCE(first_used_at, ?10),
                last_used_at = ?10",
        )?;
        stmt.execute(params![
            prompt_tokens,
            completion_tokens,
            reported_prompt_tokens,
            reported_completion_tokens,
            estimated_prompt_tokens,
            estimated_completion_tokens,
            token_reported,
            success_inc,
            error_inc,
            now,
        ])?;

        Ok(())
    }

    /// Get usage stats for the dashboard.
    ///
    /// `days <= 0` returns all known history.
    pub fn get_usage_summary(&self, days: i64) -> GatewayResult<Vec<UsageSummaryRow>> {
        let db = self.db.lock();
        let sql = if days > 0 {
            "SELECT provider, model_id,
                    SUM(request_count) as total_requests,
                    SUM(prompt_tokens) as total_prompt,
                    SUM(completion_tokens) as total_completion,
                    SUM(success_count) as total_success,
                    SUM(error_count) as total_errors,
                    SUM(token_reported_requests) as token_reported_requests,
                    SUM(reported_prompt_tokens) as reported_prompt,
                    SUM(reported_completion_tokens) as reported_completion,
                    SUM(estimated_prompt_tokens) as estimated_prompt,
                    SUM(estimated_completion_tokens) as estimated_completion,
                    MAX(last_used_at) as last_used
             FROM model_usage
             WHERE date >= ?1
             GROUP BY provider, model_id
             ORDER BY total_requests DESC"
        } else {
            "SELECT provider, model_id,
                    SUM(request_count) as total_requests,
                    SUM(prompt_tokens) as total_prompt,
                    SUM(completion_tokens) as total_completion,
                    SUM(success_count) as total_success,
                    SUM(error_count) as total_errors,
                    SUM(token_reported_requests) as token_reported_requests,
                    SUM(reported_prompt_tokens) as reported_prompt,
                    SUM(reported_completion_tokens) as reported_completion,
                    SUM(estimated_prompt_tokens) as estimated_prompt,
                    SUM(estimated_completion_tokens) as estimated_completion,
                    MAX(last_used_at) as last_used
             FROM model_usage
             GROUP BY provider, model_id
             ORDER BY total_requests DESC"
        };
        let mut stmt = db.prepare_cached(sql)?;

        let map_row = |row: &rusqlite::Row<'_>| {
            Ok(UsageSummaryRow {
                provider: row.get(0)?,
                model_id: row.get(1)?,
                total_requests: row.get(2)?,
                total_prompt_tokens: row.get(3)?,
                total_completion_tokens: row.get(4)?,
                total_success: row.get(5)?,
                total_errors: row.get(6)?,
                token_reported_requests: row.get(7)?,
                reported_prompt_tokens: row.get(8)?,
                reported_completion_tokens: row.get(9)?,
                estimated_prompt_tokens: row.get(10)?,
                estimated_completion_tokens: row.get(11)?,
                last_used_at: row.get(12)?,
            })
        };

        let mut result = Vec::new();
        if days > 0 {
            let cutoff = chrono::Local::now()
                .checked_sub_signed(chrono::Duration::days(days))
                .unwrap_or_default()
                .format("%Y-%m-%d")
                .to_string();
            let rows = stmt.query_map(params![cutoff], map_row)?;
            for row in rows {
                result.push(row?);
            }
        } else {
            let rows = stmt.query_map([], map_row)?;
            for row in rows {
                result.push(row?);
            }
        }

        Ok(result)
    }

    /// Get dense daily usage buckets for trend displays.
    pub fn get_usage_daily_summary(&self, days: i64) -> GatewayResult<Vec<UsageDailyRow>> {
        let days = days.clamp(1, 366);
        let start_date = chrono::Local::now()
            .checked_sub_signed(chrono::Duration::days(days - 1))
            .unwrap_or_default()
            .date_naive();
        let cutoff = start_date.format("%Y-%m-%d").to_string();

        let db = self.db.lock();
        let mut stmt = db.prepare_cached(
            "SELECT date,
                    SUM(request_count) as total_requests,
                    SUM(prompt_tokens) as total_prompt,
                    SUM(completion_tokens) as total_completion,
                    SUM(success_count) as total_success,
                    SUM(error_count) as total_errors,
                    SUM(token_reported_requests) as token_reported_requests,
                    SUM(reported_prompt_tokens) as reported_prompt,
                    SUM(reported_completion_tokens) as reported_completion,
                    SUM(estimated_prompt_tokens) as estimated_prompt,
                    SUM(estimated_completion_tokens) as estimated_completion
             FROM model_usage
             WHERE date >= ?1
             GROUP BY date",
        )?;

        let rows = stmt.query_map(params![cutoff], |row| {
            let total_requests = row.get::<_, i64>(1)?;
            let token_reported_requests = row.get::<_, i64>(6)?;
            Ok(UsageDailyRow {
                date: row.get(0)?,
                total_requests,
                total_prompt_tokens: row.get(2)?,
                total_completion_tokens: row.get(3)?,
                total_success: row.get(4)?,
                total_errors: row.get(5)?,
                token_reported_requests,
                reported_prompt_tokens: row.get(7)?,
                reported_completion_tokens: row.get(8)?,
                estimated_prompt_tokens: row.get(9)?,
                estimated_completion_tokens: row.get(10)?,
                token_reporting_coverage: coverage(token_reported_requests, total_requests),
            })
        })?;

        let mut by_date = std::collections::BTreeMap::new();
        for row in rows {
            let row = row?;
            by_date.insert(row.date.clone(), row);
        }

        let mut result = Vec::new();
        for offset in 0..days {
            let date = start_date
                .checked_add_signed(chrono::Duration::days(offset))
                .unwrap_or(start_date)
                .format("%Y-%m-%d")
                .to_string();
            result.push(by_date.remove(&date).unwrap_or(UsageDailyRow {
                date,
                total_requests: 0,
                total_prompt_tokens: 0,
                total_completion_tokens: 0,
                total_success: 0,
                total_errors: 0,
                token_reported_requests: 0,
                reported_prompt_tokens: 0,
                reported_completion_tokens: 0,
                estimated_prompt_tokens: 0,
                estimated_completion_tokens: 0,
                token_reporting_coverage: None,
            }));
        }

        Ok(result)
    }

    /// Get dense hourly usage buckets for short-window trend displays.
    pub fn get_usage_hourly_summary(&self, hours: i64) -> GatewayResult<Vec<UsageHourlyRow>> {
        let hours = hours.clamp(1, 24 * 366);
        let start_hour = chrono::Local::now()
            .checked_sub_signed(chrono::Duration::hours(hours - 1))
            .unwrap_or_default()
            .format("%Y-%m-%d %H:00:00")
            .to_string();

        let db = self.db.lock();
        let mut stmt = db.prepare_cached(
            "SELECT hour,
                    SUM(request_count) as total_requests,
                    SUM(prompt_tokens) as total_prompt,
                    SUM(completion_tokens) as total_completion,
                    SUM(success_count) as total_success,
                    SUM(error_count) as total_errors,
                    SUM(token_reported_requests) as token_reported_requests,
                    SUM(reported_prompt_tokens) as reported_prompt,
                    SUM(reported_completion_tokens) as reported_completion,
                    SUM(estimated_prompt_tokens) as estimated_prompt,
                    SUM(estimated_completion_tokens) as estimated_completion
             FROM model_usage_hourly
             WHERE hour >= ?1
             GROUP BY hour",
        )?;

        let rows = stmt.query_map(params![start_hour], |row| {
            let total_requests = row.get::<_, i64>(1)?;
            let token_reported_requests = row.get::<_, i64>(6)?;
            Ok(UsageHourlyRow {
                hour: row.get(0)?,
                total_requests,
                total_prompt_tokens: row.get(2)?,
                total_completion_tokens: row.get(3)?,
                total_success: row.get(4)?,
                total_errors: row.get(5)?,
                token_reported_requests,
                reported_prompt_tokens: row.get(7)?,
                reported_completion_tokens: row.get(8)?,
                estimated_prompt_tokens: row.get(9)?,
                estimated_completion_tokens: row.get(10)?,
                token_reporting_coverage: coverage(token_reported_requests, total_requests),
            })
        })?;

        let mut by_hour = std::collections::BTreeMap::new();
        for row in rows {
            let row = row?;
            by_hour.insert(row.hour.clone(), row);
        }

        let first = chrono::Local::now()
            .checked_sub_signed(chrono::Duration::hours(hours - 1))
            .unwrap_or_default();
        let mut result = Vec::new();
        for offset in 0..hours {
            let hour = first
                .checked_add_signed(chrono::Duration::hours(offset))
                .unwrap_or(first)
                .format("%Y-%m-%d %H:00:00")
                .to_string();
            result.push(by_hour.remove(&hour).unwrap_or(UsageHourlyRow {
                hour,
                total_requests: 0,
                total_prompt_tokens: 0,
                total_completion_tokens: 0,
                total_success: 0,
                total_errors: 0,
                token_reported_requests: 0,
                reported_prompt_tokens: 0,
                reported_completion_tokens: 0,
                estimated_prompt_tokens: 0,
                estimated_completion_tokens: 0,
                token_reporting_coverage: None,
            }));
        }

        Ok(result)
    }

    /// Get all-time aggregate usage totals.
    pub fn get_usage_lifetime(&self) -> GatewayResult<UsageLifetimeRow> {
        let db = self.db.lock();
        let mut stmt = db.prepare_cached(
            "SELECT request_count, prompt_tokens, completion_tokens,
                    token_reported_requests, success_count, error_count,
                    reported_prompt_tokens, reported_completion_tokens,
                    estimated_prompt_tokens, estimated_completion_tokens,
                    first_used_at, last_used_at
             FROM usage_lifetime
             WHERE id = 1",
        )?;
        let mut rows = stmt.query_map([], |row| {
            let total_requests = row.get::<_, i64>(0)?;
            let token_reported_requests = row.get::<_, i64>(3)?;
            Ok(UsageLifetimeRow {
                total_requests,
                total_prompt_tokens: row.get(1)?,
                total_completion_tokens: row.get(2)?,
                token_reported_requests,
                total_success: row.get(4)?,
                total_errors: row.get(5)?,
                reported_prompt_tokens: row.get(6)?,
                reported_completion_tokens: row.get(7)?,
                estimated_prompt_tokens: row.get(8)?,
                estimated_completion_tokens: row.get(9)?,
                token_reporting_coverage: coverage(token_reported_requests, total_requests),
                first_used_at: row.get(10)?,
                last_used_at: row.get(11)?,
            })
        })?;

        match rows.next() {
            Some(row) => Ok(row?),
            None => Ok(UsageLifetimeRow::default()),
        }
    }

    /// Record adaptive routing usage for a model under a specific task and optional agent.
    #[allow(clippy::too_many_arguments)]
    pub fn record_task_usage(
        &self,
        provider: &str,
        model_id: &str,
        agent: Option<&str>,
        task_kind: &str,
        success: bool,
        latency_ms: i64,
        prompt_tokens: Option<i64>,
        completion_tokens: Option<i64>,
    ) -> GatewayResult<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let date = chrono::Local::now().format("%Y-%m-%d").to_string();

        let db = self.db.lock();
        let mut stmt = db.prepare_cached(
            "INSERT INTO model_task_stats (provider, model_id, agent, task_kind, date,
                request_count, success_count, error_count, total_latency_ms,
                prompt_tokens, completion_tokens, last_used_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(provider, model_id, agent, task_kind, date) DO UPDATE SET
                request_count = request_count + 1,
                success_count = success_count + ?6,
                error_count = error_count + ?7,
                total_latency_ms = total_latency_ms + ?8,
                prompt_tokens = prompt_tokens + ?9,
                completion_tokens = completion_tokens + ?10,
                last_used_at = ?11",
        )?;
        stmt.execute(params![
            provider,
            model_id,
            agent,
            task_kind,
            date,
            if success { 1 } else { 0 },
            if success { 0 } else { 1 },
            latency_ms.max(0),
            prompt_tokens.unwrap_or(0),
            completion_tokens.unwrap_or(0),
            now,
        ])?;
        Ok(())
    }

    /// Return aggregated adaptive routing stats for one model/task over the recent window.
    pub fn get_task_stats(
        &self,
        provider: &str,
        model_id: &str,
        agent: Option<&str>,
        task_kind: &str,
        days: i64,
    ) -> GatewayResult<Option<TaskStatsRow>> {
        let cutoff = chrono::Local::now()
            .checked_sub_signed(chrono::Duration::days(days.max(0)))
            .unwrap_or_default()
            .format("%Y-%m-%d")
            .to_string();

        let db = self.db.lock();
        let mut stmt = db.prepare_cached(
            "SELECT provider, model_id, agent, task_kind,
                    SUM(request_count) as request_count,
                    SUM(success_count) as success_count,
                    SUM(error_count) as error_count,
                    SUM(total_latency_ms) as total_latency_ms,
                    SUM(prompt_tokens) as prompt_tokens,
                    SUM(completion_tokens) as completion_tokens,
                    MAX(last_used_at) as last_used_at
             FROM model_task_stats
             WHERE provider = ?1
               AND model_id = ?2
               AND ((?3 IS NULL AND agent IS NULL) OR agent = ?3)
               AND task_kind = ?4
               AND date >= ?5
             GROUP BY provider, model_id, agent, task_kind",
        )?;

        let mut rows = stmt.query_map(
            params![provider, model_id, agent, task_kind, cutoff],
            |row| {
                Ok(TaskStatsRow {
                    provider: row.get(0)?,
                    model_id: row.get(1)?,
                    agent: row.get(2)?,
                    task_kind: row.get(3)?,
                    request_count: row.get(4)?,
                    success_count: row.get(5)?,
                    error_count: row.get(6)?,
                    total_latency_ms: row.get(7)?,
                    prompt_tokens: row.get(8)?,
                    completion_tokens: row.get(9)?,
                    last_used_at: row.get(10)?,
                })
            },
        )?;

        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Return adaptive routing task stats grouped by model, optional agent, and task.
    pub fn get_task_stats_summary(&self, days: i64) -> GatewayResult<Vec<TaskStatsRow>> {
        let cutoff = chrono::Local::now()
            .checked_sub_signed(chrono::Duration::days(days.max(0)))
            .unwrap_or_default()
            .format("%Y-%m-%d")
            .to_string();

        let db = self.db.lock();
        let mut stmt = db.prepare_cached(
            "SELECT provider, model_id, agent, task_kind,
                    SUM(request_count) as request_count,
                    SUM(success_count) as success_count,
                    SUM(error_count) as error_count,
                    SUM(total_latency_ms) as total_latency_ms,
                    SUM(prompt_tokens) as prompt_tokens,
                    SUM(completion_tokens) as completion_tokens,
                    MAX(last_used_at) as last_used_at
             FROM model_task_stats
             WHERE date >= ?1
             GROUP BY provider, model_id, agent, task_kind
             ORDER BY request_count DESC, error_count DESC, provider, model_id",
        )?;

        let rows = stmt.query_map(params![cutoff], |row| {
            Ok(TaskStatsRow {
                provider: row.get(0)?,
                model_id: row.get(1)?,
                agent: row.get(2)?,
                task_kind: row.get(3)?,
                request_count: row.get(4)?,
                success_count: row.get(5)?,
                error_count: row.get(6)?,
                total_latency_ms: row.get(7)?,
                prompt_tokens: row.get(8)?,
                completion_tokens: row.get(9)?,
                last_used_at: row.get(10)?,
            })
        })?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Record an observed capability outcome, such as tools=failure.
    pub fn record_capability_observation(
        &self,
        provider: &str,
        model_id: &str,
        capability: &str,
        outcome: &str,
    ) -> GatewayResult<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let db = self.db.lock();
        let mut stmt = db.prepare_cached(
            "INSERT INTO model_capability_observations
                (provider, model_id, capability, outcome, count, last_observed_at)
             VALUES (?1, ?2, ?3, ?4, 1, ?5)
             ON CONFLICT(provider, model_id, capability, outcome) DO UPDATE SET
                count = count + 1,
                last_observed_at = ?5",
        )?;
        stmt.execute(params![provider, model_id, capability, outcome, now])?;
        Ok(())
    }

    /// Count observations for one model/capability/outcome tuple.
    pub fn get_capability_observation_count(
        &self,
        provider: &str,
        model_id: &str,
        capability: &str,
        outcome: &str,
    ) -> GatewayResult<i64> {
        let db = self.db.lock();
        Ok(db.query_row(
            "SELECT COALESCE(SUM(count), 0)
             FROM model_capability_observations
             WHERE provider = ?1 AND model_id = ?2 AND capability = ?3 AND outcome = ?4",
            params![provider, model_id, capability, outcome],
            |row| row.get(0),
        )?)
    }

    /// Return grouped learned capability observations.
    pub fn get_capability_observation_summary(
        &self,
    ) -> GatewayResult<Vec<CapabilityObservationRow>> {
        let db = self.db.lock();
        let mut stmt = db.prepare_cached(
            "SELECT provider, model_id, capability, outcome,
                    SUM(count) as count,
                    MAX(last_observed_at) as last_observed_at
             FROM model_capability_observations
             GROUP BY provider, model_id, capability, outcome
             ORDER BY count DESC, provider, model_id, capability, outcome",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(CapabilityObservationRow {
                provider: row.get(0)?,
                model_id: row.get(1)?,
                capability: row.get(2)?,
                outcome: row.get(3)?,
                count: row.get(4)?,
                last_observed_at: row.get(5)?,
            })
        })?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    // ─── Rate limit learning ──────────────────────────────────────────

    /// Record a learned rate limit.
    pub fn learn_rate_limit(
        &self,
        provider: &str,
        model_id: &str,
        limit_type: &str,
        limit_value: i64,
    ) -> GatewayResult<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let db = self.db.lock();
        let mut stmt = db.prepare_cached(
            "INSERT INTO rate_limit_learned (provider, model_id, limit_type, limit_value, learned_at, source)
             VALUES (?1, ?2, ?3, ?4, ?5, 'error_429')",
        )?;
        stmt.execute(params![provider, model_id, limit_type, limit_value, now])?;

        // Also upsert the rate limit into model_meta
        let limit_col = match limit_type {
            "rpm" => "rpm_limit",
            "rpd" => "rpd_limit",
            "tpm" => "tpm_limit",
            "tpd" => "tpd_limit",
            _ => return Ok(()),
        };

        let update_sql = format!(
            "UPDATE model_meta SET {limit_col} = ?1, last_updated_at = ?2
             WHERE provider = ?3 AND model_id = ?4
               AND ({limit_col} IS NULL OR {limit_col} > ?1)"
        );
        db.execute(&update_sql, params![limit_value, now, provider, model_id])?;

        Ok(())
    }

    /// Count learned rate-limit observations.
    pub fn learned_rate_limit_count(&self) -> GatewayResult<i64> {
        let db = self.db.lock();
        Ok(db
            .query_row("SELECT COUNT(*) FROM rate_limit_learned", [], |row| {
                row.get(0)
            })
            .unwrap_or(0))
    }

    // ─── Error category tracking ───────────────────────────────────────

    /// Classify an error message into a category for tracking.
    pub fn classify_error(error_msg: &str, http_status: u16) -> &'static str {
        let lower = error_msg.to_ascii_lowercase();
        if http_status == 429 {
            "rate_limit"
        } else if http_status == 401 || http_status == 403 {
            "auth"
        } else if http_status == 404
            || lower.contains("not a valid model id")
            || lower.contains("invalid model id")
            || lower.contains("unknown model")
            || lower.contains("no such model")
            || lower.contains("model_not_found")
        {
            "not_found"
        } else if lower.contains("timeout") || lower.contains("time out") || http_status == 504 {
            "timeout"
        } else if http_status >= 500 {
            "upstream"
        } else {
            "other"
        }
    }

    /// Record a failed request with its error category.
    pub fn record_model_error(
        &self,
        provider: &str,
        model_id: &str,
        category: &str,
    ) -> GatewayResult<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let date = chrono::Local::now().format("%Y-%m-%d").to_string();

        let db = self.db.lock();
        let mut stmt = db.prepare_cached(
            "INSERT INTO model_errors (provider, model_id, date, category, count, last_error_at)
             VALUES (?1, ?2, ?3, ?4, 1, ?5)
             ON CONFLICT(provider, model_id, date, category) DO UPDATE SET
                count = count + 1,
                last_error_at = ?5",
        )?;
        stmt.execute(params![provider, model_id, date, category, now])?;
        Ok(())
    }

    /// Get error distribution for a provider+model (last N days).
    pub fn get_model_error_distribution(
        &self,
        provider: &str,
        model_id: &str,
        days: i64,
    ) -> GatewayResult<Vec<ErrorDistRow>> {
        let cutoff = chrono::Local::now()
            .checked_sub_signed(chrono::Duration::days(days))
            .unwrap_or_default()
            .format("%Y-%m-%d")
            .to_string();

        let db = self.db.lock();
        let mut stmt = db.prepare_cached(
            "SELECT category, SUM(count) as total, MAX(last_error_at) as last_at
             FROM model_errors
             WHERE provider = ?1 AND model_id = ?2 AND date >= ?3
             GROUP BY category
             ORDER BY total DESC",
        )?;

        let rows = stmt.query_map(params![provider, model_id, cutoff], |row| {
            Ok(ErrorDistRow {
                category: row.get(0)?,
                total: row.get(1)?,
                last_error_at: row.get(2)?,
            })
        })?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Get error summary across all models (last N days).
    pub fn get_error_summary(&self, days: i64) -> GatewayResult<Vec<ErrorSummaryRow>> {
        let cutoff = chrono::Local::now()
            .checked_sub_signed(chrono::Duration::days(days))
            .unwrap_or_default()
            .format("%Y-%m-%d")
            .to_string();

        let db = self.db.lock();
        let mut stmt = db.prepare_cached(
            "SELECT provider, model_id, category, SUM(count) as total
             FROM model_errors
             WHERE date >= ?1
             GROUP BY provider, model_id, category
             ORDER BY total DESC
             LIMIT 50",
        )?;

        let rows = stmt.query_map(params![cutoff], |row| {
            Ok(ErrorSummaryRow {
                provider: row.get(0)?,
                model_id: row.get(1)?,
                category: row.get(2)?,
                total: row.get(3)?,
            })
        })?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    // ─── Sync tracking ────────────────────────────────────────────────

    /// Record a successful sync from a public source.
    pub fn record_sync(
        &self,
        source_name: &str,
        items_found: i64,
        items_updated: i64,
    ) -> GatewayResult<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let db = self.db.lock();
        let mut stmt = db.prepare_cached(
            "INSERT INTO sync_state (source_name, last_sync_at, items_found, items_updated)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(source_name) DO UPDATE SET
                last_sync_at = ?2, items_found = ?3, items_updated = ?4, error_message = NULL",
        )?;
        stmt.execute(params![source_name, now, items_found, items_updated])?;
        Ok(())
    }

    /// Record a sync failure.
    pub fn record_sync_error(&self, source_name: &str, error: &str) -> GatewayResult<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let db = self.db.lock();
        let mut stmt = db.prepare_cached(
            "INSERT INTO sync_state (source_name, last_sync_at, error_message)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(source_name) DO UPDATE SET
                last_sync_at = ?2, error_message = ?3",
        )?;
        stmt.execute(params![source_name, now, error])?;
        Ok(())
    }

    /// Get sync status for all sources.
    pub fn get_sync_status(&self) -> GatewayResult<Vec<SyncStatusRow>> {
        let db = self.db.lock();
        let mut stmt = db.prepare_cached(
            "SELECT source_name, last_sync_at, items_found, items_updated, error_message
             FROM sync_state ORDER BY source_name",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(SyncStatusRow {
                source_name: row.get(0)?,
                last_sync_at: row.get(1)?,
                items_found: row.get(2)?,
                items_updated: row.get(3)?,
                error_message: row.get(4)?,
            })
        })?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Record one concrete routing attempt and update provider/model/key state.
    #[allow(clippy::too_many_arguments)]
    pub fn record_request_attempt(
        &self,
        request_id: &str,
        attempt_index: i64,
        provider: &str,
        model_id: &str,
        key_id: &str,
        success: bool,
        error_category: &str,
        http_status: Option<u16>,
        error_message: Option<&str>,
        cooldown_seconds: Option<i64>,
        fallback: bool,
    ) -> GatewayResult<()> {
        let now = unix_timestamp();
        let success_int = if success { 1 } else { 0 };
        let fallback_int = if fallback { 1 } else { 0 };
        let error_category = if success { None } else { Some(error_category) };
        let cooldown_until = cooldown_seconds
            .filter(|seconds| *seconds > 0)
            .map(|seconds| now + seconds);
        let http_status = http_status.map(i64::from);

        let db = self.db.lock();
        db.execute(
            "INSERT INTO request_attempts (
                request_id, attempt_index, provider, model_id, key_id, success,
                error_category, http_status, error_message, cooldown_seconds,
                fallback, created_at
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                request_id,
                attempt_index,
                provider,
                model_id,
                key_id,
                success_int,
                error_category,
                http_status,
                error_message,
                cooldown_seconds,
                fallback_int,
                now,
            ],
        )?;

        if success {
            db.execute(
                "INSERT INTO deployment_state (
                    provider, model_id, key_id, success_count, error_count,
                    consecutive_failures, last_success_at, last_error_at,
                    last_error_category, last_http_status, cooldown_until, updated_at
                 )
                 VALUES (?1, ?2, ?3, 1, 0, 0, ?4, NULL, NULL, NULL, NULL, ?4)
                 ON CONFLICT(provider, model_id, key_id) DO UPDATE SET
                    success_count = success_count + 1,
                    consecutive_failures = 0,
                    last_success_at = ?4,
                    cooldown_until = NULL,
                    updated_at = ?4",
                params![provider, model_id, key_id, now],
            )?;
        } else {
            db.execute(
                "INSERT INTO deployment_state (
                    provider, model_id, key_id, success_count, error_count,
                    consecutive_failures, last_success_at, last_error_at,
                    last_error_category, last_http_status, cooldown_until, updated_at
                 )
                 VALUES (?1, ?2, ?3, 0, 1, 1, NULL, ?4, ?5, ?6, ?7, ?4)
                 ON CONFLICT(provider, model_id, key_id) DO UPDATE SET
                    error_count = error_count + 1,
                    consecutive_failures = consecutive_failures + 1,
                    last_error_at = ?4,
                    last_error_category = ?5,
                    last_http_status = ?6,
                    cooldown_until = COALESCE(?7, cooldown_until),
                    updated_at = ?4",
                params![
                    provider,
                    model_id,
                    key_id,
                    now,
                    error_category,
                    http_status,
                    cooldown_until,
                ],
            )?;
        }

        Ok(())
    }

    /// Return newest routing attempts first.
    pub fn get_recent_attempts(&self, limit: i64) -> GatewayResult<Vec<RequestAttemptRow>> {
        let limit = limit.clamp(1, 1000);
        let db = self.db.lock();
        let mut stmt = db.prepare_cached(
            "SELECT id, request_id, attempt_index, provider, model_id, key_id,
                    success, error_category, http_status, error_message,
                    cooldown_seconds, fallback, created_at
             FROM request_attempts
             ORDER BY created_at DESC, id DESC
             LIMIT ?1",
        )?;

        let rows = stmt.query_map(params![limit], |row| {
            Ok(RequestAttemptRow {
                id: row.get(0)?,
                request_id: row.get(1)?,
                attempt_index: row.get(2)?,
                provider: row.get(3)?,
                model_id: row.get(4)?,
                key_id: row.get(5)?,
                success: row.get::<_, i64>(6)? != 0,
                error_category: row.get(7)?,
                http_status: row.get(8)?,
                error_message: row.get(9)?,
                cooldown_seconds: row.get(10)?,
                fallback: row.get::<_, i64>(11)? != 0,
                created_at: row.get(12)?,
            })
        })?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Return all known provider/model/key deployment states, newest first.
    pub fn get_deployment_states(&self) -> GatewayResult<Vec<DeploymentStateRow>> {
        let db = self.db.lock();
        let mut stmt = db.prepare_cached(
            "SELECT provider, model_id, key_id, success_count, error_count,
                    consecutive_failures, last_success_at, last_error_at,
                    last_error_category, last_http_status, cooldown_until, updated_at
             FROM deployment_state
             ORDER BY updated_at DESC, provider, model_id, key_id",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(DeploymentStateRow {
                provider: row.get(0)?,
                model_id: row.get(1)?,
                key_id: row.get(2)?,
                success_count: row.get(3)?,
                error_count: row.get(4)?,
                consecutive_failures: row.get(5)?,
                last_success_at: row.get(6)?,
                last_error_at: row.get(7)?,
                last_error_category: row.get(8)?,
                last_http_status: row.get(9)?,
                cooldown_until: row.get(10)?,
                updated_at: row.get(11)?,
            })
        })?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Get summary statistics for the dashboard.
    pub fn get_stats(&self) -> GatewayResult<MetaStats> {
        let db = self.db.lock();
        let total_models: i64 = db
            .query_row("SELECT COUNT(*) FROM model_meta", [], |row| row.get(0))
            .unwrap_or(0);

        let with_context: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM model_meta WHERE context_window IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let with_vision: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM model_meta WHERE supports_vision = 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let with_pricing: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM model_meta WHERE pricing_prompt IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let synced_count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM sync_state WHERE error_message IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        Ok(MetaStats {
            total_models,
            with_context_window: with_context,
            with_vision,
            with_pricing,
            synced_sources: synced_count,
            usage_records: db
                .query_row("SELECT COUNT(*) FROM model_usage", [], |row| row.get(0))
                .unwrap_or(0),
            learned_rate_limits: db
                .query_row("SELECT COUNT(*) FROM rate_limit_learned", [], |row| {
                    row.get(0)
                })
                .unwrap_or(0),
        })
    }
}

// ─── Row types ─────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelMetaRow {
    pub id: i64,
    pub provider: String,
    pub model_id: String,
    pub display_name: Option<String>,
    pub context_window: Option<i64>,
    pub max_completion_tokens: Option<i64>,
    pub supports_vision: Option<bool>,
    pub supports_tools: Option<bool>,
    pub supports_reasoning: Option<bool>,
    pub pricing_prompt: Option<f64>,
    pub pricing_completion: Option<f64>,
    pub architecture_modality: Option<String>,
    pub rpm_limit: Option<i64>,
    pub rpd_limit: Option<i64>,
    pub tpm_limit: Option<i64>,
    pub tpd_limit: Option<i64>,
    pub first_seen_at: i64,
    pub last_updated_at: i64,
    pub update_count: i64,
    pub source: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UsageSummaryRow {
    pub provider: String,
    pub model_id: String,
    pub total_requests: i64,
    pub total_prompt_tokens: i64,
    pub total_completion_tokens: i64,
    pub reported_prompt_tokens: i64,
    pub reported_completion_tokens: i64,
    pub estimated_prompt_tokens: i64,
    pub estimated_completion_tokens: i64,
    pub total_success: i64,
    pub total_errors: i64,
    pub token_reported_requests: i64,
    pub last_used_at: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UsageDailyRow {
    pub date: String,
    pub total_requests: i64,
    pub total_prompt_tokens: i64,
    pub total_completion_tokens: i64,
    pub reported_prompt_tokens: i64,
    pub reported_completion_tokens: i64,
    pub estimated_prompt_tokens: i64,
    pub estimated_completion_tokens: i64,
    pub total_success: i64,
    pub total_errors: i64,
    pub token_reported_requests: i64,
    pub token_reporting_coverage: Option<f64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UsageHourlyRow {
    pub hour: String,
    pub total_requests: i64,
    pub total_prompt_tokens: i64,
    pub total_completion_tokens: i64,
    pub reported_prompt_tokens: i64,
    pub reported_completion_tokens: i64,
    pub estimated_prompt_tokens: i64,
    pub estimated_completion_tokens: i64,
    pub total_success: i64,
    pub total_errors: i64,
    pub token_reported_requests: i64,
    pub token_reporting_coverage: Option<f64>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct UsageLifetimeRow {
    pub total_requests: i64,
    pub total_prompt_tokens: i64,
    pub total_completion_tokens: i64,
    pub reported_prompt_tokens: i64,
    pub reported_completion_tokens: i64,
    pub estimated_prompt_tokens: i64,
    pub estimated_completion_tokens: i64,
    pub total_success: i64,
    pub total_errors: i64,
    pub token_reported_requests: i64,
    pub token_reporting_coverage: Option<f64>,
    pub first_used_at: Option<i64>,
    pub last_used_at: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskStatsRow {
    pub provider: String,
    pub model_id: String,
    pub agent: Option<String>,
    pub task_kind: String,
    pub request_count: i64,
    pub success_count: i64,
    pub error_count: i64,
    pub total_latency_ms: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub last_used_at: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CapabilityObservationRow {
    pub provider: String,
    pub model_id: String,
    pub capability: String,
    pub outcome: String,
    pub count: i64,
    pub last_observed_at: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RequestAttemptRow {
    pub id: i64,
    pub request_id: String,
    pub attempt_index: i64,
    pub provider: String,
    pub model_id: String,
    pub key_id: String,
    pub success: bool,
    pub error_category: Option<String>,
    pub http_status: Option<i64>,
    pub error_message: Option<String>,
    pub cooldown_seconds: Option<i64>,
    pub fallback: bool,
    pub created_at: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DeploymentStateRow {
    pub provider: String,
    pub model_id: String,
    pub key_id: String,
    pub success_count: i64,
    pub error_count: i64,
    pub consecutive_failures: i64,
    pub last_success_at: Option<i64>,
    pub last_error_at: Option<i64>,
    pub last_error_category: Option<String>,
    pub last_http_status: Option<i64>,
    pub cooldown_until: Option<i64>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SyncStatusRow {
    pub source_name: String,
    pub last_sync_at: i64,
    pub items_found: i64,
    pub items_updated: i64,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ErrorDistRow {
    pub category: String,
    pub total: i64,
    pub last_error_at: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ErrorSummaryRow {
    pub provider: String,
    pub model_id: String,
    pub category: String,
    pub total: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MetaStats {
    pub total_models: i64,
    pub with_context_window: i64,
    pub with_vision: i64,
    pub with_pricing: i64,
    pub synced_sources: i64,
    pub usage_records: i64,
    pub learned_rate_limits: i64,
}

fn coverage(numerator: i64, denominator: i64) -> Option<f64> {
    if denominator > 0 {
        Some(numerator as f64 / denominator as f64)
    } else {
        None
    }
}

fn unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
