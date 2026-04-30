//! `SessionLifecycleDb` trait — repository for the `/api/v1/sessions/*`
//! HTTP endpoints (E-09, E-10, E-11, E-12).
//!
//! Wraps the seven dashboard operations that live in Python's
//! `cognee/modules/session_lifecycle/metrics.py`:
//!   * `ensure_and_touch_session` — upsert + bump activity (idempotent)
//!   * `accumulate_usage` — atomic counter add to session row + per-model
//!     `session_model_usage` row
//!   * `get_session_row` — visibility-checked single-row read
//!   * `list_session_rows` — paginated list with status / since filters
//!   * `aggregate_stats` — totals / durations / status buckets
//!   * `cost_by_model` — grouped per-model attribution
//!
//! The `effective_status` value for a session is computed at read time —
//! `running` rows whose `last_activity_at` is older than
//! `SESSION_ABANDON_AFTER_SECONDS` (default 1800s — Decision 12) report
//! as `abandoned` *without* mutating the row. This mirrors Python's
//! `get_effective_status_sql` and keeps abandonment cheap (no sweeper,
//! no writes on read).
//!
//! Conventions match LIB-03's entity layer: UUIDs are persisted as
//! 32-char hex strings (`uuid_hex.rs`), timestamps as `DateTimeUtc`.
//! The trait's public Rust signatures take `Uuid` and convert at the
//! boundary so callers don't see hex strings.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::entities::session_record;
use crate::types::DatabaseError;

/// Filters for `SessionLifecycleDb::list_session_rows`. Field-for-field
/// parity with Python's `list_session_rows` keyword arguments at
/// `cognee/modules/session_lifecycle/metrics.py:365-374`.
#[derive(Debug, Clone)]
pub struct SessionListFilters {
    /// Visibility scope: caller's own sessions are always included.
    pub user_id: Uuid,
    /// Additional dataset scope — sessions whose `dataset_id` is in this
    /// list are included via OR'd visibility predicate.
    pub permitted_dataset_ids: Vec<Uuid>,
    /// Optional `last_activity_at >= since` filter.
    pub since: Option<DateTime<Utc>>,
    /// Optional effective-status filter (`completed` / `failed` /
    /// `abandoned` / `running`). The repository applies the
    /// `effective_status` SQL expression so `abandoned` matches running
    /// rows past the idle threshold.
    pub status_filter: Option<String>,
    /// Page size. Caller-validated upstream (E-09 enforces `1..=500`).
    pub limit: u32,
    /// Page offset.
    pub offset: u32,
    /// Column to sort by. Recognized: `last_activity_at`, `started_at`,
    /// `ended_at`, `cost_usd`, `tokens_in`, `tokens_out`. Anything else
    /// silently falls back to `last_activity_at` (mirrors Python's
    /// `sortable.get(order_by, ...)` lookup at `metrics.py:415-423`).
    pub order_by: String,
    /// Direction. `true` → DESC.
    pub descending: bool,
}

/// Wraps a stored `session_records` row plus the read-time effective
/// status (`abandoned` for stale running rows). Mirrors Python's
/// `SessionRowWithStatus` dataclass at
/// `cognee/modules/session_lifecycle/metrics.py:336-348`.
#[derive(Debug, Clone)]
pub struct SessionRowWithStatus {
    pub record: session_record::Model,
    pub effective_status: String,
}

impl SessionRowWithStatus {
    /// Render to a JSON object whose key order matches Python's
    /// `to_dict()` — entity dict + `effective_status`.
    pub fn to_dict(&self) -> serde_json::Value {
        let mut value = self.record.to_dict();
        if let Some(map) = value.as_object_mut() {
            map.insert(
                "effective_status".to_string(),
                serde_json::Value::String(self.effective_status.clone()),
            );
        }
        value
    }
}

/// Paginated envelope returned by `list_session_rows`. Parity with
/// Python's `SessionListPage` dataclass at `metrics.py:351-362`.
#[derive(Debug, Clone)]
pub struct SessionListPage {
    pub sessions: Vec<SessionRowWithStatus>,
    pub total: i64,
    pub limit: u32,
    pub offset: u32,
}

impl SessionListPage {
    /// `true` when pagination has more rows beyond the current page.
    /// Matches Python's `has_more` property at `metrics.py:360-362`.
    pub fn has_more(&self) -> bool {
        let returned = i64::try_from(self.sessions.len()).unwrap_or(i64::MAX);
        let offset = i64::from(self.offset);
        offset.saturating_add(returned) < self.total
    }
}

/// Aggregate counters for `GET /api/v1/sessions/stats`. Field-for-field
/// parity with the Python response body at
/// `cognee/api/v1/sessions/routers/get_sessions_router.py:179-196`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStats {
    pub sessions: i64,
    pub total_spend_usd: f64,
    pub avg_spend_per_session_usd: f64,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub tokens_total: i64,
    pub agent_time_s: f64,
    pub avg_session_s: f64,
    pub success_rate: f64,
    pub completed: i64,
    pub failed: i64,
    pub abandoned: i64,
    pub running: i64,
}

/// Single per-model row for `GET /api/v1/sessions/cost-by-model`.
/// Parity with the Python response items at
/// `cognee/api/v1/sessions/routers/get_sessions_router.py:241-251`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostByModelRow {
    pub model: String,
    pub session_count: i64,
    pub cost_usd: f64,
    pub tokens_in: i64,
    pub tokens_out: i64,
}

/// Repository trait for `/api/v1/sessions/*`. See module docs.
#[async_trait]
#[allow(clippy::too_many_arguments)] // accumulate_usage mirrors Python kw-args at metrics.py:133-141
pub trait SessionLifecycleDb: Send + Sync {
    /// Upsert a session row, bumping `last_activity_at` if the row is
    /// already running. Mirrors `metrics.py:62-130`.
    async fn ensure_and_touch_session(
        &self,
        session_id: &str,
        user_id: Uuid,
        dataset_id: Option<Uuid>,
    ) -> Result<(), DatabaseError>;

    /// Atomically add usage counters to the session row + per-model
    /// row. Mirrors `metrics.py:133-241`.
    async fn accumulate_usage(
        &self,
        session_id: &str,
        user_id: Uuid,
        model: Option<&str>,
        tokens_in: i64,
        tokens_out: i64,
        cost_usd: f64,
        errored: bool,
    ) -> Result<(), DatabaseError>;

    /// Visibility-checked single-row read. Mirrors `metrics.py:295-333`.
    async fn get_session_row(
        &self,
        session_id: &str,
        user_id: Uuid,
        permitted_dataset_ids: &[Uuid],
        prefer_other_owner: bool,
    ) -> Result<Option<SessionRowWithStatus>, DatabaseError>;

    /// Paginated list with `effective_status` filter support. Mirrors
    /// `metrics.py:365-438`.
    async fn list_session_rows(
        &self,
        filters: SessionListFilters,
    ) -> Result<SessionListPage, DatabaseError>;

    /// Dashboard counters for `GET /sessions/stats`. Mirrors
    /// `get_sessions_router.py:112-196`.
    async fn aggregate_stats(
        &self,
        user_id: Uuid,
        permitted_dataset_ids: &[Uuid],
        since: Option<DateTime<Utc>>,
    ) -> Result<SessionStats, DatabaseError>;

    /// Per-model attribution for `GET /sessions/cost-by-model`.
    /// Mirrors `get_sessions_router.py:198-252`.
    async fn cost_by_model(
        &self,
        user_id: Uuid,
        permitted_dataset_ids: &[Uuid],
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<CostByModelRow>, DatabaseError>;
}
