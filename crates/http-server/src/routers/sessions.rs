//! `/api/v1/sessions/*` — session-management dashboard endpoints.
//!
//! E-09 implements **`GET /api/v1/sessions`** (paginated list). Subsequent
//! tasks add the rest of the family:
//!
//! - **E-10** → `GET /sessions/stats` — aggregate counters.
//! - **E-11** → `GET /sessions/cost-by-model` — per-model attribution.
//! - **E-12** → `GET /sessions/{session_id}` — single-session detail.
//!
//! Python source-of-truth: [`get_sessions_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py).
//!
//! Wire-shape carve-out (Python parity): the response envelope and per-row
//! body are **snake_case** because Python emits a plain dict via
//! `jsonable_encoder` — `to_camel` does not apply. See
//! [`crate::dto::sessions`](../../dto/sessions/index.html) for the full
//! rationale.

use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;

use cognee_database::{AclDb, SessionLifecycleDb, SessionListFilters, SessionStats};

use crate::auth::AuthenticatedUser;
use crate::dto::sessions::{
    ListSessionsQuery, RangeWindow, SessionListResponseDTO, SessionRowDTO, SessionStatsDTO,
    StatsQuery,
};
use crate::error::{ApiError, ValidationDetails};
use crate::middleware::validation::ValidatedQuery;
use crate::state::AppState;

/// Build the `/api/v1/sessions` sub-router.
///
/// E-09 mounts `GET /` (list); E-10 adds `GET /stats`. E-11/E-12 will
/// add the remaining handlers below — keep the doc-comment in sync as
/// routes land:
///   * `.route("/cost-by-model", get(cost_by_model))` (E-11)
///   * `.route("/{session_id}", get(get_session_detail))` (E-12)
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_sessions))
        .route("/stats", get(get_stats))
}

// ─── GET /api/v1/sessions ─────────────────────────────────────────────────────

/// `GET /api/v1/sessions` — paginated session list.
///
/// Mirrors Python `list_sessions` at
/// [`get_sessions_router.py:64-110`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L64-L110).
///
/// Visibility model: caller's own sessions are always included; sessions
/// attached to a dataset the caller has `read` permission on are also
/// included via [`AclDb::authorized_dataset_ids_with_roles`].
///
/// Decision 9 / divergence D-1: unknown `order_by` values return `400`
/// with the Python validation envelope (typed enum at the DTO layer).
/// Python silently falls back to `last_activity_at` — see
/// [`README.md §1.2`](../../../../docs/http-api-v2/README.md#12-v2-acknowledged-divergences-changes-to-steady-state-wire-output).
#[utoipa::path(
    get,
    path = "/api/v1/sessions",
    tag = "sessions",
    params(ListSessionsQuery),
    responses(
        (status = 200, description = "paginated session list", body = SessionListResponseDTO),
        (status = 400, description = "validation error (e.g. unknown order_by)", body = serde_json::Value),
        (status = 401, description = "unauthorized"),
        (status = 500, description = "list failed"),
    )
)]
#[tracing::instrument(
    name = "cognee.api.sessions.list",
    skip(state),
    fields(
        cognee.session.user_id = %user.id,
        cognee.session.range = ?query.range,
        cognee.session.limit = query.limit,
        cognee.session.offset = query.offset,
    )
)]
pub async fn list_sessions(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    ValidatedQuery(query): ValidatedQuery<ListSessionsQuery>,
) -> Result<Json<SessionListResponseDTO>, ApiError> {
    // Validate `limit ∈ 1..=500`. Python's FastAPI does this via
    // `Query(ge=1, le=500)`; our DTO accepts any `u32` so we enforce it
    // here as the first line of the handler.
    if !(1..=500).contains(&query.limit) {
        return Err(ApiError::Validation(ValidationDetails {
            detail: json!([{
                "loc": ["query", "limit"],
                "msg": format!(
                    "ensure this value is in 1..=500 (got {})",
                    query.limit
                ),
                "type": "value_error",
            }]),
            body: None,
        }));
    }

    let components = state.components().ok_or_else(|| {
        // Treat un-wired components like the Python catch-all: 500 with
        // `{"error": "list failed"}` (Python `get_sessions_router.py:108-110`).
        tracing::error!("list_sessions: components not configured");
        ApiError::OntologyEnvelope("list failed".to_string(), StatusCode::INTERNAL_SERVER_ERROR)
    })?;

    // Resolve permitted dataset ids — Python's
    // `_permitted_dataset_ids_for` swallows every exception and returns
    // empty (`get_sessions_router.py:55-58`).
    let permitted_dataset_ids = match components
        .database
        .authorized_dataset_ids_with_roles(user.id, "read")
        .await
    {
        Ok(ids) => ids,
        Err(err) => {
            tracing::warn!(error = %err, "authorized_dataset_ids_with_roles failed; proceeding with empty set");
            Vec::new()
        }
    };

    let since = range_since(query.range);

    let filters = SessionListFilters {
        user_id: user.id,
        permitted_dataset_ids,
        since,
        status_filter: query.status.clone(),
        limit: query.limit,
        offset: query.offset,
        order_by: query.order_by.as_column().to_string(),
        descending: query.descending,
    };

    match components.database.list_session_rows(filters).await {
        Ok(page) => {
            let has_more = page.has_more();
            let response = SessionListResponseDTO {
                sessions: page.sessions.into_iter().map(SessionRowDTO::from).collect(),
                total: page.total,
                limit: page.limit,
                offset: page.offset,
                has_more,
            };
            Ok(Json(response))
        }
        Err(err) => {
            // Python parity: catch-all returns 500 `{"error": "list failed"}`
            // (`get_sessions_router.py:108-110`). Use `OntologyEnvelope` —
            // its render is `{"error": <msg>}` at the given status, which
            // matches Python's `JSONResponse(status_code=500, content={...})`.
            // `ApiError::Internal` would render as `{"detail": ...}`, which
            // does not match.
            tracing::error!(error = %err, "list_sessions failed");
            Err(ApiError::OntologyEnvelope(
                "list failed".to_string(),
                StatusCode::INTERNAL_SERVER_ERROR,
            ))
        }
    }
}

// ─── GET /api/v1/sessions/stats ───────────────────────────────────────────────

/// `GET /api/v1/sessions/stats` — aggregate counters for the dashboard
/// stat cards + status bar.
///
/// Mirrors Python `get_stats` at
/// [`get_sessions_router.py:112-196`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L112-L196).
///
/// Visibility model: caller's own sessions are always counted; sessions
/// attached to a dataset the caller has `read` permission on are also
/// counted via [`AclDb::authorized_dataset_ids_with_roles`].
///
/// Response wire shape: snake_case (Python returns a plain dict via
/// `jsonable_encoder`, not an `OutDTO`). 14 fields: `range` echoes the
/// input + 13 counters from
/// [`cognee_database::SessionStats`](../../../cognee_database/struct.SessionStats.html).
#[utoipa::path(
    get,
    path = "/api/v1/sessions/stats",
    tag = "sessions",
    params(StatsQuery),
    responses(
        (status = 200, description = "dashboard counters", body = SessionStatsDTO),
        (status = 401, description = "unauthorized"),
        (status = 500, description = "stats failed"),
    )
)]
#[tracing::instrument(
    name = "cognee.api.sessions.stats",
    skip(state),
    fields(
        cognee.session.user_id = %user.id,
        cognee.session.range = ?query.range,
    )
)]
pub async fn get_stats(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    ValidatedQuery(query): ValidatedQuery<StatsQuery>,
) -> Result<Json<SessionStatsDTO>, ApiError> {
    let components = state.components().ok_or_else(|| {
        // Treat un-wired components like the Python catch-all: 500 with
        // `{"error": "stats failed"}` (Python `get_sessions_router.py:108-110`
        // pattern reused across the read endpoints).
        tracing::error!("get_stats: components not configured");
        ApiError::OntologyEnvelope(
            "stats failed".to_string(),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;

    // Resolve permitted dataset ids — Python's `_permitted_dataset_ids_for`
    // swallows every exception and returns empty
    // (`get_sessions_router.py:55-58`).
    let permitted_dataset_ids = match components
        .database
        .authorized_dataset_ids_with_roles(user.id, "read")
        .await
    {
        Ok(ids) => ids,
        Err(err) => {
            tracing::warn!(error = %err, "authorized_dataset_ids_with_roles failed; proceeding with empty set");
            Vec::new()
        }
    };

    let since = range_since(query.range);

    match components
        .database
        .aggregate_stats(user.id, &permitted_dataset_ids, since)
        .await
    {
        Ok(stats) => {
            let SessionStats {
                sessions,
                total_spend_usd,
                avg_spend_per_session_usd,
                tokens_in,
                tokens_out,
                tokens_total,
                agent_time_s,
                avg_session_s,
                success_rate,
                completed,
                failed,
                abandoned,
                running,
            } = stats;
            Ok(Json(SessionStatsDTO {
                range: query.range.as_wire_str().to_string(),
                sessions,
                total_spend_usd,
                avg_spend_per_session_usd,
                tokens_in,
                tokens_out,
                tokens_total,
                agent_time_s,
                avg_session_s,
                success_rate,
                completed,
                failed,
                abandoned,
                running,
            }))
        }
        Err(err) => {
            // Python parity: the underlying SeaORM/repo errors bubble up
            // through `_permitted_dataset_ids_for` / `aggregate_stats` and
            // hit the same catch-all envelope used by `list_sessions` at
            // `:108-110`. Use `OntologyEnvelope` — its render is
            // `{"error": <msg>}` at the given status, matching Python's
            // `JSONResponse(status_code=500, content={...})`.
            tracing::error!(error = %err, "get_stats failed");
            Err(ApiError::OntologyEnvelope(
                "stats failed".to_string(),
                StatusCode::INTERNAL_SERVER_ERROR,
            ))
        }
    }
}

/// Translate [`RangeWindow`] to an inclusive `last_activity_at >= since`
/// lower bound. Mirrors Python `_range_since` at
/// [`get_sessions_router.py:39-47`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L39-L47).
fn range_since(range: RangeWindow) -> Option<DateTime<Utc>> {
    let now = Utc::now();
    match range {
        RangeWindow::H24 => Some(now - Duration::hours(24)),
        RangeWindow::D7 => Some(now - Duration::days(7)),
        RangeWindow::D30 => Some(now - Duration::days(30)),
        RangeWindow::All => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_since_all_is_none() {
        assert!(range_since(RangeWindow::All).is_none());
    }

    #[test]
    fn range_since_24h_is_recent() {
        let s = range_since(RangeWindow::H24).expect("Some");
        let now = Utc::now();
        let delta = (now - s).num_seconds();
        assert!((23 * 3600..=25 * 3600).contains(&delta), "delta={delta}");
    }

    #[test]
    fn range_since_30d_is_default() {
        let s = range_since(RangeWindow::D30).expect("Some");
        let now = Utc::now();
        let days = (now - s).num_days();
        assert!((29..=31).contains(&days), "days={days}");
    }
}
