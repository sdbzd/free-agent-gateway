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
                success_count   INTEGER DEFAULT 0,
                error_count     INTEGER DEFAULT 0,
                last_used_at    INTEGER,
                UNIQUE(provider, model_id, date)
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
            ",
        )?;
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

    // ─── Usage tracking ──────────────────────────────────────────────

    /// Record a request attempt for a model.
    pub fn record_usage(
        &self,
        provider: &str,
        model_id: &str,
        success: bool,
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
            "INSERT INTO model_usage (provider, model_id, date, request_count, prompt_tokens,
                completion_tokens, success_count, error_count, last_used_at)
            VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(provider, model_id, date) DO UPDATE SET
                request_count     = request_count + 1,
                prompt_tokens     = prompt_tokens + COALESCE(?4, 0),
                completion_tokens = completion_tokens + COALESCE(?5, 0),
                success_count     = success_count + ?6,
                error_count       = error_count + ?7,
                last_used_at      = ?8",
        )?;

        stmt.execute(params![
            provider,
            model_id,
            date,
            prompt_tokens.unwrap_or(0),
            completion_tokens.unwrap_or(0),
            if success { 1 } else { 0 },
            if success { 0 } else { 1 },
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
                last_used_at: row.get(7)?,
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
        if http_status == 429 {
            "rate_limit"
        } else if http_status == 401 || http_status == 403 {
            "auth"
        } else if http_status == 404 {
            "not_found"
        } else if error_msg.contains("timeout")
            || error_msg.contains("Time out")
            || http_status == 504
        {
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
    pub total_success: i64,
    pub total_errors: i64,
    pub last_used_at: Option<i64>,
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
