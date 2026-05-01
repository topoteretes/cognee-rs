//! DTOs for `/api/v1/sessions/*` (E-09 owns the list endpoint).
//!
//! ## Wire shape — Python parity carve-out
//!
//! Unlike most v2 body DTOs (Decision 10 → camelCase), the sessions list
//! response wire shape is **snake_case** because Python returns a plain
//! `dict` via `JSONResponse(content={...})` rather than an `OutDTO`
//! subclass — `to_camel` does not apply to plain dicts. The per-row keys
//! mirror `SessionRecord.to_dict()` ([`models.py:68-86`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/session_lifecycle/models.py#L68-L86))
//! plus the read-time `effective_status`, and the envelope keys mirror
//! `get_sessions_router.py:99-107`. Both `SessionListResponseDTO` and
//! `SessionRowDTO` are therefore on the snake_case allow-list in
//! `tests/test_openapi_camelcase.rs`.
//!
//! Query-parameter struct (`ListSessionsQuery`) keeps its literal
//! parameter names on the wire — Python's `Query()` does not apply
//! `alias_generator` to query params (see `dto/mod.rs` doc).
//!
//! Decision 9 / divergence D-1: the `OrderBy` enum rejects unknown
//! variants at deserialization time. Python's handler silently falls
//! back to `last_activity_at`; Rust deliberately diverges to surface
//! client typos (see [`README.md §1.2`](../../../../docs/http-api-v2/README.md#12-v2-acknowledged-divergences-changes-to-steady-state-wire-output)).

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

// ─── Query parameter enums ────────────────────────────────────────────────────

/// Time-window filter for `GET /api/v1/sessions`.
///
/// Mirrors Python's `_RangeLiteral` at
/// [`get_sessions_router.py:36`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L36)
/// — strict-parity: the four variants are `24h | 7d | 30d | all`. The
/// previous draft (`90d`) is **not** a Python value and is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize, ToSchema)]
pub enum RangeWindow {
    #[serde(rename = "24h")]
    H24,
    #[serde(rename = "7d")]
    D7,
    #[default]
    #[serde(rename = "30d")]
    D30,
    #[serde(rename = "all")]
    All,
}

/// Sortable columns for `GET /api/v1/sessions`.
///
/// Decision 9 / divergence D-1: typed enum rejects unknown variants at
/// deserialization time. Python's [`metrics.py:415-423`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/session_lifecycle/metrics.py#L415-L423)
/// silently falls back to `last_activity_at` for unknown inputs; Rust
/// surfaces `400` with the Python validation envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OrderBy {
    #[default]
    LastActivityAt,
    StartedAt,
    EndedAt,
    CostUsd,
    TokensIn,
    TokensOut,
}

impl OrderBy {
    /// String form passed to LIB-05's `SessionListFilters::order_by`.
    /// The canonical column names match Python's `sortable` lookup at
    /// [`metrics.py:415-423`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/session_lifecycle/metrics.py#L415-L423).
    pub fn as_column(self) -> &'static str {
        match self {
            Self::LastActivityAt => "last_activity_at",
            Self::StartedAt => "started_at",
            Self::EndedAt => "ended_at",
            Self::CostUsd => "cost_usd",
            Self::TokensIn => "tokens_in",
            Self::TokensOut => "tokens_out",
        }
    }
}

// ─── Query struct ─────────────────────────────────────────────────────────────

fn default_limit() -> u32 {
    50
}

fn default_descending() -> bool {
    true
}

/// Query parameters for `GET /api/v1/sessions`.
///
/// Wire names match the literal Rust field names (snake_case) — Python's
/// `Query()` defaults at
/// [`get_sessions_router.py:64-72`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L64-L72)
/// expose the same names. Out of scope for Decision 10's camelCase rule
/// (which targets `OutDTO`/`InDTO` body fields, not query strings).
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListSessionsQuery {
    /// Time window. Default `30d`.
    #[serde(default)]
    pub range: RangeWindow,
    /// Optional effective-status filter (`completed` / `failed` /
    /// `abandoned` / `running`). String passthrough — LIB-05 applies the
    /// `effective_status` SQL expression so `abandoned` matches running
    /// rows past the idle threshold.
    #[serde(default)]
    pub status: Option<String>,
    /// Page size, validated `1..=500` in the handler. Default `50`.
    #[serde(default = "default_limit")]
    pub limit: u32,
    /// Page offset (`u32` enforces `>= 0`).
    #[serde(default)]
    pub offset: u32,
    /// Sort column. Default `last_activity_at`. Decision 9 / D-1 rejects
    /// unknown variants with 400.
    #[serde(default)]
    pub order_by: OrderBy,
    /// Direction. `true` → DESC. Default `true`.
    #[serde(default = "default_descending")]
    pub descending: bool,
}

// ─── Response DTOs (snake_case wire) ──────────────────────────────────────────

/// Paginated envelope for `GET /api/v1/sessions`.
///
/// snake_case wire — Python returns a plain dict via `jsonable_encoder`
/// ([`get_sessions_router.py:99-107`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L99-L107)),
/// not an `OutDTO`, so `to_camel` does not apply.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SessionListResponseDTO {
    pub sessions: Vec<SessionRowDTO>,
    pub total: i64,
    pub limit: u32,
    pub offset: u32,
    pub has_more: bool,
}

/// Per-row body for the sessions list. snake_case keys mirror Python
/// `SessionRecord.to_dict()` at
/// [`models.py:68-86`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/session_lifecycle/models.py#L68-L86)
/// plus the read-time `effective_status`. Every `DateTime<Utc>` field
/// uses the Decision 6 `iso8601_offset` serde helper (`+00:00` shape with
/// microsecond precision).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SessionRowDTO {
    pub session_id: String,
    pub user_id: String,
    pub dataset_id: Option<String>,
    pub status: String,
    #[serde(with = "crate::dto::util::iso8601_offset")]
    pub started_at: chrono::DateTime<chrono::Utc>,
    #[serde(with = "crate::dto::util::iso8601_offset")]
    pub last_activity_at: chrono::DateTime<chrono::Utc>,
    #[serde(with = "crate::dto::util::iso8601_offset_option", default)]
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    pub tokens_in: i32,
    pub tokens_out: i32,
    pub cost_usd: f64,
    pub error_count: i32,
    pub last_model: Option<String>,
    pub effective_status: String,
}

impl From<cognee_database::SessionRowWithStatus> for SessionRowDTO {
    fn from(row: cognee_database::SessionRowWithStatus) -> Self {
        let cognee_database::SessionRowWithStatus {
            record,
            effective_status,
        } = row;
        Self {
            session_id: record.session_id,
            user_id: record.user_id,
            dataset_id: record.dataset_id,
            status: record.status,
            started_at: record.started_at,
            last_activity_at: record.last_activity_at,
            ended_at: record.ended_at,
            tokens_in: record.tokens_in,
            tokens_out: record.tokens_out,
            cost_usd: record.cost_usd,
            error_count: record.error_count,
            last_model: record.last_model,
            effective_status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_window_default_is_30d() {
        assert_eq!(RangeWindow::default(), RangeWindow::D30);
    }

    #[test]
    fn order_by_default_is_last_activity_at() {
        assert_eq!(OrderBy::default(), OrderBy::LastActivityAt);
        assert_eq!(OrderBy::LastActivityAt.as_column(), "last_activity_at");
        assert_eq!(OrderBy::CostUsd.as_column(), "cost_usd");
    }

    #[test]
    fn list_sessions_query_defaults() {
        let q: ListSessionsQuery = serde_urlencoded::from_str("").expect("empty query");
        assert_eq!(q.range, RangeWindow::D30);
        assert_eq!(q.limit, 50);
        assert_eq!(q.offset, 0);
        assert_eq!(q.order_by, OrderBy::LastActivityAt);
        assert!(q.descending);
        assert!(q.status.is_none());
    }

    #[test]
    fn list_sessions_query_parses_all_fields() {
        let q: ListSessionsQuery = serde_urlencoded::from_str(
            "range=24h&status=running&limit=200&offset=10&order_by=cost_usd&descending=false",
        )
        .expect("parse query");
        assert_eq!(q.range, RangeWindow::H24);
        assert_eq!(q.status.as_deref(), Some("running"));
        assert_eq!(q.limit, 200);
        assert_eq!(q.offset, 10);
        assert_eq!(q.order_by, OrderBy::CostUsd);
        assert!(!q.descending);
    }

    #[test]
    fn range_window_rejects_90d() {
        // 90d is NOT a valid Python value — strict parity drops it.
        let res: Result<ListSessionsQuery, _> = serde_urlencoded::from_str("range=90d");
        assert!(res.is_err(), "90d must be rejected");
    }

    #[test]
    fn order_by_rejects_unknown_variant() {
        // Decision 9 / D-1: typed enum rejects unknown variants.
        let res: Result<ListSessionsQuery, _> = serde_urlencoded::from_str("order_by=banana");
        assert!(res.is_err(), "unknown order_by must be rejected");
    }

    #[test]
    fn session_row_dto_emits_snake_case_keys() {
        use chrono::TimeZone;
        let dto = SessionRowDTO {
            session_id: "s".into(),
            user_id: "u".into(),
            dataset_id: None,
            status: "running".into(),
            started_at: chrono::Utc
                .with_ymd_and_hms(2026, 4, 29, 0, 0, 0)
                .single()
                .expect("valid"),
            last_activity_at: chrono::Utc
                .with_ymd_and_hms(2026, 4, 29, 0, 0, 1)
                .single()
                .expect("valid"),
            ended_at: None,
            tokens_in: 1,
            tokens_out: 2,
            cost_usd: 0.5,
            error_count: 0,
            last_model: Some("gpt-4o".into()),
            effective_status: "running".into(),
        };
        let s = serde_json::to_string(&dto).expect("serialize");
        // snake_case wire keys + Decision 6 timestamp shape.
        assert!(
            s.contains("\"session_id\""),
            "expected snake_case session_id: {s}"
        );
        assert!(
            s.contains("\"last_activity_at\""),
            "expected snake_case last_activity_at: {s}"
        );
        assert!(
            s.contains("\"effective_status\""),
            "expected snake_case effective_status: {s}"
        );
        assert!(
            s.contains("+00:00"),
            "expected Decision-6 +00:00 timestamp: {s}"
        );
    }
}
