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

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;

use cognee_database::{AclDb, SessionLifecycleDb, SessionListFilters, SessionStats};

use crate::auth::AuthenticatedUser;
use crate::dto::sessions::{
    CostByModelDTO, CostByModelQuery, ListSessionsQuery, RangeWindow, SessionDetailDTO,
    SessionListResponseDTO, SessionRowDTO, SessionStatsDTO, StatsQuery,
};
use crate::error::{ApiError, ValidationDetails};
use crate::middleware::validation::ValidatedQuery;
use crate::state::AppState;

/// Build the `/api/v1/sessions` sub-router.
///
/// E-09 mounts `GET /` (list); E-10 adds `GET /stats`; E-11 adds
/// `GET /cost-by-model`; E-12 adds `GET /{session_id}` (detail).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_sessions))
        .route("/stats", get(get_stats))
        .route("/cost-by-model", get(cost_by_model))
        .route("/{session_id}", get(get_session_detail))
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

// ─── GET /api/v1/sessions/cost-by-model ───────────────────────────────────────

/// `GET /api/v1/sessions/cost-by-model` — per-model cost + token
/// breakdown for the dashboard's "Spend by model" widget.
///
/// Mirrors Python `cost_by_model` at
/// [`get_sessions_router.py:198-252`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L198-L252).
///
/// Visibility model: caller's own sessions are always counted; sessions
/// attached to a dataset the caller has `read` permission on are also
/// counted via [`AclDb::authorized_dataset_ids_with_roles`].
///
/// Response wire shape: a plain JSON array (not an envelope) of 5-field
/// snake_case rows. Python returns a plain list of dicts via
/// `jsonable_encoder` (`get_sessions_router.py:241-251`), so `to_camel`
/// does not apply (same parity carve-out as the list and stats
/// endpoints). Sorted by `SUM(cost_usd)` descending; null-model rows
/// fold into a single `"unknown"` bucket (LIB-05's repo applies the
/// fallback at `crates/database/src/ops/session_lifecycle.rs:822`).
#[utoipa::path(
    get,
    path = "/api/v1/sessions/cost-by-model",
    tag = "sessions",
    params(CostByModelQuery),
    responses(
        (status = 200, description = "per-model cost + token breakdown", body = Vec<CostByModelDTO>),
        (status = 401, description = "unauthorized"),
        (status = 500, description = "cost-by-model failed"),
    )
)]
#[tracing::instrument(
    name = "cognee.api.sessions.cost_by_model",
    skip(state),
    fields(
        cognee.session.user_id = %user.id,
        cognee.session.range = ?query.range,
    )
)]
pub async fn cost_by_model(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    ValidatedQuery(query): ValidatedQuery<CostByModelQuery>,
) -> Result<Json<Vec<CostByModelDTO>>, ApiError> {
    let components = state.components().ok_or_else(|| {
        // Treat un-wired components like the Python catch-all: 500 with
        // `{"error": "cost-by-model failed"}` (Python `get_sessions_router.py:108-110`
        // pattern reused across the read endpoints).
        tracing::error!("cost_by_model: components not configured");
        ApiError::OntologyEnvelope(
            "cost-by-model failed".to_string(),
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
        .cost_by_model(user.id, &permitted_dataset_ids, since)
        .await
    {
        Ok(rows) => {
            let dtos: Vec<CostByModelDTO> = rows.into_iter().map(CostByModelDTO::from).collect();
            Ok(Json(dtos))
        }
        Err(err) => {
            // Python parity: the underlying SeaORM/repo errors bubble up
            // through `_permitted_dataset_ids_for` / `cost_by_model` and
            // hit the same catch-all envelope used by `list_sessions` at
            // `:108-110`. Use `OntologyEnvelope` — its render is
            // `{"error": <msg>}` at the given status, matching Python's
            // `JSONResponse(status_code=500, content={...})`.
            tracing::error!(error = %err, "cost_by_model failed");
            Err(ApiError::OntologyEnvelope(
                "cost-by-model failed".to_string(),
                StatusCode::INTERNAL_SERVER_ERROR,
            ))
        }
    }
}

// ─── GET /api/v1/sessions/{session_id} ────────────────────────────────────────

/// `GET /api/v1/sessions/{session_id}` — single-session detail.
///
/// Mirrors Python `get_session_detail` at
/// [`get_sessions_router.py:254-307`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L254-L307).
///
/// Visibility model: caller's own sessions are always visible; sessions
/// attached to a dataset the caller has `read` permission on are also
/// visible (via [`AclDb::authorized_dataset_ids_with_roles`]). When the
/// row is invisible or absent the handler returns `404 {"detail": "session
/// not found"}` — the **only** v2 endpoint that intentionally emits the
/// `{detail}` envelope (Python `HTTPException(404, detail=...)` parity);
/// every other endpoint in this router uses `{error}`.
///
/// Cache-content reads are owner-aware: `qas` and `traces` are fetched
/// under the **session's owner** `user_id` (taken from the row), not the
/// authenticated caller, so a dataset-grant viewer sees the actual cache
/// content of someone else's session. Cache failures are swallowed
/// silently — Python wraps the cache reads in `try / except: pass` so the
/// row body is returned even when the cache is unavailable. Replicate via
/// `unwrap_or_default()` on the `Result`s.
///
/// `msg_count` / `tool_calls` are the **pre-truncation** list lengths
/// (Python computes `len(qas) / len(traces)` before slicing to the
/// trailing 20). The label fallback chain is: first non-empty
/// `qas[i].question` truncated to 120 chars (chars not bytes — Python
/// `[:120]` on a `str`) → first non-empty `traces[i].origin_function` →
/// `None`.
///
/// Response wire shape: snake_case (Python returns a plain dict via
/// `jsonable_encoder`, not an `OutDTO`). 17 fields total: 13 from
/// [`SessionRowDTO`] flattened + 4 extras (`label`, `msg_count`,
/// `tool_calls`, `qas`, `traces`).
#[utoipa::path(
    get,
    path = "/api/v1/sessions/{session_id}",
    tag = "sessions",
    params(("session_id" = String, Path, description = "Session id")),
    responses(
        (status = 200, description = "session detail (record + truncated qas / traces)", body = SessionDetailDTO),
        (status = 401, description = "unauthorized"),
        (status = 404, description = "session not found (FastAPI HTTPException — `{detail}` envelope)"),
        (status = 500, description = "session detail failed"),
    )
)]
#[tracing::instrument(
    name = "cognee.api.sessions.detail",
    skip(state),
    fields(
        cognee.session.user_id = %user.id,
        cognee.session.session_id = %session_id,
    )
)]
pub async fn get_session_detail(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(session_id): Path<String>,
) -> Result<Json<SessionDetailDTO>, ApiError> {
    let components = state.components().ok_or_else(|| {
        // Python parity: 500 with `{"error": "session detail failed"}`
        // (matches the catch-all envelope used by the three sibling
        // read endpoints — `get_sessions_router.py:108-110` pattern).
        tracing::error!("get_session_detail: components not configured");
        ApiError::OntologyEnvelope(
            "session detail failed".to_string(),
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
            tracing::warn!(
                error = %err,
                "authorized_dataset_ids_with_roles failed; proceeding with empty set"
            );
            Vec::new()
        }
    };

    // Visibility-checked single-row read.
    let row = match components
        .database
        .get_session_row(&session_id, user.id, &permitted_dataset_ids, false)
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            // 404 path uses `{"detail": ...}` for FastAPI HTTPException
            // parity — the only v2 endpoint that intentionally emits the
            // `{detail}` envelope (README §1.1 wire conventions).
            return Err(ApiError::NotFound("session not found".to_string()));
        }
        Err(err) => {
            tracing::error!(error = %err, "get_session_row failed");
            return Err(ApiError::OntologyEnvelope(
                "session detail failed".to_string(),
                StatusCode::INTERNAL_SERVER_ERROR,
            ));
        }
    };

    // Owner-aware cache lookup: pull from cache under the row's user_id,
    // not the authenticated caller. Supports the dataset-grant scenario
    // where a viewer can read someone else's session content.
    let owner_user_id = row.record.user_id.clone();

    // Best-effort cache reads (Python's `try / except: pass`). When the
    // session manager isn't wired or the owner is empty, return the row
    // with empty `qas` / `traces` (mirrors Python's `is_available` and
    // owner_user_id short-circuits at `:277`).
    let (mut qas, mut traces) = if !owner_user_id.is_empty()
        && let Some(sm) = components.session_manager.as_ref()
    {
        let qas = match components.session_store.as_ref() {
            // Python `sm.get_session(formatted=False)` returns the full QA
            // list; we mirror the unbounded read by passing `usize::MAX`
            // so `msg_count` reflects the full length below.
            Some(store) => store
                .get_latest_qa_entries(&session_id, Some(&owner_user_id), usize::MAX)
                .await
                .unwrap_or_default(),
            None => Vec::new(),
        };
        // Pass `last_n=None` so we get the full trace list and can compute
        // `tool_calls` against the unbounded length — Python computes
        // `record["tool_calls"] = len(traces)` BEFORE truncating to
        // `traces[-20:]`. See `get_sessions_router.py:304-306`.
        let traces = sm
            .get_agent_trace_session(&owner_user_id, Some(&session_id), None)
            .await
            .unwrap_or_default();
        (qas, traces)
    } else {
        (Vec::new(), Vec::new())
    };

    // Pre-truncation lengths — Python parity.
    let msg_count = qas.len();
    let tool_calls = traces.len();

    // Label fallback chain: first non-empty QA question (truncated to
    // 120 chars) → first non-empty trace origin_function → None.
    let label = compute_label(&qas, &traces);

    // Tail-truncate to last 20. `Vec::split_off` keeps the trailing
    // entries (oldest of the 20 first), matching Python's `[-20:]`.
    let qas_tail = qas.split_off(qas.len().saturating_sub(20));
    let traces_tail = traces.split_off(traces.len().saturating_sub(20));

    // Serialize each entry to a JSON value so the wire shape matches
    // Python's untyped dicts. `SessionQAEntry` and `SessionTraceStep`
    // both have snake_case `Serialize` impls (see `types.rs:55-79`),
    // matching Python's persisted JSON shape byte-for-byte.
    let qas_json: Vec<serde_json::Value> = qas_tail
        .into_iter()
        .map(|entry| serde_json::to_value(&entry).unwrap_or(serde_json::Value::Null))
        .collect();
    let traces_json: Vec<serde_json::Value> = traces_tail
        .into_iter()
        .map(|step| serde_json::to_value(&step).unwrap_or(serde_json::Value::Null))
        .collect();

    Ok(Json(SessionDetailDTO {
        record: SessionRowDTO::from(row),
        label,
        msg_count,
        tool_calls,
        qas: qas_json,
        traces: traces_json,
    }))
}

/// Compute the dashboard label per Python's fallback chain at
/// [`get_sessions_router.py:292-302`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L292-L302):
///
/// 1. First non-empty `qas[i].question` truncated to 120 chars.
/// 2. Else first non-empty `traces[i].origin_function`.
/// 3. Else `None`.
///
/// The 120-char truncation is on `chars()` (Unicode scalar values), not
/// bytes — Python's `str[:120]` slices code points, so `"é" * 200`
/// truncates to 120 `é`s (240 bytes), not 120 bytes of the encoded form.
fn compute_label(
    qas: &[cognee_session::SessionQAEntry],
    traces: &[cognee_session::SessionTraceStep],
) -> Option<String> {
    for entry in qas {
        if !entry.question.is_empty() {
            return Some(entry.question.chars().take(120).collect::<String>());
        }
    }
    for step in traces {
        if !step.origin_function.is_empty() {
            return Some(step.origin_function.clone());
        }
    }
    None
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
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
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
