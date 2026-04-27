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

use cognee_search::types::{SearchError as CoreSearchError, SearchRequest};

use crate::auth::AuthenticatedUser;
use crate::dto::recall::{RecallHistoryItemDTO, RecallPayloadDTO, RecallResultDTO};
use crate::dto::search::flatten_search_response;
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

/// `POST /api/v1/recall` — semantic search (wire-level alias for `/search`).
///
/// Delegates to the same `SearchOrchestrator` as `/api/v1/search` (Python
/// parity — must NOT call the library-level `cognee_lib::api::recall::recall`,
/// which would diverge from the Python HTTP contract).
#[utoipa::path(
    post,
    path = "/api/v1/recall",
    tag = "recall",
    request_body = RecallPayloadDTO,
    responses(
        (status = 200, description = "recall results", body = Vec<RecallResultDTO>),
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
) -> Result<Json<Vec<RecallResultDTO>>, ApiError> {
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

    let request = SearchRequest {
        query_text: payload.query,
        search_type: payload.search_type.into(),
        top_k: payload
            .top_k
            .and_then(|n| if n > 0 { Some(n as usize) } else { None }),
        datasets: payload.datasets,
        dataset_ids: payload.dataset_ids,
        system_prompt: payload.system_prompt,
        system_prompt_path: None,
        only_context: Some(payload.only_context),
        use_combined_context: None,
        session_id: None,
        node_type: None,
        node_name: payload.node_name,
        node_name_filter_operator: None,
        wide_search_top_k: None,
        triplet_distance_penalty: None,
        save_interaction: None,
        user_id: Some(user.id),
        verbose: Some(payload.verbose),
        feedback_influence: None,
        retriever_specific_config: None,
        response_schema: None,
        custom_search_type: None,
        auto_feedback_detection: None,
        neighborhood_depth: None,
        neighborhood_seed_top_k: None,
    };

    match orchestrator.search(&request).await {
        Ok(response) => Ok(Json(flatten_search_response(response))),
        Err(err) => Err(map_recall_error(err)),
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
