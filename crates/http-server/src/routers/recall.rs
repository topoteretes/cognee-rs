//! `/api/v1/recall` — wire-level alias for `/api/v1/search`.
//!
//! Per Python parity ([`get_recall_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py)),
//! this router shares the underlying `SearchOrchestrator` with `/api/v1/search`
//! but maps errors through three distinct envelopes:
//! - `200 []` for permission denied (silent — NOT 403).
//! - `422 {error, hint}` for prerequisite errors.
//! - `409 {error}` for any other unhandled exception.
//!
//! See `docs/http-server/routers/recall.md` for the full spec.

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};

use cognee_search::recall_scope::{
    RecallItem, RecallScope, RecallSource, fetch_graph_context, run_graph, search_session,
    search_trace,
};
use cognee_search::types::SearchError as CoreSearchError;

use crate::auth::AuthenticatedUser;
use crate::dto::recall::{RecallHistoryItemDTO, RecallPayloadDTO};
use crate::error::{ApiError, RecallErrorBody};
use crate::middleware::validation::Json as ValidatedJson;
use crate::state::AppState;

/// Build the `/api/v1/recall` sub-router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(get_recall_history))
        .route("/", post(post_recall))
}

// ─── GET /api/v1/recall ───────────────────────────────────────────────────────

/// `GET /api/v1/recall` — list the caller's recall/search history.
///
/// Returns the **same** rows as `GET /api/v1/search`. On DB error, returns the
/// `{error}` single-field envelope (NOT `{error, detail}` — Python drops the
/// detail to avoid leaking DB internals).
#[utoipa::path(
    get,
    path = "/api/v1/recall",
    tag = "recall",
    responses(
        (status = 200, description = "recall history", body = Vec<RecallHistoryItemDTO>),
        (status = 401, description = "unauthorized"),
        (status = 500, description = "internal error", body = RecallErrorBody),
    )
)]
#[tracing::instrument(name = "cognee.api.recall.history", skip(state), fields(cognee.search.user_id = %user.id))]
pub async fn get_recall_history(
    user: AuthenticatedUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<RecallHistoryItemDTO>>, ApiError> {
    let Some(orchestrator) = state
        .components()
        .and_then(|c| c.search_orchestrator.clone())
    else {
        return Err(ApiError::RecallError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: RecallErrorBody::JustError {
                error: "An error occurred while fetching recall history.".to_string(),
            },
        });
    };

    match orchestrator.get_history(Some(user.id), None).await {
        Ok(entries) => {
            let items = entries
                .into_iter()
                .map(RecallHistoryItemDTO::from_entry)
                .collect();
            Ok(Json(items))
        }
        Err(err) => {
            tracing::error!(error = %err, "failed to fetch recall history");
            Err(ApiError::RecallError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                body: RecallErrorBody::JustError {
                    error: "An error occurred while fetching recall history.".to_string(),
                },
            })
        }
    }
}

// ─── POST /api/v1/recall ──────────────────────────────────────────────────────

/// `POST /api/v1/recall` — multi-source semantic recall.
///
/// Calls `cognee_search::recall_scope::*` helpers directly because the
/// http-server -> lib cycle constraint (`Cargo.toml:35-37`) forbids
/// importing `cognee_lib::api::recall::recall`. The fan-out logic mirrors
/// Python `recall.py:373-531` byte-for-byte (per Decisions 17 + 18 — the
/// LIB-08 lift makes the four `pub` helpers reachable without a cycle).
#[utoipa::path(
    post,
    path = "/api/v1/recall",
    tag = "recall",
    request_body = RecallPayloadDTO,
    responses(
        (status = 200, description = "recall results — flat list of dicts each tagged with `_source`", body = Vec<serde_json::Value>),
        (status = 400, description = "validation error", body = serde_json::Value),
        (status = 401, description = "unauthorized"),
        (status = 409, description = "catch-all", body = RecallErrorBody),
        (status = 422, description = "prerequisites not met", body = RecallErrorBody),
    )
)]
#[tracing::instrument(
    name = "cognee.api.recall",
    skip(state, payload),
    fields(
        cognee.search.user_id = %user.id,
        cognee.search.type = ?payload.search_type,
        cognee.search.top_k = ?payload.top_k,
    )
)]
pub async fn post_recall(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    ValidatedJson(payload): ValidatedJson<RecallPayloadDTO>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    let Some(orchestrator) = state
        .components()
        .and_then(|c| c.search_orchestrator.clone())
    else {
        return Err(ApiError::RecallError {
            status: StatusCode::CONFLICT,
            body: RecallErrorBody::JustError {
                error: "An error occurred during recall.".to_string(),
            },
        });
    };

    // Component handles for the optional session-backed sources. Both stay
    // `None` when the embedder did not wire them — the helpers gracefully
    // return `Ok(vec![])` in that case (Python `is_available` short-circuit
    // at `recall.py:170-171`).
    let session_store = state
        .components()
        .and_then(|c| c.session_store.as_ref().cloned());
    let session_manager = state
        .components()
        .and_then(|c| c.session_manager.as_ref().cloned());

    // -- Resolve scope to a concrete source list (mirrors
    //    `cognee_lib::api::recall::recall()` at `crates/lib/src/api/recall.rs:78-100`,
    //    Python `recall.py:373-386`). --
    let normalized: Vec<RecallScope> = match payload.scope {
        None => vec![RecallScope::Auto],
        Some(v) if v.is_empty() => vec![RecallScope::Auto],
        Some(v) => v,
    };

    let session_id_owned = payload.session_id.clone();
    let session_id_opt: Option<&str> = session_id_owned.as_deref();
    let user_id_string = user.id.to_string();
    let user_id_opt: Option<&str> = Some(user_id_string.as_str());
    let top_k: usize = payload
        .top_k
        .and_then(|n| if n > 0 { Some(n as usize) } else { None })
        .unwrap_or(10);
    let datasets: Option<Vec<String>> = payload.datasets.clone();
    let query_type: Option<cognee_search::SearchType> = Some(payload.search_type.into());

    let auto_mode = normalized.as_slice() == [RecallScope::Auto];
    let (sources, auto_fallthrough): (Vec<RecallScope>, bool) = if auto_mode {
        match (session_id_opt, datasets.as_ref(), query_type) {
            (Some(_), None, None) => (vec![RecallScope::Session, RecallScope::Graph], true),
            (Some(_), _, _) => (vec![RecallScope::Session, RecallScope::Graph], false),
            (None, _, _) => (vec![RecallScope::Graph], false),
        }
    } else {
        (normalized, false)
    };

    // -- Iterate sources in resolved order (mirrors `recall.rs:151-197`,
    //    Python `recall.py:503-513`). --
    let mut merged: Vec<RecallItem> = Vec::new();
    let span = tracing::Span::current();
    for src in &sources {
        // Auto-mode short-circuit: a session hit skips the graph runner.
        if auto_fallthrough && *src == RecallScope::Graph && !merged.is_empty() {
            break;
        }

        let part: Vec<RecallItem> = match src {
            RecallScope::Auto => continue, // sentinel — already resolved
            RecallScope::Session => search_session(
                &payload.query,
                session_id_opt,
                user_id_opt,
                top_k,
                session_store.as_deref(),
            )
            .await
            .map_err(map_recall_error)?,
            RecallScope::Trace => search_trace(
                &payload.query,
                session_id_opt,
                user_id_opt,
                top_k,
                session_manager.as_deref(),
            )
            .await
            .map_err(map_recall_error)?,
            RecallScope::GraphContext => {
                fetch_graph_context(session_id_opt, user_id_opt, session_manager.as_deref())
                    .await
                    .map_err(map_recall_error)?
            }
            RecallScope::Graph => {
                let (items, _used_type, _was_auto, _response) = run_graph(
                    &payload.query,
                    query_type,
                    datasets.clone(),
                    top_k,
                    /* auto_route = */ false,
                    session_id_opt,
                    orchestrator.as_ref(),
                    &span,
                    None,
                )
                .await
                .map_err(map_recall_error)?;
                items
            }
        };
        merged.extend(part);
    }

    // -- Map to Python's wire shape: flat list of dicts, each carrying its
    //    own `_source` field (Python `recall.py:191-208` for session,
    //    `recall.py:252-278` for trace, `recall.py:289-315` for
    //    graph_context, `recall.py:455-498` for graph). --
    let out: Vec<serde_json::Value> = merged.into_iter().map(item_to_wire).collect();
    Ok(Json(out))
}

/// Convert a [`RecallItem`] into its Python-parity wire JSON.
///
/// The Python implementation injects `_source` into each per-source dict
/// (Python `recall.py:208/278/315/495-498`). Rust mirrors that:
/// - `Session` / `Trace` content is already a JSON object — inject
///   `_source` next to its existing keys.
/// - `GraphContext` content is the raw snapshot string — wrap it as
///   `{"_source": "graph_context", "content": <string>}` (Python
///   `recall.py:314` returns exactly that shape).
/// - `Graph` content is whatever `run_graph` produced (object, string,
///   array). For object-shaped content inject `_source`; for non-object
///   content wrap as `{"text": <s>, "_source": "graph"}` (mirrors Python's
///   string-fallback behavior — Python emits the bare string, but the
///   Rust wire wraps it for consistent `_source` tagging across all rows).
fn item_to_wire(item: RecallItem) -> serde_json::Value {
    let source_str = item.source.as_str();
    match item.source {
        RecallSource::Session | RecallSource::Trace => {
            // search_session / search_trace already produce a JSON object —
            // mutate to inject "_source".
            inject_source_into_object(item.content, source_str)
        }
        RecallSource::GraphContext => {
            // fetch_graph_context returns a JSON string snapshot; wrap it
            // under "content" per Python `recall.py:314`.
            serde_json::json!({
                "_source": source_str,
                "content": item.content,
            })
        }
        RecallSource::Graph => match item.content {
            v @ serde_json::Value::Object(_) => inject_source_into_object(v, source_str),
            other => serde_json::json!({
                "text": other,
                "_source": source_str,
            }),
        },
    }
}

/// Mutate a JSON object to add `"_source": <src>`. If the input is not an
/// object the helper falls back to wrapping under a `value` key so the
/// `_source` field is always present (defensive — `search_session` and
/// `search_trace` always produce an object today).
fn inject_source_into_object(value: serde_json::Value, src: &str) -> serde_json::Value {
    match value {
        serde_json::Value::Object(mut map) => {
            map.insert(
                "_source".to_string(),
                serde_json::Value::String(src.to_string()),
            );
            serde_json::Value::Object(map)
        }
        other => serde_json::json!({
            "value": other,
            "_source": src,
        }),
    }
}

/// Map a `SearchError` from the orchestrator into one of recall's three
/// Python-shaped error envelopes.
///
/// See `docs/http-server/routers/recall.md` §2.2.
///
/// **Parity gap (silent `200 []` for permission denied)**: Python's HTTP
/// recall returns `200 []` when `PermissionDeniedError` is raised by
/// `get_authorized_existing_datasets`. The Rust `SearchOrchestrator` has no
/// `PermissionDenied` variant on `SearchError` and does not perform per-
/// dataset ACL filtering before dispatch — so the silent-empty path is not
/// reachable through this handler today. When the orchestrator gains a
/// `PermissionDenied` variant (or the handler grows an explicit ACL
/// pre-check) this map MUST recognise it and return `Ok(Json(vec![]))`
/// rather than 409. Tracked at `docs/http-server/routers/recall.md` §2.2.
fn map_recall_error(err: CoreSearchError) -> ApiError {
    match err {
        // 422: prerequisite errors carry the `{error, hint}` envelope.
        // Python emits this for DatabaseNotCreatedError, UserNotFoundError,
        // CogneeValidationError, DatasetNotFoundError.
        CoreSearchError::DatasetNotFound(_)
        | CoreSearchError::InvalidInput(_)
        | CoreSearchError::NotFound(_) => ApiError::RecallError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            body: RecallErrorBody::WithHint {
                error: "Recall prerequisites not met".to_string(),
                hint:
                    "Run `await cognee.remember(...)` or `await cognee.add(...)` then `await cognee.cognify()` before recalling."
                        .to_string(),
            },
        },
        // 409: catch-all single-field envelope. Log the underlying error.
        other => {
            tracing::error!(error = %other, "recall failed with unhandled error");
            ApiError::RecallError {
                status: StatusCode::CONFLICT,
                body: RecallErrorBody::JustError {
                    error: "An error occurred during recall.".to_string(),
                },
            }
        }
    }
}
